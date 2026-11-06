//! Extra branch-coverage and mutation-killing unit tests for `pve_abyssal_lifecycle.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::{with_mut_combat_runtime, DroneSpaceState};
use crate::services::pve_abyssal_lifecycle::{
    collapse_abyssal_pocket_and_pod_pilot_at, evacuate_abyssal_pockets_for_downtime,
    evacuate_void_space_session_sync, random_offset_around_trace_origin, reconcile_abyssal_login,
    sweep_expired_void_space_sessions_sync,
};
use crate::services::pve_special_sites::{
    with_pve_special_reg, AbyssalPocketSession, AbyssalRoomRecord, AbyssalTraceRecord,
    AbyssalWeatherKind, VoidSpaceSessionRecord,
};
use crate::sp6_lifecycle::{register_abyssal_session, with_mut_sp6_state};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        let mut sys08 = sde.solar_systems.values().next().cloned().expect("seed");
        sys08.solar_system_id = 30_999_080;
        sys08.security_status = 0.80;
        sde.solar_systems.insert(30_999_080, sys08);
        Arc::new(sde)
    }))
}

fn build_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_779_000_000)));
    ctx
}

fn make_room(idx: u8, npcs: Vec<i64>) -> AbyssalRoomRecord {
    AbyssalRoomRecord {
        room_index: idx,
        dungeon_id: 50_100 + u32::from(idx),
        dungeon_name: format!("Room {idx}"),
        npc_ball_ids: npcs,
        cache_item_id: 1_896_000_000 + i64::from(idx),
        cleared: false,
        gate_unlocked_at_sec: None,
    }
}

fn seed_pilot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: SolarSystemId,
) {
    ctx.db
        .provision_gm_account(&format!("p_{char_id}"), "pw", char_id, "AbyssPilot");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
}

