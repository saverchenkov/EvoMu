//! Unit tests for `pve_abyssal_lifecycle`.

use super::*;
use crate::router::ServerContext;
use crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID;
use crate::services::pve_special_sites::{
    with_abyssal_state, AbyssalPocketSession, AbyssalRoomRecord, AbyssalTraceRecord,
    AbyssalWeatherKind, VoidSpaceSessionRecord,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn build_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_300_000)));
    ctx
}

fn sample_room(idx: u8, npc_ids: Vec<i64>) -> AbyssalRoomRecord {
    AbyssalRoomRecord {
        room_index: idx,
        dungeon_id: 50_000 + u32::from(idx),
        dungeon_name: format!("Abyssal Room {idx}"),
        npc_ball_ids: npc_ids,
        cache_item_id: 1_910_000 + i64::from(idx),
        cleared: false,
        gate_unlocked_at_sec: None,
    }
}

fn seed_pilot_and_ship(ctx: &ServerContext, char_id: i64, ship_id: i64, sys_id: SolarSystemId) {
    ctx.db.provision_gm_account(&format!("abyss_{char_id}"), "pw", char_id, "AbyssRunner");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 24690, // Vexor cruiser
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_abyssal_pocket_collapse_podding_and_downtime_evacuation() {
    let ctx = build_ctx();
    let origin_sys: SolarSystemId = 30_000_142;
    let abyss_sys1: SolarSystemId = 32_000_001;
    let abyss_sys2: SolarSystemId = 32_000_002;
    let char1: i64 = 90_200_001;
    let ship1: i64 = 1_100_001;
    let char2: i64 = 90_200_002;
    let ship2: i64 = 1_100_002;

    let off = random_offset_around_trace_origin(&ctx, [10_000.0, 20_000.0, -5_000.0]);
    assert!((off[0] - 10_000.0).abs() >= 500.0);

    // No active pockets -> downtime evacuation returns 0
    assert_eq!(evacuate_abyssal_pockets_for_downtime(&ctx), 0);

    seed_pilot_and_ship(&ctx, char1, ship1, abyss_sys1);
    seed_pilot_and_ship(&ctx, char2, ship2, abyss_sys2);

    let trace1 = 1_920_001_i64;
    let trace2 = 1_920_002_i64;
    with_abyssal_state(&ctx, |st| {
        st.traces.insert(
            trace1,
            AbyssalTraceRecord {
                trace_ball_id: trace1,
                char_id: char1,
                filament_type_id: 56131,
                origin_solar_system_id: origin_sys,
                position: [1_000.0, 0.0, 0.0],
                created_unix: 1_776_299_000,
                expires_at_sec: 1_776_300_200,
                max_ships: 1,
                entered_char_ids: vec![char1],
                pocket_id: Some(7001),
                fleet_id: None,
            },
        );
        st.pockets.insert(
            char1,
            AbyssalPocketSession {
                pocket_id: 7001,
                character_id: char1,
                ship_id: ship1,
                origin_solar_system_id: origin_sys,
                abyssal_solar_system_id: abyss_sys1,
                origin_position: [1_000.0, 0.0, 0.0],
                grid_offset: [0.0; 3],
                filament_type_id: 56131,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 1,
                rooms: [
                    sample_room(1, vec![1_930_001]),
                    sample_room(2, vec![]),
                    sample_room(3, vec![]),
                ],
                started_at_sec: 1_776_299_000,
                collapse_at_sec: 1_776_299_900,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: trace1,
                fleet_ship_ids: vec![ship1],
                ship_room: HashMap::from([(ship1, 1)]),
                max_ships: 1,
            },
        );
        st.pockets.insert(
            char2,
            AbyssalPocketSession {
                pocket_id: 7002,
                character_id: char2,
                ship_id: ship2,
                origin_solar_system_id: origin_sys,
                abyssal_solar_system_id: abyss_sys2,
                origin_position: [50_000.0, 0.0, 0.0],
                grid_offset: [0.0; 3],
                filament_type_id: 56131,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 2,
                rooms: [
                    sample_room(1, vec![]),
                    sample_room(2, vec![1_930_002]),
                    sample_room(3, vec![]),
                ],
                started_at_sec: 1_776_299_500,
                collapse_at_sec: 1_776_300_700,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: trace2,
                fleet_ship_ids: vec![ship2],
                ship_room: HashMap::from([(ship2, 2)]),
                max_ships: 1,
            },
        );
    });

    // 1. Collapse char1 pocket on 20-minute expiry -> pods pilot back to home station system
    let dest1 = collapse_abyssal_pocket_and_pod_pilot(&ctx, char1, ship1, abyss_sys1);
    assert!(dest1 > 0);
    with_abyssal_state(&ctx, |st| {
        assert!(st.pockets[&char1].collapsed_destroyed);
        assert!(!st.traces.contains_key(&trace1));
    });

    // 2. Evacuate char2 safely at 11:00 UTC downtime -> keeps ship intact & moves to origin system
    let evac_count = evacuate_abyssal_pockets_for_downtime(&ctx);
    assert_eq!(evac_count, 1);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char2].solar_system_id, origin_sys);
        assert!(snap.items.contains_key(&ship2));
    });
}

