//! Regression security & mechanics tests for Round-7 Track TB P1A (`#430`, `#433`, `#434`).

#![forbid(unsafe_code)]

use crate::combat::crimewatch::CrimeWatchDirector;
use crate::combat::npc_ai::{handle_npc_ai_ball_destructions, NpcAiState, NpcCombatEntity, NpcSpawnManager};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_sde_registry, make_fighter_type,
};
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime, DroneSpaceState};
use crate::sp6_lifecycle::{
    reconnect_character_in_space, reconnect_character_in_space_sync, with_mut_sp6_state,
};
use eve_db::{DisconnectedShipEntry, InventoryFlag};
use eve_destiny::{Ball, CombatState, DVec3, DamageVector, MovementMode};
use eve_machonet::SessionState;
use std::sync::Arc;

pub(super) fn make_p1a_ctx_and_session() -> (ServerContext, SessionState, i64, i64, u32) {
    let mut sde = make_fighter_sde_registry();
    let mut rifter = make_fighter_type(
        587,
        25,
        6,
        27_289.0,
        vec![
            (eve_sde::attr_ids::HP_STRUCT, 400.0),
            (eve_sde::attr_ids::ARMOR_HP, 350.0),
            (eve_sde::attr_ids::SHIELD_CAPACITY, 450.0),
            (eve_sde::attr_ids::DRONE_CAPACITY, 25.0),
        ],
    );
    rifter.base_price = 500_000.0;
    sde.types.insert(587, rifter);
    let drone_type = make_fighter_type(2456, 100, 18, 25.0, vec![]);
    sde.types.insert(2456, drone_type);
    let mod_type = make_fighter_type(434, 46, 7, 5.0, vec![(1223, 50.0)]);
    sde.types.insert(434, mod_type);
    let (base_ctx, mut sess, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), base_ctx.db.clone());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 587;
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
            c.structure_id = None;
            c.solar_system_id = sys_id;
            c.active_ship_id = ship_id;
        }
    });
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    (ctx, sess, char_id, ship_id, sys_id)
}

#[tokio::test]
async fn r7_tb_03_reconnect_return_warp_blocked_when_scrambled_or_bubbled() {
    let (ctx, _sess, char_id, ship_id, sys_id) = make_p1a_ctx_and_session();
    let sys = ctx.systems.get_or_spawn(sys_id);
    let orig_pos = DVec3::new(10_000.0, 0.0, 0.0);
    let current_pos = DVec3::new(50_000.0, 0.0, 0.0);

    let now = ctx.now_unix();
    let insert_disc = |st: &mut crate::sp6_lifecycle::Sp6PersistedState| {
        st.disconnected_ships.insert(
            char_id,
            DisconnectedShipEntry {
                char_id,
                ship_id,
                ship_type_id: 587,
                solar_system_id: sys_id,
                disconnected_at_unix: now,
                remove_at_unix: now + 600,
                emergency_warp_dest: None,
                last_position: [current_pos.x, current_pos.y, current_pos.z],
                removed_from_space: false,
                pre_warp_position: Some([orig_pos.x, orig_pos.y, orig_pos.z]),
            },
        );
    };
    with_mut_sp6_state(&ctx.db, |st, _snap| insert_disc(st));

    // 1. Targeted warp scrambler active -> reconnect must NOT force ship into warp
    let mut scrammed_cs = CombatState::default();
    scrammed_cs.warp_scramble_strength = 2;
    sys.spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 587,
            position: current_pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        scrammed_cs,
    )
    .await
    .unwrap();

    reconnect_character_in_space_sync(&ctx, char_id).expect("reconnect");
    {
        let bp = sys.ballpark.read().unwrap();
        assert!(
            !matches!(bp.balls[&ship_id].mode, MovementMode::Warp { .. }),
            "Scrambled ship must not enter return warp on reconnect"
        );
    }

    // 2. Inside warp disruption bubble without targeted scram -> must NOT enter return warp
    with_mut_sp6_state(&ctx.db, |st, _snap| insert_disc(st));
    sys.set_combat_state(ship_id, CombatState::default()).await.unwrap();
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.add_warp_bubble(910_001, current_pos, 20_000.0);
    }
    reconnect_character_in_space(&ctx, char_id).await.expect("reconnect async");
    {
        let bp = sys.ballpark.read().unwrap();
        assert!(
            !matches!(bp.balls[&ship_id].mode, MovementMode::Warp { .. }),
            "Bubbled ship must not enter return warp on reconnect"
        );
    }

    // 3. Clean unbubbled/unscrambled reconnect > 10km -> initiates return warp
    with_mut_sp6_state(&ctx.db, |st, _snap| insert_disc(st));
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.warp_bubble_mgr.bubbles.remove(&910_001);
    }
    reconnect_character_in_space(&ctx, char_id).await.expect("reconnect clean");
    {
        let bp = sys.ballpark.read().unwrap();
        assert!(
            matches!(bp.balls[&ship_id].mode, MovementMode::Warp { .. }),
            "Clean ship > 10km from pre-disconnect spot should return-warp on reconnect"
        );
    }
}

