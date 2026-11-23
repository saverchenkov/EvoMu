//! `pve_exploration_sites` — SDE-backed PvE Cosmic Exploration Sites, DED Complexes & Wormhole Sleeper Combat.
// parity: feat:pve.data-sites, feat:pve.relic-sites, feat:pve.loot-spew, feat:pve.ded-complexes, feat:pve.wormhole-sites

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use eve_db::lock_rank::AnyRwLock;
use glam::DVec3;
use eve_db::{InventoryFlag, InventoryItemRecord, SolarSystemId, TypeId, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{InvType, SdeRegistry};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{with_mut_combat_runtime_with_locks, CombatRuntimeState, ExtendedNpcEntity};
use crate::services::pve_exploration::{
    ctx_pve_key, db_pve_key, drop_site_loot_into_space, drop_site_loot_into_space_core,
    expire_and_prune_site_in_reg, finalize_expired_site_prune_core, find_site_by_canonical_id,
    find_site_by_canonical_id_mut, find_site_by_canonical_key_any_state_in_reg,
    is_site_expired_in_reg,
    now_sec, ExplorationSiteKeyRef,
    read_pve_exploration_reg, reject_expired_site_prune, try_with_pve_exploration_reg_snap,
    with_pve_exploration_reg, with_pve_exploration_reg_db, with_pve_exploration_reg_snap_db,
    EscalatingPathEntry, ExpiredSitePruneResult, PveExplorationRegistry, PveSiteInstance,
    SpewedLootCan, PVE_EXPLORATION_NOTE_ID,
};
use crate::services::pve_exploration_seeding::{
    build_type_combat_state_from_sde, build_wave_npc_balls_in_snap, ensure_system_pve_sites_seeded,
    pirate_faction_for_system, PirateFaction,
};
use crate::system_actor::SystemRegistry;

/// SDE group IDs used by exploration container loot tables.
pub const DATACORE_GROUP_ID: u32 = 333;
pub const DECRYPTOR_GROUP_IDS: [u32; 9] = [728, 729, 730, 731, 732, 733, 734, 735, 1304];
pub const SALVAGED_MATERIALS_GROUP_ID: u32 = 754;
pub const ANCIENT_SALVAGE_GROUP_ID: u32 = 966;
/// SDE group "Sleeper Components" (Blue Loot).
pub const SLEEPER_COMPONENTS_GROUP_ID: u32 = 880;
/// SDE type ID range of the 23 "Overseer's Personal Effects" tiers.
pub const OVERSEER_EFFECTS_TYPE_RANGE: std::ops::RangeInclusive<u32> = 19_400..=19_422;

/// Name keywords used to weight SDE loot entries towards a pirate faction's home-region flavour.
/// DERIVED: hand-picked name-keyword heuristic (racial/tech flavour of each pirate faction's home regions);
/// the SDE has no faction->datacore/salvage loot table, so this only orders SDE group candidates.
fn faction_loot_keywords(faction: PirateFaction) -> &'static [&'static str] {
    match faction {
        PirateFaction::Guristas => &["Caldari", "Graviton", "Hydromagnetic", "Logic", "Electronic"],
        PirateFaction::BloodRaiders => &["Amarr", "Laser", "Nuclear", "Armor", "Circuit"],
        PirateFaction::Sansha => &["Amarr", "Quantum", "Laser", "Logic", "Armor"],
        PirateFaction::Serpentis => &["Gallente", "Plasma", "Mechanical", "Alloyed", "Tritanium"],
        PirateFaction::AngelCartel => &["Minmatar", "Rocket", "Nuclear", "Armor", "Nanoribbons"],
    }
}

/// Published SDE types matching `pred`, ordered with faction-flavoured entries first, then by type ID.
fn sde_loot_candidates<'a>(
    sde: &'a SdeRegistry,
    keywords: &[&str],
    pred: impl Fn(&InvType) -> bool,
) -> Vec<&'a InvType> {
    let mut out: Vec<&InvType> = sde.types.values().filter(|t| t.published && pred(t)).collect();
    out.sort_by_key(|t| (!keywords.iter().any(|k| t.name.contains(k)), t.type_id));
    out
}

/// Generates SDE-derived loot for a Data or Relic container (`feat:pve.data-sites`, `feat:pve.relic-sites`).
///
/// Candidates are queried from `sde.types` by `group_id` (Datacores 333, Decryptors 728..=735 & 1304,
/// Salvaged Materials 754, Ancient Salvage 966). Entries whose names match the system's pirate faction
/// (`pirate_faction_for_system`) are picked first; true security scales quantity and count, and gates
/// Ancient Salvage / extra decryptors to null-sec and J-space.
pub fn generate_exploration_container_loot(
    sde: &SdeRegistry,
    solar_system_id: SolarSystemId,
    is_relic: bool,
) -> Vec<(TypeId, i64, String)> {
    let true_sec = sde.solar_systems.get(&solar_system_id).map(|s| s.security_status).unwrap_or(0.5);
    let is_jspace = (31_000_000..32_000_000).contains(&solar_system_id);
    let deep_space = is_jspace || true_sec <= 0.0;
    let (sec_mult, extra): (i64, usize) = if deep_space {
        (4, 2)
    } else if true_sec < 0.5 {
        (2, 1)
    } else {
        (1, 0)
    };
    let keywords = faction_loot_keywords(pirate_faction_for_system(sde, solar_system_id));

    let mut items = Vec::new();
    let mut push = |cands: Vec<&InvType>, count: usize, qty: &dyn Fn(usize) -> i64| {
        for (idx, t) in cands.into_iter().take(count).enumerate() {
            items.push((t.type_id, qty(idx) * sec_mult, t.name.clone()));
        }
    };
    if is_relic {
        let salvage = sde_loot_candidates(sde, keywords, |t| t.group_id == SALVAGED_MATERIALS_GROUP_ID);
        push(salvage, 3 + extra, &|idx| [4, 3, 3, 2, 1].get(idx).copied().unwrap_or(1));
        if deep_space {
            let ancient = sde_loot_candidates(sde, keywords, |t| t.group_id == ANCIENT_SALVAGE_GROUP_ID);
            push(ancient, 1 + extra / 2, &|_| 2);
        }
    } else {
        let datacores = sde_loot_candidates(sde, keywords, |t| t.group_id == DATACORE_GROUP_ID);
        push(datacores, 2 + extra, &|idx| if idx < 2 { 3 } else { 2 });
        let decryptors = sde_loot_candidates(sde, keywords, |t| DECRYPTOR_GROUP_IDS.contains(&t.group_id));
        push(decryptors, 1 + extra, &|_| 1);
    }
    items
}