#[tokio::test]
async fn random_offset_and_coop_vs_solo_pocket_collapse_branches() {
    let ctx = build_ctx();
    for _ in 0..8 {
        let p = random_offset_around_trace_origin(&ctx, [1_000.0, -2_000.0, 3_000.0]);
        let dx = (p[0] - 1_000.0).abs();
        let dy = (p[1] - (-2_000.0)).abs();
        let dz = (p[2] - 3_000.0).abs();
        assert!((500.0..=5_000.0).contains(&dx));
        assert!((500.0..=5_000.0).contains(&dy));
        assert!((500.0..=5_000.0).contains(&dz));
    }

    let origin_sys: SolarSystemId = 30_000_142;
    let abyss_sys: SolarSystemId = 32_000_010;
    let char1 = 920_101_i64;
    let ship1 = 1_200_101_i64;
    let char2 = 920_102_i64;
    let ship2 = 1_200_102_i64;
    let char_pod = 920_103_i64;
    let pod_ship = 1_200_103_i64;

    seed_pilot(&ctx, char1, ship1, 24690, abyss_sys);
    seed_pilot(&ctx, char2, ship2, 24690, abyss_sys);
    seed_pilot(&ctx, char_pod, pod_ship, 670, abyss_sys); // already in capsule (type 670)

    let pid = 800_i64;
    let trace_id = 1_950_001_i64;
    let cache1_id = 1_896_000_000 + pid * 10 + 1;
    let cache_loot_id = 1_990_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.zarzakh_gate_locks
            .insert(char1, (50_001_234, 1_779_001_000));
        snap.items.insert(
            cache1_id,
            InventoryItemRecord {
                item_id: cache1_id,
                type_id: 47965,
                location_id: abyss_sys as i64,
                ..Default::default()
            },
        );
        snap.items.insert(
            cache_loot_id,
            InventoryItemRecord {
                item_id: cache_loot_id,
                type_id: 34,
                location_id: cache1_id,
                quantity: 5,
                ..Default::default()
            },
        );
    });

    with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_traces.insert(
            trace_id,
            AbyssalTraceRecord {
                trace_ball_id: trace_id,
                char_id: char1,
                filament_type_id: 56131,
                origin_solar_system_id: origin_sys,
                position: [0.0; 3],
                created_unix: 1_778_999_000,
                expires_at_sec: 1_779_001_000,
                max_ships: 3,
                entered_char_ids: vec![char1, char2],
                pocket_id: Some(pid),
                fleet_id: Some(1),
            },
        );
        for (cid, sid) in [(char1, ship1), (char2, ship2)] {
            reg.abyssal_pockets.insert(
                cid,
                AbyssalPocketSession {
                    pocket_id: pid,
                    character_id: cid,
                    ship_id: sid,
                    origin_solar_system_id: origin_sys,
                    abyssal_solar_system_id: abyss_sys,
                    origin_position: [0.0; 3],
                    grid_offset: [0.0; 3],
                    filament_type_id: 56131,
                    tier: 2,
                    weather: AbyssalWeatherKind::Dark,
                    weather_penalty_pct: 20.0,
                    weather_bonus_pct: 25.0,
                    npc_difficulty_mult: 1.0,
                    current_room: 1,
                    rooms: [
                        make_room(1, vec![1_960_001]),
                        make_room(2, vec![]),
                        make_room(3, vec![]),
                    ],
                    started_at_sec: 1_778_999_000,
                    collapse_at_sec: 1_779_000_000,
                    saved_ship_attrs: HashMap::new(),
                    completed: false,
                    collapsed_destroyed: false,
                    trace_ball_id: trace_id,
                    fleet_ship_ids: vec![ship1, ship2],
                    ship_room: HashMap::new(),
                    max_ships: 3,
                },
            );
        }
    });

    // 1. Collapse char1 while char2 is still alive in the same pocket_id (any_other_alive == true):
    // trace and cache items are preserved for char2, zarzakh gate lock is cleared for char1
    let home1 = collapse_abyssal_pocket_and_pod_pilot_at(&ctx, char1, ship1, abyss_sys, 1_779_000_000);
    assert!(home1 > 0);
    with_pve_special_reg(&ctx, |reg| {
        assert!(reg.abyssal_traces.contains_key(&trace_id));
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.zarzakh_gate_locks.contains_key(&char1));
        assert!(snap.items.contains_key(&cache1_id));
        assert!(snap.items.contains_key(&cache_loot_id));
    });

    // 2. Collapse char2 (last pilot in pocket -> any_other_alive == false):
    // trace and cache + cache loot are now purged!
    let _ = collapse_abyssal_pocket_and_pod_pilot_at(&ctx, char2, ship2, abyss_sys, 1_779_000_000);
    with_pve_special_reg(&ctx, |reg| {
        assert!(!reg.abyssal_traces.contains_key(&trace_id));
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&cache1_id));
        assert!(!snap.items.contains_key(&cache_loot_id));
    });

    // 3. Collapse char_pod (no entry in reg.abyssal_pockets, ship_type_id == 670):
    // records only 1 pod killmail (no separate ship killmail for type 670)
    let kms_before = ctx.db.with_snapshot(|s| s.killmails.len());
    let _ = collapse_abyssal_pocket_and_pod_pilot_at(
        &ctx,
        char_pod,
        pod_ship,
        abyss_sys,
        1_779_000_000,
    );
    let kms_after = ctx.db.with_snapshot(|s| s.killmails.len());
    assert_eq!(kms_after - kms_before, 1);
}