#[tokio::test]
async fn r7_tb_06_npc_ai_destroying_player_ship_or_drone_runs_death_pipeline_and_cleanup() {
    let (ctx, _sess, char_id, ship_id, sys_id) = make_p1a_ctx_and_session();
    let sys = ctx.systems.get_or_spawn(sys_id);

    // 1. NpcSpawnManager lethal volley destroys fragile player ship -> triggers death pipeline (capsule + wreck)
    sys.spawn_ball_with_combat(
        Ball {
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
        },
        CombatState {
            shield_hp: 0.0,
            max_shield_hp: 100.0,
            armor_hp: 0.0,
            max_armor_hp: 100.0,
            structure_hp: 5.0,
            max_structure_hp: 100.0,
            ..CombatState::default()
        },
    )
    .await
    .unwrap();

    let npc_id = 700_001_i64;
    sys.spawn_ball_with_combat(
        Ball {
            id: npc_id,
            type_id: 10001,
            position: DVec3::new(1_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 400.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    )
    .await
    .unwrap();

    let mut mgr = NpcSpawnManager::new();
    mgr.entities.insert(
        npc_id,
        NpcCombatEntity {
            ball_id: npc_id,
            type_id: 10001,
            faction_id: 500_010,
            state: NpcAiState::Attack,
            spawn_group_id: 1,
            spawn_pos: DVec3::new(1_500.0, 0.0, 0.0),
            target_id: Some(ship_id),
            bounty_isk: 5_000.0,
            optimal_range: 10_000.0,
            orbit_range: 1_000.0,
            base_dps: 800.0,
            damage_vector: DamageVector::new(200.0, 200.0, 200.0, 200.0),
            last_attack_tick: 0,
            has_shield_rep: false,
            has_armor_rep: false,
        },
    );

    mgr.step_ai_sync(&ctx, sys_id, 10, 1.0);

    let (ship_still_exists, active_ship) = ctx
        .db
        .with_snapshot(|s| (s.items.contains_key(&ship_id), s.characters[&char_id].active_ship_id));
    assert!(!ship_still_exists, "Destroyed ship item must be removed by death pipeline");
    assert_ne!(active_ship, ship_id, "Pilot must be ejected into capsule when NPC kills ship");

    // 2. Player drone destroyed in space -> removed from active_drones_in_space & snap.items
    let drone_id = 880_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            drone_id,
            make_fighter_item(drone_id, 2456, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            drone_id,
            DroneSpaceState {
                ball_id: drone_id,
                drone_item_id: drone_id,
                type_id: 2456,
                group_id: 100,
                owner_char_id: char_id,
                original_owner_char_id: char_id,
                parent_ship_id: active_ship,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 1_500.0,
                optimal_range: 1_500.0,
                damage_vector: DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 1.0,
                neut_amount: 0.0,
                tp_sig_bonus_pct: 0.0,
                ecm_strength: 0.0,
            },
        );
    });
    handle_npc_ai_ball_destructions(
        &ctx,
        sys_id,
        vec![(
            npc_id,
            drone_id,
            Ball {
                id: drone_id,
                type_id: 2456,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 1_500.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 5.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )],
    );
    assert!(with_combat_runtime(&ctx, |rt| !rt.active_drones_in_space.contains_key(&drone_id)));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&drone_id)));
}

#[tokio::test]
async fn r7_tb_07_step_concord_validates_active_non_capsule_ship_and_despawns_concord_ball() {
    let (ctx, _sess, criminal_id, ship_id, sys_id) = make_p1a_ctx_and_session();
    let victim_id = 90_000_002_i64;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let now = 2_000_000_u64;

    // 1. Stale/docked or ejected criminal must NOT trigger CONCORD destruction on stale ship
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&criminal_id) {
            c.station_id = Some(60_003_760);
        }
    });
    let mut cw = CrimeWatchDirector::new();
    cw.handle_combat_aggression(criminal_id, victim_id, ship_id, sys_id, 0.9, false, now);
    let reports_docked = cw.step_concord(&ctx, now + 30, 99).await;
    assert!(reports_docked.is_empty(), "Docked criminal must not trigger CONCORD ship execution");

    // 2. Undocked in active non-capsule ship -> executes criminal AND despawns transient CONCORD ball
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&criminal_id) {
            c.station_id = None;
            c.solar_system_id = sys_id;
            c.active_ship_id = ship_id;
        }
    });
    sys.spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(5_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .unwrap();

    cw.handle_combat_aggression(criminal_id, victim_id, ship_id, sys_id, 0.9, false, now + 40);
    let reports_live = cw.step_concord(&ctx, now + 80, 123).await;
    assert_eq!(reports_live.len(), 1);
    assert!(reports_live[0].death_result.is_ok());
    let concord_ball_id = reports_live[0].concord_ball_id;
    assert!(
        sys.get_ball_snapshot(concord_ball_id).await.unwrap().is_some(),
        "Transient CONCORD Commander ball must remain on grid immediately after execution"
    );
    cw.step_concord(
        &ctx,
        now + 80 + crate::combat::crimewatch::CONCORD_ON_GRID_DURATION_SECS,
        124,
    )
    .await;
    assert!(
        sys.get_ball_snapshot(concord_ball_id).await.unwrap().is_none(),
        "Transient CONCORD Commander ball must despawn once its on-grid execution window elapses"
    );
}
