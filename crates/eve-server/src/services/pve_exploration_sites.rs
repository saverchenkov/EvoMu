//! `pve_exploration_sites` — SDE-backed PvE Cosmic Exploration Sites, DED Complexes & Wormhole Sleeper Combat.
// parity: feat:pve.data-sites, feat:pve.relic-sites, feat:pve.loot-spew, feat:pve.ded-complexes, feat:pve.wormhole-sites

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use glam::DVec3;
use eve_db::{InventoryFlag, InventoryItemRecord, SolarSystemId, TypeId, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_proto::BridgeRpcError;
use eve_sde::{InvType, SdeRegistry};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{CombatRuntimeState, ExtendedNpcEntity};
use crate::services::pve_exploration::SpewedLootCan;
use crate::services::pve_exploration_seeding::{
    build_type_combat_state_from_sde, pirate_faction_for_system, PirateFaction,
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
        });
    };
    let grp = ship_type.group_id;
    if !allowed_groups.contains(&grp) || ship_mass > max_mass {
        return Err(BridgeRpcError {
            code: "ShipSizeRestricted".into(),
            message: format!("Acceleration gate rejects ship hull {} (group {grp}, mass {ship_mass:.0} kg): {label}", ship_type.name),
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
        for (tid, qty, _) in &loot_items {
            snap.next_entity_id += 1;
            let item_id = snap.next_entity_id;
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
    combat_ephemeral: &Arc<RwLock<CombatRuntimeState>>,
    solar_system_id: SolarSystemId,
    wh_class: u8,
    wave: u8,
    center: DVec3,
) -> Vec<i64> {
    let (trigger_tid, other_tids) = sleeper_npc_types_for_class_and_wave(wh_class, wave);
    let blue_loot = sleeper_blue_loot_for_class(sde, wh_class);
    let sys_handle = systems.get_or_spawn(solar_system_id);
    let mut ball_ids = Vec::with_capacity(1 + other_tids.len());
    for (idx, tid) in std::iter::once(trigger_tid).chain(other_tids).enumerate() {
        let bid = db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            snap.next_entity_id
        });
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
        let mut rt = combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
        rt.npc_entities.insert(
            bid,
            ExtendedNpcEntity {
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
                web_speed_factor: 0.5,
                web_range: 15_000.0,
                scram_strength: 2.0,
                scram_range: 12_000.0,
                neut_amount: 120.0,
                ecm_jammed: false,
                remote_shield_rep: 0.0,
                remote_armor_rep: 0.0,
                remote_rep_range: 0.0,
                tag_drop_type_id: None,
                spawn_position: pos,
                sleeper_wh_class: wh_class,
                custom_cargo_loot: blue_loot,
            },
        );
        drop(rt);
        ball_ids.push(bid);
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
