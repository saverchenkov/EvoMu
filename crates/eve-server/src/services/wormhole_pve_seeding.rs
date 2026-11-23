//! Wormhole PvE special-system seeding (C13 small-ship shattered, standard shattered Debris/Ice
//! fields, Drifter systems), Sleeper sentry towers, and specialized NPC SDE component construction.
//!
// parity: feat:navigation.small-ship-shattered-wormholes, feat:navigation.standard-shattered-wormholes
// parity: feat:navigation.drifter-wormhole-systems, feat:pve.sleeper-sentry-towers

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::{with_mut_combat_runtime_with_locks, CombatRuntimeState, ExtendedNpcEntity};
use crate::services::pve_exploration::{
    with_pve_exploration_reg, CharacterId, ExplorationSiteId, PveSiteInstance,
};
use crate::services::pve_exploration_seeding::build_type_combat_state_from_sde;
use crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde;
use crate::system_actor::SystemRegistry;
use eve_db::{
    AsteroidRecord, CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord,
    WorldRepository,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
use glam::DVec3;
use eve_db::lock_rank::AnyRwLock;
use std::collections::HashMap;
use std::sync::Arc;

pub const SHATTERED_DEBRIS_ORE_TYPES: [TypeId; 15] = [
    1230, 1228, 1224, 18, 1227, 20, 1226, 1231, 21, 1229, 1232, 1225, 1223, 22, 11396,
];

pub const SHATTERED_ICE_TYPES: [TypeId; 8] = [
    16264, 16262, 16263, 16265, 16266, 16267, 16268, 16269,
];

pub fn sleeper_sentry_type_for_class(wh_class: u8) -> TypeId {
    match wh_class {
        1 | 2 => 30_460, // Vigilant Sentry Tower
        3 | 4 => 30_461, // Wakeful Sentry Tower
        _ => 30_462,     // Restless Sentry Tower
    }
}

pub fn is_sleeper_sentry_type(type_id: TypeId) -> bool {
    matches!(type_id, 30_460 | 30_461 | 30_462)
}

pub fn capital_escalation_class_for_group(group_id: u32) -> Option<bool> {
    match group_id {
        485 | 4594 => Some(true),  // Dreadnought / Lancer Dreadnought
        547 | 1538 => Some(false), // Carrier / Force Auxiliary (FAX)
        _ => None,
    }
}

pub fn is_any_capital_group(group_id: u32) -> bool {
    matches!(group_id, 485 | 4594 | 547 | 1538 | 30 | 659)
}

pub fn build_sleeper_sentry_entity(
    sde: &SdeRegistry,
    solar_system_id: SolarSystemId,
    ball_id: i64,
    type_id: TypeId,
    pos: DVec3,
    wh_class: u8,
) -> (Ball, CombatState, ExtendedNpcEntity) {
    let sde_type = sde.types.get(&type_id);
    let attr = |aid: u16| -> Option<f64> {
        sde_type.and_then(|t| t.default_attributes.get(&aid).copied())
    };
    // All tower stats come from the SDE type (`54` range, `64` damage multiplier, `114`/`116`/`117`/`118`
    // damage, `552` signature, `9`/`265` HP); a missing attribute contributes nothing.
    let opt_range = attr(54).unwrap_or(0.0);
    let dmg_mult = attr(64).unwrap_or(0.0);
    let em = attr(114).unwrap_or(0.0) * dmg_mult;
    let thm = attr(118).unwrap_or(0.0) * dmg_mult;
    let kin = attr(117).unwrap_or(0.0) * dmg_mult;
    let exp = attr(116).unwrap_or(0.0) * dmg_mult;
    let mut cs = build_type_combat_state_from_sde(sde, type_id);
    cs.signature_radius = attr(552).unwrap_or(0.0);
    let ball = Ball {
        id: ball_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: sde_type.map_or(0.0, |t| t.mass),
        radius: sde_type.map_or(0.0, |t| t.radius),
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let entity = ExtendedNpcEntity {
        ball_id,
        type_id,
        faction_id: 500_005,
        archetype: "SleeperSentry".into(),
        solar_system_id,
        spawn_group_id: 0,
        target_id: None,
        aggression_threat: HashMap::new(),
        threat_table: HashMap::new(),
        bounty_isk: 0.0,
        optimal_range: opt_range,
        orbit_range: 0.0,
        damage_vector: DamageVector {
            em,
            thermal: thm,
            kinetic: kin,
            explosive: exp,
        },
        entropic_cycles: 0,
        drifter_overshield_hp: 0.0,
        drifter_doomsday_fired: false,
        web_speed_factor: 0.0,
        web_range: 0.0,
        scram_strength: 0.0,
        scram_range: 0.0,
        neut_amount: 0.0,
        ecm_jammed: false,
        remote_shield_rep: 0.0,
        remote_armor_rep: 0.0,
        remote_rep_range: 0.0,
        tag_drop_type_id: None,
        spawn_position: pos,
        sleeper_wh_class: wh_class,
        custom_cargo_loot: None,
    };
    (ball, cs, entity)
}

#[allow(clippy::too_many_arguments)]
pub async fn spawn_sleeper_sentry_tower_for_class(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    sde: &Arc<SdeRegistry>,
    combat_ephemeral: &dyn AnyRwLock<CombatRuntimeState>,
    solar_system_id: SolarSystemId,
    wh_class: u8,
    center: DVec3,
) -> i64 {
    let tid = sleeper_sentry_type_for_class(wh_class);
    let bid = db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        snap.next_entity_id
    });
    let pos = center + DVec3::new(3_000.0, 0.0, 2_500.0);
    let (ball, cs, entity) =
        build_sleeper_sentry_entity(sde, solar_system_id, bid, tid, pos, wh_class);
    let sde_t = sde.types.get(&tid);
    let fac_owner = if entity.faction_id > 0 {
        entity.faction_id as i64
    } else {
        500_005
    };
    let sentry_meta = eve_destiny::SlimItemMeta {
        group_id: sde_t.map(|t| t.group_id).unwrap_or(959),
        category_id: sde_t.map(|t| t.category_id).unwrap_or(11),
        owner_id: fac_owner,
        char_id: None,
        corp_id: crate::services::combat_crimewatch::combat_npc_ai::resolve_npc_corp_id_for_sde_faction(
            sde,
            fac_owner,
        ),
        alliance_id: -1,
        security_status: -1.0,
        is_cloaked: false,
        name: sde_t.map(|t| t.name.clone()).filter(|n| !n.is_empty()),
        ..Default::default()
    };
    let sys_handle = systems.get_or_spawn(solar_system_id);
    let _ = sys_handle.spawn_ball_with_meta(ball, cs, sentry_meta).await;
    with_mut_combat_runtime_with_locks(db, combat_ephemeral, |rt| {
        rt.npc_entities.insert(bid, entity);
    });
    bid
}