#[tokio::test]
async fn downtime_evacuation_drones_cleanup_and_highsec_tier_suspect_flag() {
    let ctx = build_ctx();
    let highsec_origin: SolarSystemId = 30_999_080; // 0.80 security -> T5 triggers suspect flag
    let abyss_sys: SolarSystemId = 32_000_020;
    let char_id = 920_201_i64;
    let ship_id = 1_200_201_i64;
    let active_drone_ball = 1_250_001_i64;
    let active_drone_item = 1_250_002_i64;
    let orphan_drone_ball = 1_250_003_i64;
    let orphan_drone_item = 1_250_004_i64;

    let drone_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 18)
        .map(|t| t.type_id)
        .unwrap_or(2454);

    seed_pilot(&ctx, char_id, ship_id, 24690, abyss_sys);
    ctx.db.with_mut_snapshot(|snap| {
        for did in [active_drone_item, orphan_drone_item] {
            snap.items.insert(
                did,
                InventoryItemRecord {
                    item_id: did,
                    type_id: drone_type_id,
                    owner_id: if did == active_drone_item {
                        char_id
                    } else {
                        999_999
                    },
                    location_id: abyss_sys as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    with_mut_combat_runtime(&ctx, |rt| {
        for (ball_id, item_id, owner_id, parent_ship) in [
            (active_drone_ball, active_drone_item, char_id, ship_id),
            (orphan_drone_ball, orphan_drone_item, 888_888, 777_777),
        ] {
            rt.active_drones_in_space.insert(
                ball_id,
                DroneSpaceState {
                    ball_id,
                    drone_item_id: item_id,
                    type_id: drone_type_id,
                    group_id: 100,
                    owner_char_id: owner_id,
                    original_owner_char_id: owner_id,
                    parent_ship_id: parent_ship,
                    solar_system_id: abyss_sys,
                    target_id: None,
                    activity_state: 0,
                    assist_char_id: None,
                    guard_char_id: None,
                    is_abandoned: false,
                    is_sentry: false,
                    bandwidth_used: 10.0,
                    max_velocity: 3_000.0,
                    optimal_range: 5_000.0,
                    damage_vector: eve_destiny::DamageVector::default(),
                    shield_repair_amount: 0.0,
                    armor_repair_amount: 0.0,
                    hull_repair_amount: 0.0,
                    web_speed_factor: 1.0,
                    neut_amount: 0.0,
                    ecm_strength: 0.0,
                    tp_sig_bonus_pct: 0.0,
                },
            );
        }
    });

    // Tier 5 pocket originating in highsec -> triggers suspect flag on downtime evacuation
    with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_pockets.insert(
            char_id,
            AbyssalPocketSession {
                pocket_id: 901,
                character_id: char_id,
                ship_id,
                origin_solar_system_id: highsec_origin,
                abyssal_solar_system_id: abyss_sys,
                origin_position: [10_000.0, 0.0, 0.0],
                grid_offset: [0.0; 3],
                filament_type_id: 56138,
                tier: 5,
                weather: AbyssalWeatherKind::Exotic,
                weather_penalty_pct: 50.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 1,
                rooms: [
                    make_room(1, vec![]),
                    make_room(2, vec![]),
                    make_room(3, vec![]),
                ],
                started_at_sec: 1_778_999_500,
                collapse_at_sec: 1_779_000_500,
                saved_ship_attrs: HashMap::from([(37, 250.0)]),
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 1_970_001,
                fleet_ship_ids: vec![ship_id],
                ship_room: HashMap::new(),
                max_ships: 1,
            },
        );
    });

    assert_eq!(evacuate_abyssal_pockets_for_downtime(&ctx), 1);

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&active_drone_item));
        assert!(!snap.items.contains_key(&orphan_drone_item));
        assert_eq!(
            snap.items
                .get(&ship_id)
                .and_then(|s| s.dynamic_attributes.get(&37))
                .copied(),
            Some(250.0)
        );
    });
    let is_suspect = ctx
        .crimewatch
        .write()
        .unwrap()
        .get_or_create(char_id)
        .is_suspect(ctx.now_unix());
    assert!(is_suspect);
}

