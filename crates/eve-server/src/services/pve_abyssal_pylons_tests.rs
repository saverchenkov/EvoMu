//! Unit and integration tests for `pve_abyssal_pylons`.

use super::*;
use crate::router::ServerContext;
use crate::services::pve_abyssal_hazards::{
    AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, MULTIBODY_TRACKING_PYLON_TYPE_ID,
};
use crate::services::pve_special_sites_abyssal::{
    with_abyssal_state, AbyssalPocketSession, AbyssalRoomRecord, AbyssalWeatherKind,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, Ballpark, MovementMode};
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn fitted_ship_ball_builder_tracking_beneficiary_and_pod_respawn() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let char_id: i64 = 90_050_101;
    let ship_id: i64 = 1_050_100_001;
    let pod_id: i64 = 1_050_100_002;
    let station_id: i64 = 60_003_760;

    db.provision_gm_account("pylon_pilot", "pw", char_id, "PylonPilot");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = 30_000_142;
            ch.home_station_id = station_id;
            ch.active_ship_id = pod_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            pod_id,
            InventoryItemRecord {
                item_id: pod_id,
                type_id: 670,
                owner_id: char_id,
                location_id: 32_000_001,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    if let Some(t) = sde.types.get(&587) {
        assert!(is_tracking_pylon_beneficiary(t));
    }

    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db.clone());
    let ball = build_fitted_ship_space_ball(&ctx, char_id, ship_id, 587, DVec3::new(10.0, 20.0, 30.0));
    assert_eq!(ball.id, ship_id);
    assert!(ball.mass > 0.0);

    db.with_mut_snapshot(|snap| {
        let _ = destroy_pod_and_respawn_faction_corvette_in_snap(
            snap,
            Some(&sde),
            char_id,
            32_000_001,
            [0.0; 3],
            1_775_650_000,
        );
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.station_id, Some(station_id));
        assert!(ch.active_ship_id > 0);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abyssal_automated_pylons_pulse_probe_prune_and_ballpark_step() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let char_id: i64 = 90_050_102;
    let ship_id: i64 = 1_050_100_201;
    let ab_sys: SolarSystemId = 32_000_001;

    db.provision_gm_account("pylon_ab", "pw", char_id, "PylonAbyss");
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db.clone());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_660_000)));

    let make_room = |idx: u8| AbyssalRoomRecord {
        room_index: idx,
        dungeon_id: 2100 + idx as u32,
        dungeon_name: "Abyss Room".into(),
        npc_ball_ids: if idx == 0 { vec![60_000_101] } else { Vec::new() },
        cache_item_id: 0,
        cleared: false,
        gate_unlocked_at_sec: None,
    };
    with_abyssal_state(&ctx, |st| {
        st.pockets.insert(
            char_id,
            AbyssalPocketSession {
                pocket_id: 1,
                character_id: char_id,
                ship_id,
                origin_solar_system_id: 30_000_142,
                abyssal_solar_system_id: ab_sys,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 56131,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 0,
                rooms: [make_room(0), make_room(1), make_room(2)],
                started_at_sec: 1_775_660_000,
                collapse_at_sec: 1_775_661_200,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 0,
                fleet_ship_ids: vec![ship_id],
                ship_room: HashMap::from([(ship_id, 0)]),
                max_ships: 1,
            },
        );
    });

    let ab_handle = ctx.systems.get_or_spawn(ab_sys);
    for (bid, tid, pos) in [
        (ship_id, 587_u32, DVec3::new(1_000.0, 0.0, 0.0)),
        (1_898_000_010_i64, AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, DVec3::new(2_000.0, 0.0, 0.0)),
        (1_898_000_011_i64, MULTIBODY_TRACKING_PYLON_TYPE_ID, DVec3::new(2_500.0, 0.0, 0.0)),
    ] {
        let _ = ab_handle
            .spawn_ball(Ball {
                id: bid,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    let (sup_count, trk_count) = pulse_abyssal_automated_pylons(&ctx, ab_sys).await;
    assert!(sup_count + trk_count >= 1);
    prune_out_of_bounds_abyssal_probes(&ctx, ab_sys);

    // Exercise helper parameter tables across pylon variants
    assert!(suppressor_radius_m(Some(&sde), AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, 250.0) > 0.0);
    assert!(
        suppressor_radius_m(
            Some(&sde),
            crate::services::pve_abyssal_hazards::AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID,
            25_000.0
        ) > 0.0
    );
    assert!(
        suppressor_radius_m(
            None,
            crate::services::pve_abyssal_hazards::AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID,
            40_000.0
        ) >= 1_000.0
    );
    assert!(
        suppressor_radius_m(
            Some(&sde),
            crate::services::pve_abyssal_hazards::AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID,
            40_000.0
        ) > 0.0
    );
    let sup_dmg = suppressor_pulse_damage(Some(&sde), AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID);
    assert!(sup_dmg.em + sup_dmg.thermal + sup_dmg.kinetic + sup_dmg.explosive > 0.0);
    let (_, trk_mult) = tracking_pylon_radius_and_mult(
        Some(&sde),
        crate::services::pve_abyssal_hazards::MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID,
        250.0,
    );
    assert!(trk_mult > 1.0);

    // Full ballpark step covering suppressor pulse + kill, tracking pylon decloak & buff,
    // out-of-bounds probe removal, ship hull respawn as Capsule, pod destruction cleanup,
    // and spatial hazard cloud entry/exit modifiers.
    let mut custom_sde = (*sde).clone();
    if let Some(sample) = custom_sde.types.values().next().cloned() {
        let mut ship_t = sample.clone();
        ship_t.type_id = 587;
        ship_t.category_id = 6;
        ship_t.group_id = 25;
        custom_sde.types.insert(587, ship_t);

        let mut npc_t = sample;
        npc_t.type_id = 52_338;
        npc_t.category_id = 11;
        npc_t.group_id = 1997;
        npc_t.default_attributes.insert(37, 250.0);
        custom_sde.types.insert(52_338, npc_t);
    }
    assert!(is_suppressor_target_type(
        custom_sde.types.get(&52_338).unwrap()
    ));

    let mut bp = Ballpark::new(ab_sys);
    // Player ship inside tracking pylon & tachyon cloud, initially cloaked
    bp.add_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(
        ship_id,
        eve_destiny::CombatState {
            shield_hp: 500.0,
            max_shield_hp: 500.0,
            armor_hp: 500.0,
            max_armor_hp: 500.0,
            structure_hp: 500.0,
            max_structure_hp: 500.0,
            signature_radius: 40.0,
            capacitor_recharge_time_ms: 10_000.0,
            is_cloaked: true,
            tracking_speed_multiplier: 1.0,
            ..Default::default()
        },
    );
    bp.set_ball_slim_meta(
        ship_id,
        eve_destiny::SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: char_id,
            char_id: Some(char_id),
            is_cloaked: true,
            ..Default::default()
        },
    );

    // Tracking pylon + Automata suppressor + Tachyon cloud
    bp.add_ball(Ball {
        id: 1_899_000_110,
        type_id: AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID,
        position: DVec3::new(1_200.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 250.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.add_ball(Ball {
        id: 1_899_000_111,
        type_id: MULTIBODY_TRACKING_PYLON_TYPE_ID,
        position: DVec3::new(1_100.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 250.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.add_ball(Ball {
        id: 1_898_000_112,
        type_id: crate::services::pve_abyssal_hazards::TACHYON_CLOUD_TYPE_ID,
        position: DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 15_000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });

    // Low-HP Abyssal NPC within suppressor range so suppressor pulse finishes it off
    let npc_id: i64 = 60_000_101;
    bp.add_ball(Ball {
        id: npc_id,
        type_id: 52_338,
        position: DVec3::new(1_250.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: 500_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(
        npc_id,
        eve_destiny::CombatState {
            shield_hp: 0.0,
            max_shield_hp: 10.0,
            armor_hp: 0.0,
            max_armor_hp: 10.0,
            structure_hp: 1.0,
            max_structure_hp: 100.0,
            structure_resonance: [1.0; 4],
            ..Default::default()
        },
    );

    // Out-of-bounds ship (collapses to Capsule) + out-of-bounds pod (destroyed & cleaned up)
    let oob_ship_id: i64 = 1_050_100_998;
    bp.add_ball(Ball {
        id: oob_ship_id,
        type_id: 587,
        position: DVec3::new(100_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: 500_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(
        oob_ship_id,
        eve_destiny::CombatState {
            shield_hp: 0.0,
            max_shield_hp: 100.0,
            armor_hp: 0.0,
            max_armor_hp: 100.0,
            structure_hp: 1.0,
            max_structure_hp: 100.0,
            structure_resonance: [1.0; 4],
            ..Default::default()
        },
    );

    let oob_pod_id: i64 = 1_050_100_999;
    bp.add_ball(Ball {
        id: oob_pod_id,
        type_id: 670,
        position: DVec3::new(-100_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 15.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(
        oob_pod_id,
        eve_destiny::CombatState {
            shield_hp: 0.0,
            max_shield_hp: 10.0,
            armor_hp: 0.0,
            max_armor_hp: 10.0,
            structure_hp: 1.0,
            max_structure_hp: 100.0,
            structure_resonance: [1.0; 4],
            ..Default::default()
        },
    );

    bp.set_ball_slim_meta(
        oob_pod_id,
        eve_destiny::SlimItemMeta {
            group_id: 29,
            category_id: 6,
            owner_id: char_id,
            char_id: Some(char_id),
            ..Default::default()
        },
    );

    let killed = step_abyssal_hazards_and_pylons_in_ballpark(
        &mut bp,
        Some(&custom_sde),
        Some(&db),
        ab_sys,
    );
    assert!(killed.contains(&npc_id));
    assert!(!bp.combat_states[&ship_id].is_cloaked);
    assert!(bp.combat_states[&ship_id].tracking_speed_multiplier > 1.0);
    assert!(bp.balls.values().any(|b| b.type_id == 670));
    assert!(bp.balls.values().any(|b| b.type_id == 25));
}

#[tokio::test]
async fn arch_585_abyssal_boundary_enforces_invuln_preserves_events_and_step_hook_death_pipeline() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let char_id: i64 = 90_050_585;
    let ship_id: i64 = 1_050_585_001;
    let ab_sys: SolarSystemId = 32_000_001;

    db.provision_gm_account("abyss_585", "pw", char_id, "Abyss585");
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db.clone());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_660_000)));

    let make_room = |idx: u8| AbyssalRoomRecord {
        room_index: idx,
        dungeon_id: 2100 + idx as u32,
        dungeon_name: "Abyss Room".into(),
        npc_ball_ids: Vec::new(),
        cache_item_id: 0,
        cleared: true,
        gate_unlocked_at_sec: Some(1_775_660_000),
    };
    with_abyssal_state(&ctx, |st| {
        st.pockets.insert(
            char_id,
            AbyssalPocketSession {
                pocket_id: 585,
                character_id: char_id,
                ship_id,
                origin_solar_system_id: 30_000_142,
                abyssal_solar_system_id: ab_sys,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 56131,
                tier: 1,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 0,
                rooms: [make_room(0), make_room(1), make_room(2)],
                started_at_sec: 1_775_660_000,
                collapse_at_sec: 1_775_661_200,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 0,
                fleet_ship_ids: vec![ship_id],
                ship_room: HashMap::from([(ship_id, 0)]),
                max_ships: 1,
            },
        );
    });

    let start_isk = db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: ab_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, [100_000.0, 0.0, 0.0]);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = ab_sys;
            ch.station_id = None;
            ch.balance.to_wire_f64()
        } else {
            0.0
        }
    });

    let now_u = ctx.now_unix();
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.insurance_contracts.insert(
            ship_id,
            crate::services::combat_crimewatch::InsuranceContractState {
                ship_id,
                ship_type_id: 587,
                owner_id: char_id,
                level_name: "Platinum".into(),
                level_fraction: 1.0,
                cost_isk: 100_000.0,
                payout_isk: 500_000.0,
                start_unix: now_u.saturating_sub(10),
                end_unix: now_u + 86_400,
            },
        );
    });

    // Invulnerability guard check on boundary tick in Ballpark
    let mut bp = Ballpark::new(ab_sys);
    bp.add_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(100_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: 500_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(
        ship_id,
        eve_destiny::CombatState {
            structure_hp: 1.0,
            max_structure_hp: 100.0,
            structure_resonance: [1.0; 4],
            warp_exit_invulnerable_remaining_ticks: 3,
            ..Default::default()
        },
    );
    let shielded = step_abyssal_hazards_and_pylons_in_ballpark_for_tick(
        &mut bp,
        Some(&sde),
        Some(&db),
        ab_sys,
    );
    assert!(shielded.is_empty());
    assert!((bp.combat_states[&ship_id].structure_hp - 1.0).abs() < 1e-6);

    // Live system step destroys low-HP ship on Abyssal boundary and routes through execute_death_pipeline_sync
    let sys = ctx.systems.get_or_spawn(ab_sys);
    let _ = sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 500_000.0,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys
        .set_combat_state(
            ship_id,
            eve_destiny::CombatState {
                structure_hp: 1.0,
                max_structure_hp: 100.0,
                structure_resonance: [1.0; 4],
                ..Default::default()
            },
        )
        .await;

    let tick_res = sys.step_manual(1.2).await.unwrap();
    let has_term_explosion = tick_res
        .observer_events
        .values()
        .flatten()
        .chain(tick_res.events.iter())
        .any(|ev| matches!(ev, &eve_destiny::DestinyEvent::TerminalExplosion { ball_id, .. } if ball_id == ship_id))
        || sys
            .drain_observer_updates(ship_id)
            .await
            .map(|(_, evs, _)| evs)
            .unwrap_or_default()
            .iter()
            .any(|ev| matches!(ev, &eve_destiny::DestinyEvent::TerminalExplosion { ball_id, .. } if ball_id == ship_id));
    assert!(has_term_explosion, "TerminalExplosion must be emitted on Abyssal boundary destruction");

    db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ship_id));
        let ch = snap.characters.get(&char_id).unwrap();
        let cap_id = ch.active_ship_id;
        assert_ne!(cap_id, ship_id);
        assert_eq!(snap.items[&cap_id].type_id, 670);
        assert!(ch.balance.to_wire_f64() > start_isk);
        assert!(snap.killmails.values().any(|k| k.victim_character_id == char_id));
        assert!(
            snap.abyssal_state.boundary_ticks.contains_key(&cap_id),
            "escalated tick must carry over to ejected capsule"
        );
    });
}