pub async fn spawn_sleeper_sentry_tower(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    wh_class: u8,
    center: DVec3,
) -> i64 {
    spawn_sleeper_sentry_tower_for_class(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        solar_system_id,
        wh_class,
        center,
    )
    .await
}

pub fn can_sentry_fire_and_record(
    ctx: &ServerContext,
    sentry_ball_id: i64,
    interval_s: f64,
    now_sim: f64,
) -> bool {
    let now_unix = ctx.now_unix_secs() as f64;
    with_pve_exploration_reg(ctx, |reg| {
        let prev_unix = reg.sentry_last_fire_unix.get(&sentry_ball_id).copied();
        let prev_sim = reg.sentry_last_fire_sim_s.get(&sentry_ball_id).copied();
        let ready = match (prev_unix, prev_sim) {
            (None, _) | (_, None) => true,
            (Some(pu), Some(ps)) => (now_unix - pu) >= interval_s || (now_sim - ps) >= interval_s,
        };
        if ready {
            reg.sentry_last_fire_unix.insert(sentry_ball_id, now_unix);
            reg.sentry_last_fire_sim_s.insert(sentry_ball_id, now_sim);
        }
        ready
    })
}

pub async fn drop_site_loot_into_space_core(
    db: &Arc<WorldRepository>,
    systems: &Arc<SystemRegistry>,
    solar_system_id: SolarSystemId,
    char_id: CharacterId,
    pos: [f64; 3],
    type_id: TypeId,
    quantity: i64,
) -> i64 {
    let now_unix = db.clock().now_unix_secs();
    let container_type_id: TypeId = 3293;
    let (container_id, item_id) = db.with_mut_snapshot(|snap| {
        let item_id = snap.alloc_entity_id();
        let cid = snap.alloc_entity_id();
        crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(snap, cid, now_unix, false);
        snap.items.insert(
            cid,
            InventoryItemRecord {
                item_id: cid,
                type_id: container_type_id,
                owner_id: char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(cid, pos);
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id: char_id,
                location_id: cid,
                flag: InventoryFlag::Cargo,
                quantity: quantity as i32,
                singleton: false,
                ..Default::default()
            },
        );
        (cid, item_id)
    });
    let sys_handle = systems.get_or_spawn(solar_system_id);
    let _ = sys_handle
        .spawn_ball_with_meta(
            Ball {
                id: container_id,
                type_id: container_type_id,
                position: DVec3::from_array(pos),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
            eve_destiny::SlimItemMeta {
                group_id: 340,
                category_id: 2,
                owner_id: char_id,
                char_id: None,
                corp_id: -1,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await;
    item_id
}

pub async fn drop_site_loot_into_space(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    char_id: CharacterId,
    pos: [f64; 3],
    type_id: TypeId,
    quantity: i64,
) -> i64 {
    drop_site_loot_into_space_core(
        &ctx.db,
        &ctx.systems,
        solar_system_id,
        char_id,
        pos,
        type_id,
        quantity,
    )
    .await
}

fn combat_dungeon_for_wh_class(wh_class: u8, variant: usize) -> (u32, &'static str, u8) {
    match wh_class {
        1 => (3707, "Perimeter Ambush Point", 3),
        2 => (3708, "Perimeter Camp", 3),
        3 => (3710, "Fortification Frontier Stronghold", 3),
        4 => (3713, "Outpost Frontier Stronghold", 3),
        5 => {
            if variant % 2 == 0 {
                (3704, "Core Citadel", 4)
            } else {
                (3716, "Core Garrison", 4)
            }
        }
        _ => (3716, "Core Garrison", 4),
    }
}

fn make_wh_pve_sig(
    sig_id: String,
    sys_id: SolarSystemId,
    sig_group: &str,
    type_name: String,
    pos: [f64; 3],
    now: u64,
    lifetime: u64,
) -> CosmicSignatureRecord {
    CosmicSignatureRecord {
        signature_id: sig_id,
        solar_system_id: sys_id,
        sig_group: sig_group.into(),
        type_name,
        position: pos,
        signal_strength: 0.0,
        wormhole_type_id: None,
        destination_system_id: None,
        created_unix: now,
        lifetime_seconds: lifetime,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        collapsed: false,
    }
}

/// Seeds special wormhole PvE sites for:
/// - Drifter systems (`14..=18`): zero PvE anomalies/signatures (all signatures are wormholes).
/// - C13 small-ship shattered systems (`13`, 25 `J000xxx`): higher-count C1-C3 sites with faster 4h respawn.
/// - Standard shattered systems (`1..=6` with `is_shattered`): sites of class `+/- 1`, plus one
///   `Shattered Debris Field` (all 15 ore types) and one `Shattered Ice Field` (all 8 ice types,
///   the only ice in J-space), each guarded by a single delayed Sleeper wave.
pub async fn seed_special_wormhole_pve_sites(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
) -> bool {
    let (wh_class, _is_thera, is_shattered, _) =
        system_wormhole_profile_sde(&ctx.sde, solar_system_id);
    if (14..=18).contains(&wh_class) {
        with_pve_exploration_reg(ctx, |reg| {
            reg.seeded_systems.insert(solar_system_id, true);
            reg.sites_by_system.entry(solar_system_id).or_default();
        });
        return true;
    }
    if !(wh_class == 13 || ((1..=6).contains(&wh_class) && is_shattered)) {
        return false;
    }

    let now = ctx.now_unix();
    let is_c13 = wh_class == 13;
    let lifetime = if is_c13 { 14_400 } else { 86_400 };
    let mut sites = Vec::new();
    let mut site_instance_ids: Vec<(String, ExplorationSiteId)> = Vec::new();
    let mut new_anoms = Vec::new();
    let mut new_sigs = Vec::new();
    let mut cans_to_insert = Vec::new();
    let mut resource_asteroids: Vec<(u32, i64, [f64; 3], f64)> = Vec::new();

    let combat_classes: Vec<u8> = if is_c13 {
        vec![1, 2, 3, 2, 3]
    } else {
        let min_c = wh_class.saturating_sub(1).max(1);
        let max_c = (wh_class + 1).min(6);
        (min_c..=max_c).collect()
    };

    for (idx, &site_c) in combat_classes.iter().enumerate() {
        let (did, default_name, waves) = combat_dungeon_for_wh_class(site_c, idx);
        let d_name = ctx
            .sde
            .dungeons
            .get(&did)
            .map(|d| d.name.trim().to_string())
            .unwrap_or_else(|| default_name.to_string());
        let site_id = format!("PVE-ANOM-{solar_system_id}-{}", idx + 1);
        let pos = [
            25_000_000_000.0 + (idx as f64) * 5_000_000_000.0,
            12_000_000_000.0,
            -8_000_000_000.0,
        ];
        let npc_ids = crate::services::pve_exploration_sites::spawn_sleeper_wave_in_system(
            ctx,
            solar_system_id,
            site_c,
            1,
            DVec3::from_array(pos),
        )
        .await;
        new_anoms.push(CosmicAnomalyRecord {
            anomaly_id: site_id.clone(),
            solar_system_id,
            anomaly_type: "Combat".into(),
            name: d_name.clone(),
            position: pos,
            signal_strength: 1.0,
            ore_types: Vec::new(),
            remaining_volume: 0.0,
            created_unix: now,
            lifetime_seconds: lifetime,
            cleared: false,
        });
        let wh_site_inst_id =
            ExplorationSiteId::new(ctx.db.with_mut_snapshot(|snap| snap.alloc_entity_id()));
        site_instance_ids.push((site_id.clone(), wh_site_inst_id));
        sites.push(PveSiteInstance {
            site_id,
            dungeon_id: did,
            dungeon_name: d_name,
            solar_system_id,
            site_category: "WormholeCombat".into(),
            wormhole_class: Some(site_c),
            position: pos,
            probe_required: false,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: waves,
            active_npc_ball_ids: npc_ids,
            npc_guards_spawned: true,
            created_unix: now,
            lifetime_seconds: lifetime,
            ..Default::default()
        });
    }

    let sig_classes: Vec<u8> = if is_c13 {
        vec![1, 2, 3, 2]
    } else {
        let min_c = wh_class.saturating_sub(1).max(1);
        let max_c = (wh_class + 1).min(6);
        vec![min_c, max_c]
    };

    for (s_idx, &site_c) in sig_classes.iter().enumerate() {
        let is_relic = s_idx % 2 == 1;
        let (did, fallback_name, grp, cat, can_tid) = if is_relic {
            (
                3722u32,
                "Forgotten Core Data Field",
                "Relic",
                "Relic",
                21_741u32,
            )
        } else {
            (
                3729u32,
                "Unsecured Frontier Enclave Relay",
                "Data",
                "Data",
                21_740u32,
            )
        };
        let sig_id = format!(
            "{}-{solar_system_id}-{}",
            if is_relic { "REL" } else { "DAT" },
            s_idx + 1
        );
        let name = ctx
            .sde
            .dungeons
            .get(&did)
            .map(|d| d.name.trim().to_string())
            .unwrap_or_else(|| fallback_name.to_string());
        let pos = [
            -32_000_000.0 - (s_idx as f64) * 4_500_000.0,
            28_000_000.0,
            -10_000_000.0 + (s_idx as f64) * 3_500_000.0,
        ];
        let (sig_site_inst_id, can_ids) = ctx.db.with_mut_snapshot(|snap| {
            let sid = ExplorationSiteId::new(snap.alloc_entity_id());
            let cids = (0..3).map(|_| snap.alloc_entity_id()).collect::<Vec<_>>();
            (sid, cids)
        });
        for &cid in &can_ids {
            cans_to_insert.push(InventoryItemRecord {
                item_id: cid,
                type_id: can_tid,
                owner_id: 1_000_125,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            });
        }
        let guard_ids = crate::services::pve_exploration_sites::spawn_sleeper_wave_in_system(
            ctx,
            solar_system_id,
            site_c,
            1,
            DVec3::from_array(pos),
        )
        .await;
        new_sigs.push(make_wh_pve_sig(
            sig_id.clone(),
            solar_system_id,
            grp,
            name.clone(),
            pos,
            now,
            lifetime,
        ));
        site_instance_ids.push((sig_id.clone(), sig_site_inst_id));
        sites.push(PveSiteInstance {
            site_id: sig_id,
            dungeon_id: did,
            dungeon_name: name,
            solar_system_id,
            site_category: cat.into(),
            wormhole_class: Some(site_c),
            position: pos,
            probe_required: true,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 1,
            active_npc_ball_ids: guard_ids,
            hackable_can_ids: can_ids,
            npc_guards_spawned: true,
            created_unix: now,
            lifetime_seconds: lifetime,
            ..Default::default()
        });
    }

    if !is_c13 && is_shattered {
        // 1. Shattered Debris Field (all 15 ore types, guarded by a single delayed Sleeper wave)
        let debris_id = format!("SHD-{solar_system_id}");
        let debris_pos = [68_000_000.0, 24_000_000.0, -32_000_000.0];
        let mut debris_vol = 0.0_f64;
        for (i, &ore_tid) in SHATTERED_DEBRIS_ORE_TYPES.iter().enumerate() {
            let (uvol, rad) = ctx
                .sde
                .types
                .get(&ore_tid)
                .map(|t| (t.volume.max(0.1), t.radius.max(80.0)))
                .unwrap_or((1.0, 100.0));
            let qty = 20_000_i64;
            debris_vol += (qty as f64) * uvol;
            let apos = [
                debris_pos[0] + ((i as f64) - 7.0) * 1_800.0,
                debris_pos[1] + ((i % 3) as f64) * 1_200.0,
                debris_pos[2],
            ];
            resource_asteroids.push((ore_tid, qty, apos, rad));
        }
        new_anoms.push(CosmicAnomalyRecord {
            anomaly_id: debris_id.clone(),
            solar_system_id,
            anomaly_type: "Ore".into(),
            name: "Shattered Debris Field".into(),
            position: debris_pos,
            signal_strength: 1.0,
            ore_types: SHATTERED_DEBRIS_ORE_TYPES.to_vec(),
            remaining_volume: debris_vol,
            created_unix: now,
            lifetime_seconds: lifetime,
            cleared: false,
        });
        let (debris_site_inst_id, ice_site_inst_id) = ctx.db.with_mut_snapshot(|snap| {
            (
                ExplorationSiteId::new(snap.alloc_entity_id()),
                ExplorationSiteId::new(snap.alloc_entity_id()),
            )
        });
        site_instance_ids.push((debris_id.clone(), debris_site_inst_id));
        sites.push(PveSiteInstance {
            site_id: debris_id,
            dungeon_id: 3790,
            dungeon_name: "Shattered Debris Field".into(),
            solar_system_id,
            site_category: "ShatteredDebris".into(),
            wormhole_class: Some(wh_class),
            position: debris_pos,
            probe_required: false,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 0,
            total_waves: 1,
            is_shattered_resource_site: true,
            delayed_wave_triggered: false,
            created_unix: now,
            lifetime_seconds: lifetime,
            ..Default::default()
        });

        // 2. Shattered Ice Field (all 8 ice types — the ONLY ice in J-space, guarded by a single delayed Sleeper wave)
        let ice_id = format!("SHI-{solar_system_id}");
        let ice_pos = [-74_000_000.0, -28_000_000.0, 42_000_000.0];
        let mut ice_vol = 0.0_f64;
        for (i, &ice_tid) in SHATTERED_ICE_TYPES.iter().enumerate() {
            let (uvol, rad) = ctx
                .sde
                .types
                .get(&ice_tid)
                .map(|t| (t.volume.max(1_000.0), t.radius.max(120.0)))
                .unwrap_or((1_000.0, 150.0));
            let qty = 500_i64;
            ice_vol += (qty as f64) * uvol;
            let apos = [
                ice_pos[0] + ((i as f64) - 3.5) * 2_200.0,
                ice_pos[1] + ((i % 2) as f64) * 1_500.0,
                ice_pos[2],
            ];
            resource_asteroids.push((ice_tid, qty, apos, rad));
        }
        new_anoms.push(CosmicAnomalyRecord {
            anomaly_id: ice_id.clone(),
            solar_system_id,
            anomaly_type: "IceBelt".into(),
            name: "Shattered Ice Field".into(),
            position: ice_pos,
            signal_strength: 1.0,
            ore_types: SHATTERED_ICE_TYPES.to_vec(),
            remaining_volume: ice_vol,
            created_unix: now,
            lifetime_seconds: lifetime,
            cleared: false,
        });
        site_instance_ids.push((ice_id.clone(), ice_site_inst_id));
        sites.push(PveSiteInstance {
            site_id: ice_id,
            dungeon_id: 3791,
            dungeon_name: "Shattered Ice Field".into(),
            solar_system_id,
            site_category: "ShatteredIce".into(),
            wormhole_class: Some(wh_class),
            position: ice_pos,
            probe_required: false,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 0,
            total_waves: 1,
            is_shattered_resource_site: true,
            delayed_wave_triggered: false,
            created_unix: now,
            lifetime_seconds: lifetime,
            ..Default::default()
        });
    }

    let (added_anoms, added_sigs, spawned_ast_balls) = ctx.db.with_mut_snapshot(|snap| {
        let mut added_a = Vec::new();
        let sys_anoms = snap.cosmic_anomalies.entry(solar_system_id).or_default();
        for a in new_anoms {
            if !sys_anoms.iter().any(|x| x.anomaly_id == a.anomaly_id) {
                added_a.push(a.clone());
                sys_anoms.push(a);
            }
        }
        let mut added_s = Vec::new();
        let sys_sigs = snap.cosmic_signatures.entry(solar_system_id).or_default();
        for s in new_sigs {
            if !sys_sigs.iter().any(|x| x.signature_id == s.signature_id) {
                added_s.push(s.clone());
                sys_sigs.push(s);
            }
        }
        for item in cans_to_insert {
            snap.items.insert(item.item_id, item);
        }
        let mut ast_balls = Vec::with_capacity(resource_asteroids.len());
        for (tid, qty, apos, rad) in resource_asteroids {
            let aid = snap.alloc_entity_id();
            snap.asteroids.insert(
                aid,
                AsteroidRecord {
                    asteroid_id: aid,
                    solar_system_id,
                    belt_id: 0,
                    type_id: tid,
                    quantity: qty,
                    initial_quantity: qty,
                    position: (apos[0], apos[1], apos[2]),
                    radius: rad,
                },
            );
            ast_balls.push((aid, tid, apos, rad));
        }
        (added_a, added_s, ast_balls)
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    for (aid, tid, apos, rad) in spawned_ast_balls {
        let uvol = ctx
            .sde
            .types
            .get(&tid)
            .map(|t| t.volume.max(0.1))
            .unwrap_or(1.0);
        let _ = sys_handle
            .spawn_asteroid(
                crate::system_actor::AsteroidState {
                    asteroid_id: aid,
                    belt_id: 0,
                    type_id: tid,
                    initial_units: 25_000,
                    remaining_units: 25_000,
                    unit_volume: uvol,
                    depleted: false,
                    respawn_at_tick: None,
                },
                Ball {
                    id: aid,
                    type_id: tid,
                    position: DVec3::from_array(apos),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 0.0,
                    mass: 100_000_000.0,
                    radius: rad,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                },
            )
            .await;
    }

    crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_anomaly_delta(
        ctx,
        solar_system_id,
        &added_anoms,
        &[],
    );
    crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_signature_delta(
        ctx,
        solar_system_id,
        &added_sigs,
        &[],
    );

    with_pve_exploration_reg(ctx, |reg| {
        reg.seeded_systems.insert(solar_system_id, true);
        let existing = reg.sites_by_system.entry(solar_system_id).or_default();
        for site in sites {
            if !existing.iter().any(|s| s.site_id == site.site_id) {
                existing.push(site);
            }
        }
        for (sid, inst_id) in site_instance_ids {
            reg.site_instance_ids.entry(sid).or_insert(inst_id);
        }
    });
    true
}

fn is_empire_sentry_faction(faction_id: u32) -> bool {
    matches!(faction_id, 500_001..=500_004 | 500_006..=500_008)
}

/// Resolves an empire faction ID from an NPC type name or Navy archetype when SDE `faction_id` is absent.
fn resolve_empire_faction_from_name(type_name: &str, archetype: &str) -> Option<u32> {
    if type_name.contains("Republic Fleet")
        || type_name.contains("Minmatar")
        || archetype == "MinmatarNavy"
    {
        Some(500_002)
    } else if type_name.contains("Imperial Navy")
        || type_name.contains("Amarr")
        || archetype == "AmarrNavy"
    {
        Some(500_003)
    } else if type_name.contains("Federal Navy")
        || type_name.contains("Gallente")
        || archetype == "GallenteNavy"
    {
        Some(500_004)
    } else if type_name.contains("Caldari")
        || type_name.contains("State Navy")
        || type_name.contains("State Police")
        || type_name.contains("State Peacekeeper")
        || archetype == "CaldariNavy"
    {
        Some(500_001)
    } else if type_name.contains("CONCORD") {
        Some(500_006)
    } else if type_name.contains("Ammatar") {
        Some(500_007)
    } else if type_name.contains("Khanid") {
        Some(500_008)
    } else {
        None
    }
}

/// Resolves a system's governing empire faction from station, constellation, or region SDE ownership.
fn resolve_system_empire_faction(ctx: &ServerContext, solar_system_id: u32) -> u32 {
    ctx.sde
        .solar_systems
        .get(&solar_system_id)
        .and_then(|sys| {
            sys.stations
                .iter()
                .find_map(|st| {
                    st.owner_faction_id
                        .filter(|&f| f > 0)
                        .map(|f| f as u32)
                        .filter(|&f| is_empire_sentry_faction(f))
                })
                .or_else(|| {
                    ctx.sde
                        .constellations
                        .get(&sys.constellation_id)
                        .and_then(|c| c.faction_id.filter(|&f| is_empire_sentry_faction(f)))
                })
                .or_else(|| {
                    ctx.sde
                        .regions
                        .get(&sys.region_id)
                        .and_then(|r| r.faction_id.filter(|&f| is_empire_sentry_faction(f)))
                })
        })
        .unwrap_or(500_001)
}

/// Builds `(Ball, CombatState, ExtendedNpcEntity)` for `spawn_specialized_npc_entity`.
pub fn build_specialized_npc_components(
    ctx: &ServerContext,
    solar_system_id: u32,
    ball_id: i64,
    npc_type_id: u32,
    archetype: &str,
    spawn_group_id: u32,
    pos: DVec3,
) -> (Ball, CombatState, ExtendedNpcEntity) {
    let type_name = ctx
        .sde
        .types
        .get(&npc_type_id)
        .map(|t| t.name.as_str())
        .unwrap_or("");
    let empire_faction_override = match archetype {
        "Navy"
        | "FactionPolice"
        | "Police"
        | "CaldariNavy"
        | "MinmatarNavy"
        | "AmarrNavy"
        | "GallenteNavy" => Some(
            resolve_empire_faction_from_name(type_name, archetype).unwrap_or(500_001),
        ),
        // Gate/station sentry types in EVE SDE omit `faction_id`; resolve empire sovereignty
        // rather than falling back to regional pirate factions.
        "EmpireSentry" => Some(
            resolve_empire_faction_from_name(type_name, archetype).unwrap_or_else(|| {
                if is_empire_sentry_faction(spawn_group_id) {
                    spawn_group_id
                } else {
                    resolve_system_empire_faction(ctx, solar_system_id)
                }
            }),
        ),
        _ => None,
    };
    crate::combat::npc_ai_weapons_loot::build_specialized_npc_components_with_empire_hint(
        ctx,
        solar_system_id,
        ball_id,
        npc_type_id,
        archetype,
        spawn_group_id,
        pos,
        empire_faction_override,
    )
}

#[cfg(test)]
#[path = "wormhole_pve_seeding_tests.rs"]
mod wormhole_pve_seeding_tests;

#[cfg(test)]
#[path = "wormhole_pve_seeding_edges_tests.rs"]
mod wormhole_pve_seeding_edges_tests;