#[tokio::test]
async fn void_space_and_reconcile_abyssal_login_sp6_and_completed_branches() {
    let ctx = build_ctx();
    let origin_sys: SolarSystemId = 30_000_142;
    let abyss_sys: SolarSystemId = 32_000_050;
    let char_id = 920_301_i64;
    let ship_id = 1_200_301_i64;
    seed_pilot(&ctx, char_id, ship_id, 24690, abyss_sys);

    // 1. SP6-only abyssal session (no pve_special_reg entry, but sp6_state has session) stays in cur_sys
    register_abyssal_session(
        &ctx,
        char_id,
        ship_id,
        abyss_sys,
        origin_sys,
        1_778_999_000,
        1_779_001_000,
    );
    assert_eq!(
        reconcile_abyssal_login(&ctx, char_id, abyss_sys, ship_id),
        abyss_sys
    );
    with_mut_sp6_state(&ctx.db, |st, _| {
        st.abyssal_sessions.remove(&char_id);
    });

    // 2. Already completed pocket in reg.abyssal_pockets reconciles via Evacuated branch & restores attrs
    with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_pockets.insert(
            char_id,
            AbyssalPocketSession {
                pocket_id: 902,
                character_id: char_id,
                ship_id,
                origin_solar_system_id: origin_sys,
                abyssal_solar_system_id: abyss_sys,
                origin_position: [5_000.0, 0.0, 0.0],
                grid_offset: [0.0; 3],
                filament_type_id: 56131,
                tier: 1,
                weather: AbyssalWeatherKind::Gamma,
                weather_penalty_pct: 30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 3,
                rooms: [
                    make_room(1, vec![]),
                    make_room(2, vec![]),
                    make_room(3, vec![]),
                ],
                started_at_sec: 1_778_999_000,
                collapse_at_sec: 1_779_001_000,
                saved_ship_attrs: HashMap::from([(263, 1_500.0)]),
                completed: true,
                collapsed_destroyed: false,
                trace_ball_id: 0,
                fleet_ship_ids: vec![ship_id],
                ship_room: HashMap::new(),
                max_ships: 1,
            },
        );
    });
    assert_eq!(
        reconcile_abyssal_login(&ctx, char_id, abyss_sys, ship_id),
        origin_sys
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].solar_system_id, origin_sys);
        assert_eq!(snap.items[&ship_id].location_id, origin_sys as i64);
        assert_eq!(
            snap.items[&ship_id].dynamic_attributes.get(&263).copied(),
            Some(1_500.0)
        );
    });

    // 3. Expired pocket on login triggers CollapsedDestroy
    seed_pilot(&ctx, char_id, ship_id, 24690, abyss_sys);
    with_pve_special_reg(&ctx, |reg| {
        let p = reg.abyssal_pockets.get_mut(&char_id).unwrap();
        p.completed = false;
        p.collapsed_destroyed = false;
        p.collapse_at_sec = 1_779_000_000; // exact boundary now >= collapse_at_sec
    });
    let pod_home = reconcile_abyssal_login(&ctx, char_id, abyss_sys, ship_id);
    assert!(pod_home > 0);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ship_id));
    });

    // 4. evacuate_void_space_session_sync when character is NOT in VOID_SPACE_SYSTEM_ID returns current sys
    let vs = VoidSpaceSessionRecord {
        char_id,
        ship_id,
        origin_solar_system_id: origin_sys,
        origin_position: [0.0; 3],
        target_id: 777,
        entered_unix: 100,
        expires_at_sec: 200,
    };
    assert_eq!(
        evacuate_void_space_session_sync(&ctx, char_id, &vs, 300),
        pod_home
    );

    // 5. sweep_expired_void_space_sessions_sync ignores expires_at_sec == 0
    with_pve_special_reg(&ctx, |reg| {
        reg.void_space_sessions.insert(
            char_id,
            VoidSpaceSessionRecord {
                expires_at_sec: 0,
                ..vs
            },
        );
    });
    sweep_expired_void_space_sessions_sync(&ctx, 1_779_000_000);
    with_pve_special_reg(&ctx, |reg| {
        assert!(reg.void_space_sessions.contains_key(&char_id));
    });
}