/// Materializes hacked-container loot spew as real in-space cargo containers in `WorldRepository`
/// (`feat:pve.loot-spew`). Each spewed can holds exactly one loot stack; the parent container receives
/// nothing, so loot exists only once. Cans are also spawned as balls in the system ballpark.
pub async fn materialize_spewed_cans_core(
    db: &Arc<WorldRepository>,
    systems: &Arc<SystemRegistry>,
    owner_char_id: i64,
    cans: &[SpewedLootCan],
) {
    if cans.is_empty() {
        return;
    }
    let can_type_id: TypeId = 3293; // Cargo Container
    let now_unix = db.clock().now_unix_secs();
    db.with_mut_snapshot(|snap| {
        for can in cans {
            crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(snap, can.can_item_id, now_unix, false);
            snap.items.insert(
                can.can_item_id,
                InventoryItemRecord {
                    item_id: can.can_item_id,
                    type_id: can_type_id,
                    owner_id: owner_char_id,
                    location_id: can.solar_system_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(can.can_item_id, can.position);
            snap.next_entity_id += 1;
            let loot_id = snap.next_entity_id;
            snap.items.insert(
                loot_id,
                InventoryItemRecord {
                    item_id: loot_id,
                    type_id: can.loot_type_id,
                    owner_id: owner_char_id,
                    location_id: can.can_item_id,
                    flag: InventoryFlag::Cargo,
                    quantity: can.loot_quantity as i32,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
    });
    for can in cans {
        let _ = systems
            .get_or_spawn(can.solar_system_id)
            .spawn_ball(Ball {
                id: can.can_item_id,
                type_id: can_type_id,
                position: DVec3::from_array(can.position),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 15.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }
}

/// Canonical DED acceleration-gate hull/mass restrictions (`feat:pve.ded-complexes`):
/// 1/10–3/10 Frigates & Destroyers, 4/10–6/10 up to Cruisers & Battlecruisers, 7/10–10/10 up to Battleships.
pub fn ded_ship_restriction_for_rating(ded_rating: u8) -> (f64, &'static str, &'static [u32]) {
    match ded_rating {
        1..=3 => (
            10_000_000.0,
            "Frigates and Destroyers only (DED 1/10 - 3/10)",
            &[25, 324, 420, 541, 830, 831, 834, 1283, 1305, 1527, 1534],
        ),
        4..=6 => (
            75_000_000.0,
            "Cruisers and Battlecruisers and below (DED 4/10 - 6/10)",
            &[25, 26, 324, 358, 419, 420, 540, 541, 830, 831, 832, 833, 834, 894, 906, 963, 1201, 1283, 1305, 1527, 1534, 1972],
        ),
        _ => (
            250_000_000.0,
            "Battleships and below (DED 7/10 - 10/10, No Capitals)",
            &[25, 26, 27, 324, 358, 419, 420, 540, 541, 830, 831, 832, 833, 834, 894, 898, 900, 906, 963, 1201, 1283, 1305, 1527, 1534, 1972],
        ),
    }
}

/// Validates a ship hull against `ded_ship_restriction_for_rating` (`feat:pve.ded-complexes`).
pub fn check_ded_gate_ship_allowed(
    sde: &SdeRegistry,
    ded_rating: u8,
    ship_type_id: TypeId,
    ship_mass: f64,
) -> Result<(), BridgeRpcError> {
    let (max_mass, label, allowed_groups) = ded_ship_restriction_for_rating(ded_rating);
    let Some(ship_type) = sde.types.get(&ship_type_id) else {
        return Err(BridgeRpcError {
            code: "ShipSizeRestricted".into(),
            message: format!("Acceleration gate cannot classify hull type {ship_type_id}: {label}"),
            client_error: None,
        });
    };
    let grp = ship_type.group_id;
    if !allowed_groups.contains(&grp) || ship_mass > max_mass {
        return Err(BridgeRpcError {
            code: "ShipSizeRestricted".into(),
            message: format!("Acceleration gate rejects ship hull {} (group {grp}, mass {ship_mass:.0} kg): {label}", ship_type.name),
            client_error: None,
        });
    }
    Ok(())
}

/// Selects the "Overseer's Personal Effects" tier for a DED rating from SDE types 19400..=19422,
/// ordered by the tier number in the SDE type name (1st..23rd) (`feat:pve.ded-complexes`).
pub fn overseer_effects_for_rating(sde: &SdeRegistry, ded_rating: u8) -> Option<(TypeId, String)> {
    let tier_of = |t: &InvType| -> u32 {
        t.name.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0)
    };
    let mut ope: Vec<&InvType> = sde.types.values().filter(|t| OVERSEER_EFFECTS_TYPE_RANGE.contains(&t.type_id)).collect();
    if ope.is_empty() {
        return None;
    }
    ope.sort_by_key(|t| (tier_of(t), t.type_id));
    let idx = ((ded_rating.clamp(1, 10) as usize - 1) * (ope.len() - 1)) / 9;
    Some((ope[idx].type_id, ope[idx].name.clone()))
}

/// Spawns the DED Overseer wreck in space when the final-room overseer NPC is destroyed
/// (`feat:pve.ded-complexes`). Contains SDE Overseer's Personal Effects and a deadspace module.
pub async fn spawn_ded_overseer_wreck_core(
    db: &Arc<WorldRepository>,
    systems: &Arc<SystemRegistry>,
    sde: &Arc<SdeRegistry>,
    solar_system_id: SolarSystemId,
    killer_char_id: i64,
    pos: DVec3,
    ded_rating: u8,
) -> (i64, Vec<(TypeId, i64, String)>) {
    let mut loot_items = Vec::new();
    if let Some((ope_tid, ope_name)) = overseer_effects_for_rating(sde, ded_rating) {
        loot_items.push((ope_tid, 1_i64, ope_name));
    }
    // DERIVED: representative Gist(ii/um) deadspace afterburner per DED size band (C-Type small / A-Type
    // medium / A-Type large); SDE has no DED overseer drop table, type IDs verified to exist via `sde.types`.
    let deadspace_mod_tid: TypeId = match ded_rating {
        1..=3 => 18658, // Gistii C-Type 1MN Afterburner
        4..=6 => 18672, // Gistum A-Type 10MN Afterburner
        _ => 18674,     // Gist A-Type 100MN Afterburner
    };
    if let Some(t) = sde.types.get(&deadspace_mod_tid) {
        loot_items.push((deadspace_mod_tid, 1_i64, t.name.clone()));
    }
    let wreck_type_id: TypeId = if ded_rating <= 3 { 26473 } else { 26561 }; // Cruiser / Battleship Wreck

    let now_unix = db.clock().now_unix_secs();
    let wreck_id = db.with_mut_snapshot(|snap| {
        let wid = snap.alloc_entity_id();
        crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(snap, wid, now_unix, true);
        snap.items.insert(
            wid,
            InventoryItemRecord {
                item_id: wid,
                type_id: wreck_type_id,
                owner_id: killer_char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(wid, [pos.x, pos.y, pos.z]);
        for (tid, qty, _) in &loot_items {
            let item_id = snap.alloc_entity_id();
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id: *tid,
                    owner_id: killer_char_id,
                    location_id: wid,
                    flag: InventoryFlag::Cargo,
                    quantity: *qty as i32,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
        wid
    });
    let _ = systems
        .get_or_spawn(solar_system_id)
        .spawn_ball(Ball {
            id: wreck_id,
            type_id: wreck_type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 50_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    (wreck_id, loot_items)
}

/// DERIVED: Sleeper class/wave composition table (Emergent C1-C2, Awakened C3-C4, Sleepless C5-C6) curated from
/// public site compositions; the SDE has no dungeon spawn tables, so wave rosters and triggers are hand-mapped.
/// Returns Sleeper NPC type IDs for a wormhole class and wave as `(wave_trigger_type_id, other_type_ids)`
/// (`feat:pve.wormhole-sites`).
pub fn sleeper_npc_types_for_class_and_wave(wh_class: u8, wave: u8) -> (TypeId, Vec<TypeId>) {
    match wh_class {
        1 => (30215, if wave == 1 { vec![30212, 30213] } else { vec![30216, 30217] }),
        2 => (30206, if wave == 1 { vec![30212, 30215] } else { vec![30203, 30204] }),
        3 => (30208, if wave == 1 { vec![30203, 30206] } else { vec![30203, 30207] }),
        4 => (30192, if wave == 1 { vec![30203, 30206] } else { vec![30208, 30193] }),
        _ => (30198, if wave == 1 { vec![30192, 30196] } else { vec![30192, 30197] }),
    }
}

// DERIVED: SDE group 880 "Sleeper Components" has no per-wormhole-class drop table in the static dump, so we
// order the four Sleeper Component types by ascending type ID (`30744..=30747`) and index by `(wh_class - 1) / 2`.
/// Blue Loot for a wormhole class, from SDE group 880 "Sleeper Components" ordered by type ID
/// (higher classes yield higher-value components) (`feat:pve.wormhole-sites`).
pub fn sleeper_blue_loot_for_class(sde: &SdeRegistry, wh_class: u8) -> Option<(TypeId, i64)> {
    let mut comps: Vec<TypeId> = sde
        .types
        .values()
        .filter(|t| t.group_id == SLEEPER_COMPONENTS_GROUP_ID)
        .map(|t| t.type_id)
        .collect();
    comps.sort_unstable();
    let idx = ((wh_class.clamp(1, 6) as usize - 1) / 2).min(comps.len().checked_sub(1)?);
    Some((comps[idx], (wh_class.max(1) as i64) * 2))
}

/// Spawns one wave of Sleeper NPCs (trigger NPC first) into the ballpark and registers them with the
/// combat runtime with zero CONCORD bounty (`feat:pve.wormhole-sites`). Returns the spawned ball IDs.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_sleeper_wave_core(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    sde: &Arc<SdeRegistry>,
    combat_ephemeral: &dyn AnyRwLock<CombatRuntimeState>,
    solar_system_id: SolarSystemId,
    wh_class: u8,
    wave: u8,
    center: DVec3,
) -> Vec<i64> {
    let (trigger_tid, other_tids) = sleeper_npc_types_for_class_and_wave(wh_class, wave);
    let blue_loot = sleeper_blue_loot_for_class(sde, wh_class);
    let sys_handle = systems.get_or_spawn(solar_system_id);
    let mut ball_ids = Vec::with_capacity(1 + other_tids.len());
    let mut entities = Vec::with_capacity(1 + other_tids.len());
    for (idx, tid) in std::iter::once(trigger_tid).chain(other_tids).enumerate() {
        let bid = db.with_mut_snapshot(|snap| snap.alloc_entity_id());
        let pos = center
            + DVec3::new(((idx as f64) - 1.0) * 2_500.0, ((idx % 2) as f64) * 1_500.0, ((idx / 2) as f64) * 2_000.0);
        let ball = Ball {
            id: bid,
            type_id: tid,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 0.8,
            mass: 5_000_000.0,
            radius: 45.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let _ = sys_handle.spawn_ball_with_combat(ball, build_type_combat_state_from_sde(sde, tid)).await;
        let sde_attr = |aid: u16| sde.types.get(&tid).and_then(|t| t.default_attributes.get(&aid).copied());
        entities.push(ExtendedNpcEntity {
                ball_id: bid,
                type_id: tid,
                faction_id: 500_005, // Sleepers
                archetype: "Sleeper".into(),
                solar_system_id,
                spawn_group_id: 0,
                target_id: None,
                aggression_threat: HashMap::new(),
                threat_table: HashMap::new(),
                bounty_isk: 0.0, // CONCORD pays no bounty for Sleepers
                optimal_range: 20_000.0,
                orbit_range: 15_000.0,
                damage_vector: eve_destiny::DamageVector { em: 80.0, thermal: 80.0, kinetic: 80.0, explosive: 80.0 },
                entropic_cycles: 0,
                drifter_overshield_hp: 0.0,
                drifter_doomsday_fired: false,
                web_speed_factor: sde
                    .types
                    .get(&tid)
                    .and_then(|t| t.default_attributes.get(&20).copied())
                    .filter(|&v| v < 0.0)
                    .unwrap_or(-50.0),
                web_range: 15_000.0,
                scram_strength: 2.0,
                scram_range: 12_000.0,
                neut_amount: 120.0,
                ecm_jammed: false,
                // Sleeper logistics: remote shield / armor amounts from SDE `shieldBonus` (68) /
                // `armorDamageAmount` (84) on the Sleeper type; 0 when the type carries no rep module.
                remote_shield_rep: sde_attr(68).unwrap_or(0.0),
                remote_armor_rep: sde_attr(84).unwrap_or(0.0),
                remote_rep_range: if sde_attr(68).or(sde_attr(84)).unwrap_or(0.0) > 0.0 { 25_000.0 } else { 0.0 },
                tag_drop_type_id: None,
                spawn_position: pos,
                sleeper_wh_class: wh_class,
                custom_cargo_loot: blue_loot,
            });
        ball_ids.push(bid);
    }
    if !entities.is_empty() {
        with_mut_combat_runtime_with_locks(db, combat_ephemeral, |rt| {
            for entity in entities {
                rt.npc_entities.insert(entity.ball_id, entity);
            }
        });
    }
    if wave == 1 {
        let sentry_bid = crate::services::wormhole_pve_escalations::spawn_sleeper_sentry_tower_for_class(
            systems,
            db,
            sde,
            combat_ephemeral,
            solar_system_id,
            wh_class,
            center,
        )
        .await;
        ball_ids.push(sentry_bid);
    }
    ball_ids
}

/// `ServerContext` wrapper over [`spawn_sleeper_wave_core`] used by production site seeding.
pub async fn spawn_sleeper_wave_in_system(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    wh_class: u8,
    wave: u8,
    center: DVec3,
) -> Vec<i64> {
    spawn_sleeper_wave_core(&ctx.systems, &ctx.db, &ctx.sde, &ctx.combat_ephemeral, solar_system_id, wh_class, wave, center).await
}

/// Drops a Sleeper wreck holding SDE Blue Loot when a Sleeper is destroyed by in-space damage
/// (`feat:pve.wormhole-sites`). Returns the wreck item ID.
pub async fn drop_sleeper_wreck_core(
    db: &Arc<WorldRepository>,
    systems: &Arc<SystemRegistry>,
    sde: &Arc<SdeRegistry>,
    solar_system_id: SolarSystemId,
    killer_char_id: i64,
    pos: DVec3,
    wh_class: u8,
) -> i64 {
    let wreck_type_id: TypeId = 30495; // Sleeper wreck
    let loot = sleeper_blue_loot_for_class(sde, wh_class);
    let now_unix = db.clock().now_unix_secs();
    let wreck_id = db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let wid = snap.next_entity_id;
        crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(snap, wid, now_unix, true);
        snap.items.insert(
            wid,
            InventoryItemRecord {
                item_id: wid,
                type_id: wreck_type_id,
                owner_id: killer_char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(wid, [pos.x, pos.y, pos.z]);
        if let Some((tid, qty)) = loot {
            snap.next_entity_id += 1;
            let lid = snap.next_entity_id;
            snap.items.insert(
                lid,
                InventoryItemRecord {
                    item_id: lid,
                    type_id: tid,
                    owner_id: killer_char_id,
                    location_id: wid,
                    flag: InventoryFlag::Cargo,
                    quantity: qty as i32,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
        wid
    });
    let _ = systems
        .get_or_spawn(solar_system_id)
        .spawn_ball(Ball {
            id: wreck_id,
            type_id: wreck_type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 50_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    wreck_id
}

type WaveSpawnKey = (usize, SolarSystemId, String);

fn inflight_wave_spawns(
) -> &'static Mutex<HashMap<WaveSpawnKey, Arc<tokio::sync::watch::Sender<bool>>>> {
    static INFLIGHT: OnceLock<Mutex<HashMap<WaveSpawnKey, Arc<tokio::sync::watch::Sender<bool>>>>> =
        OnceLock::new();
    INFLIGHT.get_or_init(|| Mutex::new(HashMap::new()))
}

struct WaveSpawnGuard {
    key: WaveSpawnKey,
    tx: Arc<tokio::sync::watch::Sender<bool>>,
}

impl Drop for WaveSpawnGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = inflight_wave_spawns().lock() {
            if map.get(&self.key).is_some_and(|cur| Arc::ptr_eq(cur, &self.tx)) {
                map.remove(&self.key);
            }
        }
        let _ = self.tx.send(true);
    }
}

fn current_wave_spawn_rx(key: &WaveSpawnKey) -> Option<tokio::sync::watch::Receiver<bool>> {
    let map = inflight_wave_spawns().lock().ok()?;
    map.get(key).map(|tx| tx.subscribe())
}

fn register_wave_spawn_guard(key: WaveSpawnKey) -> WaveSpawnGuard {
    let (tx, _) = tokio::sync::watch::channel(false);
    let tx = Arc::new(tx);
    if let Ok(mut map) = inflight_wave_spawns().lock() {
        map.insert(key.clone(), Arc::clone(&tx));
    }
    WaveSpawnGuard { key, tx }
}

fn site_has_unhacked_cans(
    reg: &PveExplorationRegistry,
    solar_system_id: SolarSystemId,
    site: &PveSiteInstance,
    include_ghost: bool,
) -> bool {
    let cat_match = matches!(site.site_category.as_str(), "Data" | "Relic")
        || (include_ghost && site.site_category == "Ghost");
    cat_match
        && !site.hackable_can_ids.is_empty()
        && !site
            .hackable_can_ids
            .iter()
            .all(|&cid| reg.hacked_containers.contains(&(solar_system_id, cid)))
}

fn site_wave_changed_err(message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: "SiteWaveChanged".into(),
        message: message.into(),
        client_error: None,
    }
}

fn claim_wave_transition_in_reg(
    db: &Arc<WorldRepository>,
    solar_system_id: SolarSystemId,
    site_id: &str,
    expected_wave: u8,
    next_wave: u8,
    new_ids: &[i64],
    keep_leftovers: bool,
) -> bool {
    let now_unix = db.clock().now_unix_secs();
    with_pve_exploration_reg_db(db, now_unix, |reg| {
        let now = now_sec(reg);
        let can_claim = reg
            .sites_by_system
            .get(&solar_system_id)
            .and_then(|sites| find_site_by_canonical_id(sites, site_id))
            .is_some_and(|site| {
                !site.cleared
                    && !is_site_expired_in_reg(reg, site, now)
                    && site.current_wave == expected_wave
            });
        if !can_claim {
            return false;
        }
        let Some(sites) = reg.sites_by_system.get_mut(&solar_system_id) else {
            return false;
        };
        let Some(site) = find_site_by_canonical_id_mut(sites, site_id) else {
            return false;
        };
        site.current_wave = next_wave;
        site.active_npc_ball_ids = if keep_leftovers {
            let leftovers = std::mem::take(&mut site.active_npc_ball_ids);
            new_ids.iter().copied().chain(leftovers).collect()
        } else {
            new_ids.to_vec()
        };
        true
    })
}

async fn cleanup_unclaimed_wave_balls(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    combat_ephemeral: &dyn AnyRwLock<CombatRuntimeState>,
    solar_system_id: SolarSystemId,
    balls: &[i64],
    remove_from_combat_rt: bool,
) {
    if balls.is_empty() {
        return;
    }
    let sys_handle = systems.get_or_spawn(solar_system_id);
    for &bid in balls {
        let _ = sys_handle.remove_ball(bid).await;
    }
    if remove_from_combat_rt {
        with_mut_combat_runtime_with_locks(db, combat_ephemeral, |rt| {
            for bid in balls {
                rt.npc_entities.remove(bid);
            }
        });
    }
}

/// Authoritative real in-space NPC destruction hook for PvE exploration sites (`feat:pve.escalations`,
/// `feat:pve.expeditions`, `feat:pve.ded-complexes`, `feat:pve.wormhole-sites`).
///
/// Called from the system-actor `npc_death_hook` (in-space damage) and from
/// `destroy_specialized_npc_and_queue_bounty`. Wormhole Sleeper sites (`WormholeCombat`) drop a Sleeper
/// wreck with SDE Blue Loot per kill (when `drop_npc_wreck`) and spawn the next wave as soon as the wave
/// trigger NPC (first active ball) dies. DED rooms only unlock their acceleration gate when cleared; the
/// final-room kill spawns the Overseer wreck. Other sites auto-advance waves. Site completion rolls
/// escalations (15-20%) and advances expeditions without invented client RPCs.
#[allow(clippy::too_many_arguments)]
pub async fn handle_pve_site_npc_destruction_core(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    sde: &Arc<SdeRegistry>,
    rng: &Arc<crate::rng::MechanicRng>,
    combat_ephemeral: &dyn AnyRwLock<CombatRuntimeState>,
    solar_system_id: SolarSystemId,
    npc_ball_id: i64,
    killer_char_id: i64,
    drop_npc_wreck: bool,
) -> (Option<String>, Option<EscalatingPathEntry>, Option<EscalatingPathEntry>) {
    Box::pin(handle_pve_site_npc_destruction_core_inner(
        systems,
        db,
        sde,
        rng,
        combat_ephemeral,
        solar_system_id,
        npc_ball_id,
        killer_char_id,
        drop_npc_wreck,
    ))
    .await
}

async fn handle_pve_site_npc_destruction_core_inner(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    sde: &Arc<SdeRegistry>,
    rng: &Arc<crate::rng::MechanicRng>,
    combat_ephemeral: &dyn AnyRwLock<CombatRuntimeState>,
    solar_system_id: SolarSystemId,
    npc_ball_id: i64,
    killer_char_id: i64,
    drop_npc_wreck: bool,
) -> (Option<String>, Option<EscalatingPathEntry>, Option<EscalatingPathEntry>) {
    let now_unix = db.clock().now_unix_secs();
    enum NpcKillLookupOutcome {
        NotFound,
        ExpiredSite(ExpiredSitePruneResult),
        ActiveHit {
            site_id: String,
            dungeon_id: u32,
            dungeon_name: String,
            site_category: String,
            ded_rating: Option<u8>,
            wormhole_class: Option<u8>,
            position: [f64; 3],
            current_wave: u8,
            total_waves: u8,
            wave_cleared: bool,
            was_wave_trigger: bool,
            expedition_instance_id: Option<i64>,
            normal_wave_balls: Vec<Ball>,
            spawn_guard: Option<WaveSpawnGuard>,
        },
    }
    let pve_key = db_pve_key(db);
    let lookup = with_pve_exploration_reg_snap_db(db, now_unix, |reg, snap| {
        let now = now_sec(reg);
        let matched = reg.sites_by_system.get(&solar_system_id).and_then(|sites| {
            for site in sites.iter() {
                if site.cleared {
                    continue;
                }
                if let Some(pos) = site.active_npc_ball_ids.iter().position(|&bid| bid == npc_ball_id) {
                    return Some((site.site_id.clone(), is_site_expired_in_reg(reg, site, now), pos));
                }
            }
            None
        });
        let Some((matched_site_id, expired, pos)) = matched else {
            return NpcKillLookupOutcome::NotFound;
        };
        if expired {
            return match expire_and_prune_site_in_reg(reg, solar_system_id, &matched_site_id, now) {
                Some(pruned) => NpcKillLookupOutcome::ExpiredSite(pruned),
                None => NpcKillLookupOutcome::NotFound,
            };
        }
        let Some(site) = reg.sites_by_system.get_mut(&solar_system_id).and_then(|sites| {
            sites
                .iter_mut()
                .find(|s| !s.cleared && s.site_id == matched_site_id)
        }) else {
            return NpcKillLookupOutcome::NotFound;
        };
        site.active_npc_ball_ids.remove(pos);
        let wave_cleared = site.active_npc_ball_ids.is_empty();
        let prev_wave = site.current_wave;
        let is_sleeper = site.site_category == "WormholeCombat" && site.wormhole_class.is_some();
        let mut normal_wave_balls = Vec::new();
        let mut spawn_guard = None;
        if wave_cleared && site.ded_rating.is_none() && site.current_wave < site.total_waves {
            if is_sleeper {
                spawn_guard = Some(register_wave_spawn_guard((
                    pve_key,
                    solar_system_id,
                    site.site_id.clone(),
                )));
            } else {
                site.current_wave += 1;
                normal_wave_balls = build_wave_npc_balls_in_snap(
                    sde,
                    snap,
                    solar_system_id,
                    site.current_wave,
                    site.position,
                    site.wormhole_class,
                );
                site.active_npc_ball_ids = normal_wave_balls.iter().map(|b| b.id).collect();
                if !normal_wave_balls.is_empty() {
                    spawn_guard = Some(register_wave_spawn_guard((
                        pve_key,
                        solar_system_id,
                        site.site_id.clone(),
                    )));
                }
            }
        }
        NpcKillLookupOutcome::ActiveHit {
            site_id: site.site_id.clone(),
            dungeon_id: site.dungeon_id,
            dungeon_name: site.dungeon_name.clone(),
            site_category: site.site_category.clone(),
            ded_rating: site.ded_rating,
            wormhole_class: site.wormhole_class,
            position: site.position,
            current_wave: prev_wave,
            total_waves: site.total_waves,
            wave_cleared,
            was_wave_trigger: pos == 0,
            expedition_instance_id: site.expedition_instance_id,
            normal_wave_balls,
            spawn_guard,
        }
    });

    let (
        site_id,
        dungeon_id,
        dungeon_name,
        site_category,
        ded_rating,
        wh_class,
        pos,
        (current_wave, total_waves),
        wave_cleared,
        was_wave_trigger,
        expedition_instance_id,
        normal_wave_balls,
        spawn_guard,
    ) = match lookup {
        NpcKillLookupOutcome::NotFound => return (None, None, None),
        NpcKillLookupOutcome::ExpiredSite(pruned) => {
            let _ = Box::pin(finalize_expired_site_prune_core(db, systems, pruned)).await;
            return (None, None, None);
        }
        NpcKillLookupOutcome::ActiveHit {
            site_id,
            dungeon_id,
            dungeon_name,
            site_category,
            ded_rating,
            wormhole_class,
            position,
            current_wave,
            total_waves,
            wave_cleared,
            was_wave_trigger,
            expedition_instance_id,
            normal_wave_balls,
            spawn_guard,
        } => (
            site_id,
            dungeon_id,
            dungeon_name,
            site_category,
            ded_rating,
            wormhole_class,
            position,
            (current_wave, total_waves),
            wave_cleared,
            was_wave_trigger,
            expedition_instance_id,
            normal_wave_balls,
            spawn_guard,
        ),
    };

    let sleeper_class = wh_class.filter(|_| site_category == "WormholeCombat");
    if let Some(wh) = sleeper_class {
        if drop_npc_wreck {
            let wreck_pos = DVec3::from_array(pos) + DVec3::new(0.0, 0.0, 1_000.0);
            Box::pin(drop_sleeper_wreck_core(
                db, systems, sde, solar_system_id, killer_char_id, wreck_pos, wh,
            )).await;
        }
        if was_wave_trigger && current_wave < total_waves {
            let next_wave = current_wave + 1;
            let new_ids = Box::pin(spawn_sleeper_wave_core(
                systems, db, sde, combat_ephemeral, solar_system_id, wh, next_wave, DVec3::from_array(pos),
            )).await;
            let claimed = claim_wave_transition_in_reg(
                db,
                solar_system_id,
                &site_id,
                current_wave,
                next_wave,
                &new_ids,
                true,
            );
            if !claimed {
                Box::pin(cleanup_unclaimed_wave_balls(
                    systems, db, combat_ephemeral, solar_system_id, &new_ids, true,
                )).await;
            }
            drop(spawn_guard);
            return (None, None, None);
        }
    }

    if !wave_cleared {
        drop(spawn_guard);
        return (None, None, None);
    }
    // DED rooms advance through `keeper.ActivateAccelerationGate`; clearing a room only unlocks its gate.
    if ded_rating.is_some() && current_wave < total_waves {
        drop(spawn_guard);
        return (None, None, None);
    }

    let sys_handle = systems.get_or_spawn(solar_system_id);

    if current_wave < total_waves {
        if let Some(wh) = sleeper_class {
            let next_wave = current_wave + 1;
            let spawned_ids = Box::pin(spawn_sleeper_wave_core(
                systems, db, sde, combat_ephemeral, solar_system_id, wh, next_wave, DVec3::from_array(pos),
            )).await;
            let claimed = claim_wave_transition_in_reg(
                db,
                solar_system_id,
                &site_id,
                current_wave,
                next_wave,
                &spawned_ids,
                false,
            );
            if !claimed {
                Box::pin(cleanup_unclaimed_wave_balls(
                    systems, db, combat_ephemeral, solar_system_id, &spawned_ids, true,
                )).await;
            }
            drop(spawn_guard);
        } else {
            for b in normal_wave_balls {
                let cs = build_type_combat_state_from_sde(sde, b.type_id);
                let _ = sys_handle.spawn_ball_with_combat(b, cs).await;
            }
            drop(spawn_guard);
        }
        (None, None, None)
    } else {
        drop(spawn_guard);
        enum FinalClearClaim {
            Claimed,
            NotClaimed,
            Expired(ExpiredSitePruneResult),
        }
        let post_now_unix = db.clock().now_unix_secs();
        let claim = with_pve_exploration_reg_db(db, post_now_unix, |reg| {
            let now = now_sec(reg);
            let expired = {
                let Some(sites) = reg.sites_by_system.get(&solar_system_id) else {
                    return FinalClearClaim::NotClaimed;
                };
                let Some(site) = sites.iter().find(|s| !s.cleared && s.site_id == site_id) else {
                    return FinalClearClaim::NotClaimed;
                };
                if site.current_wave != current_wave
                    || !site.active_npc_ball_ids.is_empty()
                    || site_has_unhacked_cans(reg, solar_system_id, site, false)
                {
                    return FinalClearClaim::NotClaimed;
                }
                is_site_expired_in_reg(reg, site, now)
            };
            if expired {
                return match expire_and_prune_site_in_reg(reg, solar_system_id, &site_id, now) {
                    Some(pruned) => FinalClearClaim::Expired(pruned),
                    None => FinalClearClaim::NotClaimed,
                };
            }
            let Some(site) = reg
                .sites_by_system
                .get_mut(&solar_system_id)
                .and_then(|l| find_site_by_canonical_id_mut(l, &site_id))
            else {
                return FinalClearClaim::NotClaimed;
            };
            if site.cleared || site.current_wave != current_wave || !site.active_npc_ball_ids.is_empty() {
                return FinalClearClaim::NotClaimed;
            }
            site.cleared = true;
            FinalClearClaim::Claimed
        });
        match claim {
            FinalClearClaim::NotClaimed => return (None, None, None),
            FinalClearClaim::Expired(pruned) => {
                let _ = Box::pin(finalize_expired_site_prune_core(db, systems, pruned)).await;
                return (None, None, None);
            }
            FinalClearClaim::Claimed => {}
        }

        db.with_mut_snapshot(|snap| {
            if let Some(anoms) = snap.cosmic_anomalies.get_mut(&solar_system_id) {
                if let Some(a) = anoms.iter_mut().find(|a| a.anomaly_id == site_id && !a.cleared) {
                    a.cleared = true;
                }
            }
            if let Some(sigs) = snap.cosmic_signatures.get_mut(&solar_system_id) {
                if let Some(s) = sigs.iter_mut().find(|s| s.signature_id == site_id && !s.collapsed) {
                    s.collapsed = true;
                }
            }
        });

        if let Some(rating) = ded_rating {
            Box::pin(spawn_ded_overseer_wreck_core(
                db,
                systems,
                sde,
                solar_system_id,
                killer_char_id,
                DVec3::from_array(pos),
                rating,
            )).await;
        } else if let Some(wh_c) = wh_class {
            // Site-completion Blue Loot from SDE group 880 "Sleeper Components" (class-scaled pick).
            if let Some((blue_tid, _)) = sleeper_blue_loot_for_class(sde, wh_c) {
                Box::pin(drop_site_loot_into_space_core(db, systems, solar_system_id, killer_char_id, pos, blue_tid, (wh_c as i64) * 3)).await;
            }
            if matches!(wh_c, 5 | 6)
                && matches!(site_category.as_str(), "WormholeCombat" | "Data" | "Relic")
            {
                let _ = Box::pin(crate::services::wormhole_pve_escalations::spawn_decloaked_transmission_relay_core(
                    systems, db, sde, combat_ephemeral, solar_system_id, &site_id, pos,
                )).await;
            }
        } else if site_category == "CombatAnomaly" || site_category == "CombatSignature" {
            let (cmd_tid, _) = crate::services::pve_exploration_seeding::pirate_commander_loot_for_system(sde, solar_system_id);
            Box::pin(drop_site_loot_into_space_core(db, systems, solar_system_id, killer_char_id, pos, cmd_tid, 1)).await;
        }

        let mut new_escalation = None;
        let mut advanced_step = None;

        if let Some(inst_id) = expedition_instance_id {
            advanced_step = Box::pin(crate::services::pve_expeditions::advance_expedition_step_core(
                db,
                sde,
                systems,
                killer_char_id,
                inst_id,
            )).await.ok().flatten();
        } else if matches!(site_category.as_str(), "CombatAnomaly" | "CombatSignature" | "DED") {
            let should_escalate = rng.roll(0.20);
            if should_escalate {
                new_escalation = Box::pin(crate::services::pve_expeditions::create_escalation_expedition_core(
                    db,
                    sde,
                    systems,
                    killer_char_id,
                    solar_system_id,
                    dungeon_id,
                    &dungeon_name,
                    pos,
                )).await;
            }
        }
        (Some(site_id), new_escalation, advanced_step)
    }
}

pub fn notify_pve_destruction_events(
    db: &WorldRepository,
    pending_notifications: &dyn AnyRwLock<HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>>,
    notify_tx: &tokio::sync::broadcast::Sender<(i64, crate::router::cache_notifications::PendingMachoNotification)>,
    killer_char_id: i64,
    cleared_site_id: Option<String>,
    new_escalation: Option<EscalatingPathEntry>,
    advanced_step: Option<EscalatingPathEntry>,
) {
    crate::services::notify_emitters_tournaments_anomalies::emit_expedition_combat_notifications(
        db,
        pending_notifications,
        notify_tx,
        cleared_site_id.as_deref(),
        new_escalation.as_ref(),
        advanced_step.as_ref(),
    );

    let cleared_sys_did_opt = cleared_site_id.as_ref().and_then(|site_id| {
        db.with_snapshot(|snap| {
            snap.owner_notes
                .get(&PVE_EXPLORATION_NOTE_ID)
                .and_then(|note| serde_json::from_str::<PveExplorationRegistry>(&note.note).ok())
                .and_then(|reg| {
                    reg.sites_by_system
                        .values()
                        .flatten()
                        .find(|s| s.site_id == *site_id)
                        .map(|s| (s.solar_system_id, s.dungeon_id))
                })
        })
    });

    let fleet_info = new_escalation.as_ref().and_then(|esc| {
        db.with_snapshot(|snap| {
            let fid = snap.characters.get(&killer_char_id).and_then(|c| c.fleet_id)?;
            if fid <= 0 {
                return None;
            }
            let members = snap
                .fleets
                .get(&fid)
                .map(|fl| fl.members.clone())
                .unwrap_or_default();
            Some((
                fid,
                members,
                killer_char_id,
                i64::from(esc.effective_journal_entry_template_id()),
            ))
        })
    });

    crate::services::pve_expeditions::emit_expedition_destruction_notifications(
        db,
        pending_notifications,
        notify_tx,
        cleared_sys_did_opt,
        new_escalation.as_ref(),
        advanced_step.as_ref(),
    );

    if let Some(site_id) = cleared_site_id {
        let cleared_did_opt = cleared_sys_did_opt.map(|(_, d)| d);
        crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_site_removed_with_db(
            db, pending_notifications, notify_tx, &site_id,
        );
        crate::router::notify_registry::emit_typed_char_core(
            db,
            Some(pending_notifications),
            Some(notify_tx),
            killer_char_id,
            None,
            crate::router::notify_registry::OnCosmicAnomalyRemoved { site_id },
        );
        if let Some(did) = cleared_did_opt {
            crate::router::notify_registry::emit_typed_char_core(
                db,
                Some(pending_notifications),
                Some(notify_tx),
                killer_char_id,
                None,
                crate::router::notify_registry::OnDungeonCompleted {
                    dungeon_id: i64::from(did),
                },
            );
        }
    }

    for esc in [new_escalation, advanced_step].into_iter().flatten() {
        crate::router::notify_registry::emit_typed_char_core(
            db,
            Some(pending_notifications),
            Some(notify_tx),
            killer_char_id,
            None,
            crate::router::notify_registry::OnEscalatingPathChange,
        );
        crate::router::notify_registry::emit_typed_char_core(
            db,
            Some(pending_notifications),
            Some(notify_tx),
            killer_char_id,
            None,
            crate::router::notify_registry::OnEscalatingPathMessage {
                instance_id: esc.instance_id,
            },
        );
    }

    if let Some((fid, members, char_id, template_id)) = fleet_info {
        crate::router::notify_registry::emit_typed_to_fleet_members_core(
            db,
            Some(pending_notifications),
            Some(notify_tx),
            fid,
            &members,
            crate::router::notify_registry::OnFleetEscalatingPathMessage {
                char_id,
                journal_entry_template_id: template_id,
            },
        );
    }
}

pub fn pve_site_category_and_dungeon_id(
    db: &Arc<WorldRepository>,
    now_unix: u64,
    solar_system_id: SolarSystemId,
    site_id: &str,
) -> (String, u32) {
    crate::services::pve_exploration::read_pve_exploration_reg_db(db, now_unix, |reg| {
        reg.sites_by_system
            .get(&solar_system_id)
            .and_then(|l| find_site_by_canonical_id(l, site_id))
            .map(|s| (s.site_category.clone(), s.dungeon_id))
            .unwrap_or_default()
    })
}

pub async fn handle_pve_site_npc_destruction(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    npc_ball_id: i64,
    killer_char_id: i64,
) {
    // `destroy_specialized_npc_and_queue_bounty` already dropped this NPC's wreck, so no extra Sleeper wreck.
    let (cleared_site_id, new_escalation, advanced_step) = handle_pve_site_npc_destruction_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.mechanic_rng,
        &ctx.combat_ephemeral,
        solar_system_id,
        npc_ball_id,
        killer_char_id,
        false,
    ).await;
    crate::services::combat_crimewatch::flush_combat_runtime_to_db(ctx);

    if let Some(ref sid) = cleared_site_id {
        let now = ctx.now_unix();
        let (cat, did) = pve_site_category_and_dungeon_id(&ctx.db, now, solar_system_id, sid);
        ctx.quasar_gateway.progression.extra.on_system_tale_encounter_completed(solar_system_id, &cat, did, 1, now, &ctx.quasar_gateway);
        crate::quasar_gateway::isolate_side_effect("pve_exploration.career_combat_site_done", || {
            crate::services::quasar_career_goals::record_combat_site_completion_in_system(
                &ctx.quasar_gateway,
                &ctx.sde,
                solar_system_id,
                killer_char_id,
                &cat,
            );
        });
    }
    notify_pve_destruction_events(
        &ctx.db,
        &ctx.pending_notifications,
        &ctx.notify_tx,
        killer_char_id,
        cleared_site_id,
        new_escalation,
        advanced_step,
    );
    Box::pin(crate::services::pve_dungeon_runtime::on_dungeon_npc_killed(
        ctx,
        solar_system_id,
        npc_ball_id,
        killer_char_id,
    ))
    .await;
}

// parity: feat:pve.combat-anomalies, feat:pve.ded-complexes, feat:pve.escalations, feat:pve.expeditions, feat:pve.wormhole-sites
pub async fn advance_site_wave_or_complete(
    ctx: &ServerContext,
    session: &mut SessionState,
    solar_system_id: SolarSystemId,
    site_id: &str,
) -> Result<PyRep, BridgeRpcError> {
    advance_site_wave_or_complete_impl(ctx, session, solar_system_id, site_id, || async {}).await
}

#[cfg(test)]
pub(crate) async fn advance_site_wave_or_complete_with_mid_await_hook<F, Fut>(
    ctx: &ServerContext,
    session: &mut SessionState,
    solar_system_id: SolarSystemId,
    site_id: &str,
    after_ball_snapshot_await: F,
) -> Result<PyRep, BridgeRpcError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    advance_site_wave_or_complete_impl(
        ctx,
        session,
        solar_system_id,
        site_id,
        after_ball_snapshot_await,
    )
    .await
}

async fn advance_site_wave_or_complete_impl<F, Fut>(
    ctx: &ServerContext,
    session: &mut SessionState,
    solar_system_id: SolarSystemId,
    site_id: &str,
    after_ball_snapshot_await: F,
) -> Result<PyRep, BridgeRpcError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: format!("Cannot advance PvE site {site_id} while docked"),
            client_error: None,
        });
    }
    if session.solar_system_id != Some(solar_system_id) {
        return Err(BridgeRpcError {
            code: "NotInSiteSystem".into(),
            message: format!("Caller must be in solar system {solar_system_id} to advance site {site_id}"),
            client_error: None,
        });
    }
    ensure_system_pve_sites_seeded(ctx, solar_system_id).await;

    enum AdvancePrecheck {
        WaitingForSpawn(tokio::sync::watch::Receiver<bool>),
        Ready {
            canonical_site_id: String,
            checked_wave: u8,
            current_balls: Vec<i64>,
            is_sleeper_cache_stabilized: bool,
        },
        NeedsExpiryPrune(String),
    }

    let pve_key = ctx_pve_key(ctx);
    let (canonical_site_id, checked_wave, current_balls, is_sleeper_cache_stabilized) = loop {
        let precheck = read_pve_exploration_reg(ctx, |reg| -> Result<AdvancePrecheck, BridgeRpcError> {
            let now = now_sec(reg);
            let Some(sites) = reg.sites_by_system.get(&solar_system_id) else {
                return Err(BridgeRpcError {
                    code: "SiteNotFound".into(),
                    message: format!("PvE site {site_id} not found in system {solar_system_id}"),
                    client_error: None,
                });
            };
            let site_key = ExplorationSiteKeyRef(site_id);
            let Some(site) = find_site_by_canonical_key_any_state_in_reg(reg, sites, site_key, now)
            else {
                return Err(BridgeRpcError {
                    code: "SiteNotFound".into(),
                    message: format!("PvE site {site_id} not found in system {solar_system_id}"),
                    client_error: None,
                });
            };
            if let Some(rx) = current_wave_spawn_rx(&(pve_key, solar_system_id, site.site_id.clone())) {
                return Ok(AdvancePrecheck::WaitingForSpawn(rx));
            }
            if site.cleared {
                return Err(BridgeRpcError {
                    code: "SiteAlreadyCleared".into(),
                    message: format!("PvE site {} is already cleared", site.site_id),
                    client_error: None,
                });
            }
            if is_site_expired_in_reg(reg, site, now) {
                return Ok(AdvancePrecheck::NeedsExpiryPrune(site.site_id.clone()));
            }
            if site_has_unhacked_cans(reg, solar_system_id, site, true) {
                return Err(BridgeRpcError {
                    code: "SiteContainersUnhacked".into(),
                    message: format!(
                        "Cannot complete exploration site {} while hackable containers remain unhacked",
                        site.site_id
                    ),
                    client_error: None,
                });
            }
            Ok(AdvancePrecheck::Ready {
                canonical_site_id: site.site_id.clone(),
                checked_wave: site.current_wave,
                current_balls: site.active_npc_ball_ids.clone(),
                is_sleeper_cache_stabilized: site.site_category == "SleeperCache"
                    && site.sleeper_rift_stabilized,
            })
        })?;

        match precheck {
            AdvancePrecheck::WaitingForSpawn(mut rx) => {
                let _ = rx.wait_for(|&done| done).await;
            }
            AdvancePrecheck::NeedsExpiryPrune(expired_sid) => {
                let pruned_opt = with_pve_exploration_reg(ctx, |reg| {
                    let now = now_sec(reg);
                    expire_and_prune_site_in_reg(reg, solar_system_id, &expired_sid, now)
                });
                if let Some(pruned) = pruned_opt {
                    return Err(reject_expired_site_prune(ctx, pruned).await);
                }
                return Err(BridgeRpcError {
                    code: "SiteNotFound".into(),
                    message: format!("PvE site {site_id} not found in system {solar_system_id}"),
                    client_error: None,
                });
            }
            AdvancePrecheck::Ready {
                canonical_site_id,
                checked_wave,
                current_balls,
                is_sleeper_cache_stabilized,
            } => break (canonical_site_id, checked_wave, current_balls, is_sleeper_cache_stabilized),
        }
    };

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    if !is_sleeper_cache_stabilized {
        for &bid in &current_balls {
            if sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_some() {
                return Err(BridgeRpcError {
                    code: "WaveNpcsRemaining".into(),
                    message: format!("Cannot advance site {canonical_site_id}: wave NPC ball {bid} has not been destroyed"),
                    client_error: None,
                });
            }
        }
    }
    after_ball_snapshot_await().await;

    enum AdvanceCommitOutcome {
        Expired(ExpiredSitePruneResult),
        Committed {
            canonical_site_id: String,
            dungeon_id: u32,
            dungeon_name: String,
            site_category: String,
            ded_rating: Option<u8>,
            wh_class: Option<u8>,
            pos: [f64; 3],
            new_wave: u8,
            total_waves: u8,
            old_balls: Vec<i64>,
            next_balls: Vec<Ball>,
            spawned_npc_ids: Vec<i64>,
            completed_now: bool,
            exp_inst_id: Option<i64>,
            spawn_guard: Option<WaveSpawnGuard>,
        },
    }

    let commit_outcome = try_with_pve_exploration_reg_snap(ctx, |reg, snap| {
        let now = now_sec(reg);
        let (resolved_site_id, expired_now) = {
            let Some(sites) = reg.sites_by_system.get(&solar_system_id) else {
                return Err(BridgeRpcError {
                    code: "SiteNotFound".into(),
                    message: format!("PvE site {canonical_site_id} not found in system {solar_system_id}"),
                    client_error: None,
                });
            };
            let Some(site) = find_site_by_canonical_id(sites, &canonical_site_id) else {
                return Err(BridgeRpcError {
                    code: "SiteNotFound".into(),
                    message: format!("PvE site {canonical_site_id} not found in system {solar_system_id}"),
                    client_error: None,
                });
            };
            if site.cleared {
                return Err(BridgeRpcError {
                    code: "SiteAlreadyCleared".into(),
                    message: format!("PvE site {} is already cleared", site.site_id),
                    client_error: None,
                });
            }
            (
                site.site_id.clone(),
                is_site_expired_in_reg(reg, site, now),
            )
        };
        if expired_now {
            if let Some(pruned) =
                expire_and_prune_site_in_reg(reg, solar_system_id, &resolved_site_id, now)
            {
                return Ok(AdvanceCommitOutcome::Expired(pruned));
            }
            return Err(BridgeRpcError {
                code: "SiteNotFound".into(),
                message: format!("PvE site {resolved_site_id} not found in system {solar_system_id}"),
                client_error: None,
            });
        }
        let has_unhacked_cans = reg
            .sites_by_system
            .get(&solar_system_id)
            .and_then(|sites| find_site_by_canonical_id(sites, &resolved_site_id))
            .is_some_and(|site| site_has_unhacked_cans(reg, solar_system_id, site, true));
        if has_unhacked_cans {
            return Err(BridgeRpcError {
                code: "SiteContainersUnhacked".into(),
                message: format!(
                    "Cannot complete exploration site {resolved_site_id} while hackable containers remain unhacked"
                ),
                client_error: None,
            });
        }
        let sites = reg.sites_by_system.entry(solar_system_id).or_default();
        let Some(site) = find_site_by_canonical_id_mut(sites, &resolved_site_id) else {
            return Err(BridgeRpcError {
                code: "SiteNotFound".into(),
                message: format!("PvE site {resolved_site_id} not found in system {solar_system_id}"),
                client_error: None,
            });
        };
        if site.current_wave != checked_wave {
            return Err(site_wave_changed_err(format!(
                "PvE site {resolved_site_id} wave changed from {checked_wave} to {} while advancing",
                site.current_wave
            )));
        }
        if site.active_npc_ball_ids != current_balls {
            if let Some(&remaining_bid) = site.active_npc_ball_ids.first() {
                return Err(BridgeRpcError {
                    code: "WaveNpcsRemaining".into(),
                    message: format!(
                        "Cannot advance site {resolved_site_id}: active wave NPCs changed ({remaining_bid} remaining)"
                    ),
                    client_error: None,
                });
            }
            return Err(site_wave_changed_err(format!(
                "PvE site {resolved_site_id} wave NPCs changed while advancing"
            )));
        }
        let old = std::mem::take(&mut site.active_npc_ball_ids);
        let done = site.current_wave >= site.total_waves;
        let next_balls = if done {
            site.cleared = true;
            Vec::new()
        } else {
            site.current_wave += 1;
            let balls = build_wave_npc_balls_in_snap(
                &ctx.sde,
                snap,
                solar_system_id,
                site.current_wave,
                site.position,
                site.wormhole_class,
            );
            site.active_npc_ball_ids = balls.iter().map(|b| b.id).collect();
            balls
        };
        let spawned_npc_ids = if done {
            Vec::new()
        } else {
            site.active_npc_ball_ids.clone()
        };
        let spawn_guard = if !next_balls.is_empty() {
            Some(register_wave_spawn_guard((
                pve_key,
                solar_system_id,
                site.site_id.clone(),
            )))
        } else {
            None
        };
        Ok(AdvanceCommitOutcome::Committed {
            canonical_site_id: site.site_id.clone(),
            dungeon_id: site.dungeon_id,
            dungeon_name: site.dungeon_name.clone(),
            site_category: site.site_category.clone(),
            ded_rating: site.ded_rating,
            wh_class: site.wormhole_class,
            pos: site.position,
            new_wave: site.current_wave,
            total_waves: site.total_waves,
            old_balls: old,
            next_balls,
            spawned_npc_ids,
            completed_now: done,
            exp_inst_id: site.expedition_instance_id,
            spawn_guard,
        })
    })?;

    let (
        canonical_site_id,
        dungeon_id,
        dungeon_name,
        site_category,
        ded_rating,
        wh_class,
        pos,
        new_wave,
        total_waves,
        old_balls,
        next_balls,
        spawned_npc_ids,
        completed_now,
        exp_inst_id,
        spawn_guard,
    ) = match commit_outcome {
        AdvanceCommitOutcome::Expired(pruned) => {
            return Err(reject_expired_site_prune(ctx, pruned).await);
        }
        AdvanceCommitOutcome::Committed {
            canonical_site_id,
            dungeon_id,
            dungeon_name,
            site_category,
            ded_rating,
            wh_class,
            pos,
            new_wave,
            total_waves,
            old_balls,
            next_balls,
            spawned_npc_ids,
            completed_now,
            exp_inst_id,
            spawn_guard,
        } => (
            canonical_site_id,
            dungeon_id,
            dungeon_name,
            site_category,
            ded_rating,
            wh_class,
            pos,
            new_wave,
            total_waves,
            old_balls,
            next_balls,
            spawned_npc_ids,
            completed_now,
            exp_inst_id,
            spawn_guard,
        ),
    };

    for bid in old_balls {
        let _ = sys_handle.remove_ball(bid).await;
    }

    let mut dropped_loot_items = Vec::new();
    let mut escalation_created: Option<EscalatingPathEntry> = None;

    if !completed_now {
        for b in next_balls {
            let cs = build_type_combat_state_from_sde(&ctx.sde, b.type_id);
            let _ = sys_handle.spawn_ball_with_combat(b, cs).await;
        }
        drop(spawn_guard);
    } else {
        drop(spawn_guard);
        let mut loot_specs: Vec<(TypeId, i64, String)> = Vec::new();

        // DED Overseer wrecks are only dropped by the real overseer kill in `handle_pve_site_npc_destruction_core`.
        if let Some(wh_c) = wh_class.filter(|_| {
            ded_rating.is_none()
                && !matches!(site_category.as_str(), "Data" | "Relic" | "Ghost")
        }) {
            // Site-completion Blue Loot from SDE group 880 "Sleeper Components".
            if let Some((blue_tid, _)) = sleeper_blue_loot_for_class(&ctx.sde, wh_c) {
                let blue_name = ctx.sde.types.get(&blue_tid).map(|t| t.name.clone()).unwrap_or_default();
                loot_specs.push((blue_tid, (wh_c as i64) * 3, blue_name));
            }
        } else if matches!(site_category.as_str(), "CombatAnomaly" | "CombatSignature") {
            let (cmd_tid, cmd_name) = crate::services::pve_exploration_seeding::pirate_commander_loot_for_system(&ctx.sde, solar_system_id);
            loot_specs.push((cmd_tid, 1, cmd_name.to_string()));
        }

        for (l_tid, l_qty, l_name) in loot_specs {
            let item_id = drop_site_loot_into_space(ctx, solar_system_id, char_id, pos, l_tid, l_qty).await;
            dropped_loot_items.push((item_id, l_tid, l_qty, l_name));
        }

        if let Some(5 | 6) = wh_class {
            if matches!(site_category.as_str(), "WormholeCombat" | "Data" | "Relic") {
                let _ = crate::services::wormhole_pve_escalations::spawn_decloaked_transmission_relay_core(
                    &ctx.systems,
                    &ctx.db,
                    &ctx.sde,
                    &ctx.combat_ephemeral,
                    solar_system_id,
                    &canonical_site_id,
                    pos,
                ).await;
                crate::services::combat_crimewatch::flush_combat_runtime_to_db(ctx);
            }
        }

        let should_escalate = matches!(site_category.as_str(), "CombatAnomaly" | "CombatSignature" | "DED")
            && crate::services::pve_expeditions::roll_escalation_chance(ctx);

        if let Some(inst_id) = exp_inst_id {
            let _ = crate::services::pve_expeditions::advance_expedition_step(ctx, char_id, inst_id).await;
        } else if should_escalate {
            escalation_created = crate::services::pve_expeditions::create_escalation_expedition(
                ctx,
                char_id,
                solar_system_id,
                dungeon_id,
                &dungeon_name,
                pos,
            ).await;
        }

        let (was_anom, was_sig) = ctx.db.with_mut_snapshot(|snap| {
            let mut a_hit = false;
            let mut s_hit = false;
            if let Some(anoms) = snap.cosmic_anomalies.get_mut(&solar_system_id) {
                if let Some(a) = anoms.iter_mut().find(|a| a.anomaly_id == canonical_site_id && !a.cleared) {
                    a.cleared = true;
                    a_hit = true;
                }
            }
            if let Some(sigs) = snap.cosmic_signatures.get_mut(&solar_system_id) {
                if let Some(s) = sigs.iter_mut().find(|s| s.signature_id == canonical_site_id && !s.collapsed) {
                    s.collapsed = true;
                    s_hit = true;
                }
            }
            (a_hit, s_hit)
        });
        if was_anom {
            crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_anomaly_delta(ctx, solar_system_id, &[], &[canonical_site_id.clone()]);
        }
        if was_sig {
            crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_signature_delta(ctx, solar_system_id, &[], &[canonical_site_id.clone()]);
        }

        crate::router::notify_registry::push_typed_session_notification(
            session,
            crate::router::notify_registry::OnCosmicAnomalyRemoved {
                site_id: canonical_site_id.clone(),
            },
        );
        ctx.quasar_gateway.progression.extra.on_system_tale_encounter_completed(
            solar_system_id, &site_category, dungeon_id, 1, ctx.now_unix(), &ctx.quasar_gateway,
        );
        crate::quasar_gateway::isolate_side_effect("pve_exploration.advance_career_combat_site", || {
            crate::services::quasar_career_goals::record_combat_site_completion_in_system(
                &ctx.quasar_gateway,
                &ctx.sde,
                solar_system_id,
                char_id,
                &site_category,
            );
        });
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnDungeonCompleted {
                dungeon_id: i64::from(dungeon_id),
            },
        );
    }

    let loot_py: Vec<PyRep> = dropped_loot_items
        .iter()
        .map(|(iid, tid, qty, name)| {
            PyRep::keyval([
                ("itemID", PyRep::Integer(*iid)),
                ("typeID", PyRep::Integer(*tid as i64)),
                ("quantity", PyRep::Integer(*qty)),
                ("typeName", PyRep::String(name.clone())),
            ])
        })
        .collect();

    Ok(PyRep::keyval([
        ("siteID", PyRep::String(canonical_site_id)),
        ("dungeonID", PyRep::Integer(dungeon_id as i64)),
        ("dungeonName", PyRep::String(dungeon_name)),
        ("currentWave", PyRep::Integer(new_wave as i64)),
        ("totalWaves", PyRep::Integer(total_waves as i64)),
        ("completed", PyRep::Bool(completed_now)),
        ("activeNpcBallIDs", PyRep::List(spawned_npc_ids.into_iter().map(PyRep::Integer).collect())),
        ("droppedLoot", PyRep::List(loot_py)),
        ("escalated", PyRep::Bool(escalation_created.is_some())),
        (
            "escalationInstanceID",
            escalation_created.as_ref().map(|e| PyRep::Integer(e.instance_id)).unwrap_or(PyRep::None),
        ),
    ]))
}

#[cfg(test)]
#[path = "pve_exploration_sites_tests.rs"]
mod pve_exploration_sites_tests;

#[cfg(test)]
#[path = "pve_exploration_sites_edges_tests.rs"]
mod pve_exploration_sites_edges_tests;