#[test]
fn test_void_space_evacuation_and_reconcile_abyssal_login_paths() {
    let ctx = build_ctx();
    let origin_sys: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_200_010;
    let ship_id: i64 = 1_100_010;
    seed_pilot_and_ship(&ctx, char_id, ship_id, origin_sys);

    // K-space login returns unchanged
    assert_eq!(
        reconcile_abyssal_login(&ctx, char_id, origin_sys, ship_id),
        origin_sys
    );

    // Void Space active session stays in VOID_SPACE_SYSTEM_ID until expiry
    with_abyssal_state(&ctx, |st| {
        st.void_space_sessions.insert(
            char_id,
            VoidSpaceSessionRecord {
                char_id,
                ship_id,
                origin_solar_system_id: origin_sys,
                origin_position: [5_000.0, 5_000.0, 5_000.0],
                target_id: 99_001,
                entered_unix: 1_776_299_500,
                expires_at_sec: 1_776_300_500,
                nebula_graphic_id: None,
            },
        );
    });
    assert_eq!(
        reconcile_abyssal_login(&ctx, char_id, VOID_SPACE_SYSTEM_ID, ship_id),
        VOID_SPACE_SYSTEM_ID
    );

    // Expired Void Space session evacuates back to origin_sys on login
    with_abyssal_state(&ctx, |st| {
        if let Some(vs) = st.void_space_sessions.get_mut(&char_id) {
            vs.expires_at_sec = 1_776_299_999;
        }
    });
    assert_eq!(
        reconcile_abyssal_login(&ctx, char_id, VOID_SPACE_SYSTEM_ID, ship_id),
        origin_sys
    );

    // Sweep expired Void Space sessions batch helper
    with_abyssal_state(&ctx, |st| {
        st.void_space_sessions.insert(
            char_id,
            VoidSpaceSessionRecord {
                char_id,
                ship_id,
                origin_solar_system_id: origin_sys,
                origin_position: [2_000.0, 0.0, 0.0],
                target_id: 99_002,
                entered_unix: 1_776_299_000,
                expires_at_sec: 1_776_299_500,
                nebula_graphic_id: None,
            },
        );
    });
    sweep_expired_void_space_sessions_sync(&ctx, 1_776_300_000);
    with_abyssal_state(&ctx, |st| {
        assert!(!st.void_space_sessions.contains_key(&char_id));
    });

    // Missing pocket record fallback in 32_000_055 evacuates safely to fallback home system
    let fallback_sys = reconcile_abyssal_login(&ctx, char_id, 32_000_055, ship_id);
    assert!(fallback_sys > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn evacuate_downtime_with_traces_drones_highsec_suspect_and_void_space_in_void_char() {
    let ctx = build_ctx();
    let char_id = 90_000_088_i64;
    let ship_id = 95_000_088_i64;
    let origin_sys = 30_000_142_u32;
    let ab_sys = 32_000_011_u32;
    let trace_id = 1_897_555_001_i64;
    let now = ctx.now_unix();
    let drone_tid = ctx.sde.types.values().find(|t| t.category_id == 18).map(|t| t.type_id).unwrap_or(2454);

    seed_pilot_and_ship(&ctx, char_id, ship_id, VOID_SPACE_SYSTEM_ID);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(96_555_001, InventoryItemRecord {
            item_id: 96_555_001, type_id: drone_tid, owner_id: char_id, location_id: ab_sys as i64, quantity: 1, ..Default::default()
        });
        snap.items.insert(96_555_002, InventoryItemRecord {
            item_id: 96_555_002, type_id: drone_tid, owner_id: char_id + 1, location_id: ab_sys as i64, quantity: 1, ..Default::default()
        });
    });

    // 1) evacuate_void_space_session_sync when ch.solar_system_id == VOID_SPACE_SYSTEM_ID
    let vs = VoidSpaceSessionRecord {
        char_id,
        ship_id,
        origin_solar_system_id: origin_sys,
        origin_position: [5_000.0, 0.0, 0.0],
        target_id: 77_001,
        entered_unix: now.saturating_sub(1_000),
        expires_at_sec: now.saturating_sub(1),
        ..Default::default()
    };
    assert_eq!(evacuate_void_space_session_sync(&ctx, char_id, &vs, now), origin_sys);

    // 2) evacuate_abyssal_pockets_for_downtime with trace, active drones, orphaned drones, and T4 highsec suspect
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        for (bid, did, oid, sid) in [
            (96_555_101, 96_555_102, char_id, ship_id),
            (96_555_103, 96_555_104, char_id + 1, ship_id + 1),
        ] {
            rt.active_drones_in_space.insert(bid, crate::services::combat_crimewatch::DroneSpaceState {
                ball_id: bid,
                drone_item_id: did,
                type_id: drone_tid,
                group_id: 100,
                owner_char_id: oid,
                original_owner_char_id: oid,
                parent_ship_id: sid,
                solar_system_id: ab_sys,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 400.0,
                optimal_range: 5_000.0,
                damage_vector: Default::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 1.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            });
        }
    });

    crate::services::pve_special_sites::with_abyssal_state(&ctx, |reg| {
        reg.traces.insert(trace_id, AbyssalTraceRecord {
            trace_ball_id: trace_id,
            char_id,
            filament_type_id: 47_762,
            origin_solar_system_id: origin_sys,
            position: [1_000.0, 0.0, 0.0],
            created_unix: now,
            expires_at_sec: now + 600,
            max_ships: 1,
            entered_char_ids: vec![char_id],
            pocket_id: Some(77),
            fleet_id: None,
        });
        reg.pockets.insert(char_id, AbyssalPocketSession {
            pocket_id: 77,
            trace_ball_id: trace_id,
            character_id: char_id,
            ship_id,
            fleet_ship_ids: vec![ship_id],
            origin_solar_system_id: origin_sys,
            origin_position: [1_000.0, 0.0, 0.0],
            grid_offset: [0.0; 3],
            abyssal_solar_system_id: ab_sys,
            filament_type_id: 47_762,
            tier: 4,
            weather: AbyssalWeatherKind::Electrical,
            weather_penalty_pct: 30.0,
            weather_bonus_pct: 50.0,
            npc_difficulty_mult: 1.0,
            started_at_sec: now,
            collapse_at_sec: now + 1_200,
            current_room: 1,
            completed: false,
            collapsed_destroyed: false,
            ship_room: Default::default(),
            max_ships: 1,
            rooms: [sample_room(1, vec![1_900_000_001]), sample_room(2, vec![]), sample_room(3, vec![])],
        });
    });
    assert_eq!(evacuate_abyssal_pockets_for_downtime(&ctx), 1);
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&96_555_001) && !s.items.contains_key(&96_555_002)));
}

#[cfg(test)]
#[path = "pve_abyssal_lifecycle_extra_unit_tests.rs"]
mod pve_abyssal_lifecycle_extra_unit_tests;
