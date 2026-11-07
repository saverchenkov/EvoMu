//! Regression security & lifecycle tests for Round-7 Track TB P0 issues (`#428`, `#429`, `#431`, `#432`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_sde_registry, make_fighter_type,
};
use crate::services::sov_services::repair_service;
use eve_db::{
    service_roles, InventoryFlag, IskCents, PersistedCombatHp, SafeLogoffEntry,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::Arc;

fn make_r7_space_ctx_and_session() -> (ServerContext, SessionState, i64, i64, u32) {
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
        ],
    );
    rifter.base_price = 500_000.0;
    sde.types.insert(587, rifter);
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
async fn r7_tb_01_safe_logoff_disconnects_session_and_clears_immunity_for_future_pvp() {
    let (ctx, mut sess, char_id, ship_id, sys_id) = make_r7_space_ctx_and_session();
    let user_id = sess.user_id.unwrap_or(1);
    let now_unix = ctx.now_unix_secs();

    let (close_tx, mut close_rx) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(sess.session_id, close_tx);
    let _ = ctx.db.acquire_online_lock(user_id, sess.session_id, Some(char_id), true, now_unix);

    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(10_000.0, 0.0, 0.0),
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

    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.safe_logoffs.insert(
            char_id,
            SafeLogoffEntry {
                char_id,
                ship_id,
                solar_system_id: sys_id,
                started_unix: now_unix,
                complete_at_unix: now_unix + 30,
                completed: false,
            },
        );
    });

    ctx.advance_secs(30);
    crate::sp6_lifecycle::on_advance_secs(&ctx, now_unix, 30);

    // 1. Safe logoff completion actively disconnects the client session
    let closed = close_rx.try_recv().expect("safe logoff must disconnect session");
    assert_eq!(closed.0, "SafeLogoff");

    // 2. First immediate disconnect consumes completed entry once and removes ship cleanly
    let first_disc = crate::sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id).await;
    assert!(first_disc.is_none());
    crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        assert!(!st.safe_logoffs.contains_key(&char_id), "safe_logoffs must be cleared after consume");
    });

    // 3. Even if a completed entry was persisted (e.g. session did not drop), actions & login clear it
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.safe_logoffs.insert(
            char_id,
            SafeLogoffEntry {
                char_id,
                ship_id,
                solar_system_id: sys_id,
                started_unix: now_unix,
                complete_at_unix: now_unix + 30,
                completed: true,
            },
        );
    });
    assert!(!crate::sp6_lifecycle::maybe_abort_safe_logoff_on_action(&ctx, char_id, "ACTION"));
    crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        assert!(!st.safe_logoffs.contains_key(&char_id));
    });

    // 4. Pilot re-enters space in a ship under PvP aggression -> ungraceful disconnect leaves ship in space
    sys.spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(25_000.0, 0.0, 0.0),
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
    let pvp_disc = crate::sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id).await;
    assert!(
        pvp_disc.is_some(),
        "Pilot in space must NOT retain permanent instant-logoff immunity after prior safe logoff"
    );
    let _ = &mut sess;
}

#[tokio::test]
async fn r7_tb_02_disconnect_emergency_warp_and_reconcile_respect_bubbles_and_targeted_scram() {
    let (ctx, _sess, char_id, ship_id, sys_id) = make_r7_space_ctx_and_session();
    let sys = ctx.systems.get_or_spawn(sys_id);
    let pos = DVec3::new(50_000.0, 0.0, 0.0);

    // 1. Non-nullified ship inside a warp disruption bubble -> emergency warp blocked on disconnect
    sys.spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: pos,
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
    sys.set_combat_state(ship_id, CombatState::default()).await.unwrap();
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.add_warp_bubble(900_991, pos, 20_000.0);
    }

    let bubbled_disc = crate::sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id)
        .await
        .expect("disconnected ship persisted");
    assert!(
        bubbled_disc.emergency_warp_dest.is_none(),
        "Warp disruption bubble must block disconnect emergency warp"
    );

    // 2. Ship already in emergency warp alignment caught in bubble -> tackle reconciliation stops warp
    {
        let mut bp = sys.ballpark.write().unwrap();
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.is_emergency_warping = true;
        }
        if let Some(b) = bp.balls.get_mut(&ship_id) {
            b.mode = MovementMode::new_warp(pos, pos + DVec3::new(1e9, 0.0, 0.0), 3.0);
        }
        let mut cycles = Vec::new();
        crate::system_actor::reconcile_tackle_and_bubbles(
            &mut bp,
            &mut cycles,
            &HashMap::new(),
            &HashMap::new(),
            &None,
            None,
        );
        assert!(
            !matches!(bp.balls[&ship_id].mode, MovementMode::Warp { .. }),
            "reconcile_tackle_and_bubbles must abort emergency warp inside a bubble"
        );
    }

    // 3. Interdiction nullifier bypasses bubble alone, but CANNOT bypass targeted warp scrambler
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.disconnected_ships.remove(&char_id);
    });
    sys.set_combat_state(
        ship_id,
        CombatState {
            is_interdiction_nullified: true,
            warp_scramble_strength: 0,
            warp_core_strength: 1,
            ..CombatState::default()
        },
    )
    .await
    .unwrap();
    let nullified_disc = crate::sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id)
        .await
        .unwrap();
    assert!(
        nullified_disc.emergency_warp_dest.is_some(),
        "Nullified ship in bubble without targeted scram can emergency warp"
    );

    // Now apply targeted warp scrambler (strength 2 >= core 1) while still interdiction-nullified
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.disconnected_ships.remove(&char_id);
    });
    sys.set_combat_state(
        ship_id,
        CombatState {
            is_interdiction_nullified: true,
            warp_scramble_strength: 2,
            warp_core_strength: 1,
            ..CombatState::default()
        },
    )
    .await
    .unwrap();
    let scrammed_null_disc = crate::sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id)
        .await
        .unwrap();
    assert!(
        scrammed_null_disc.emergency_warp_dest.is_none(),
        "Interdiction nullifier must NOT bypass targeted warp scrambler on disconnect"
    );
}

#[tokio::test]
async fn r7_tb_04_repair_in_space_blocked_and_station_repair_charges_isk_and_clears_ship_combat_hp() {
    let (ctx, mut sess, char_id, ship_id, sys_id) = make_r7_space_ctx_and_session();
    let station_id = 60_003_760_i64;
    let kw = HashMap::new();

    // Seed combat damage in ship_combat_hp + live ballpark (with 0.0 dynamic_attributes) while in space
    let initial_isk = 5_000_000_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.balance = IskCents::from_isk(initial_isk);
            c.station_id = None;
            c.structure_id = None;
        }
        snap.sp6_lifecycle.ship_combat_hp.insert(
            ship_id,
            PersistedCombatHp {
                shield_hp: 50.0,
                armor_hp: 50.0,
                hull_hp: 50.0,
            },
        );
    });
    sess.station_id = None;
    sess.structure_id = None;

    // 1. Repair RPCs in untethered open space must fail
    assert_eq!(
        repair_service::handle_repair_get_quotes(&ctx, &sess, &[PyRep::Integer(ship_id)])
            .unwrap_err()
            .code,
        "NotInStation"
    );
    assert_eq!(
        repair_service::handle_repair_items_in_location(&ctx, &sess, false, &[PyRep::Integer(ship_id)])
            .unwrap_err()
            .code,
        "NotInStation"
    );

    // 2. Dock in NPC station -> quote & station repair must charge ISK (> 0) for ship_combat_hp damage
    sess.station_id = Some(station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = Some(station_id);
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = station_id;
            sh.flag = InventoryFlag::Hangar;
        }
    });

    let quotes = repair_service::handle_repair_get_quotes(&ctx, &sess, &[PyRep::Integer(ship_id)])
        .unwrap();
    let PyRep::Dict(q_map) = quotes else { panic!("expected Dict") };
    let PyRep::List(rows) = &q_map[0].1 else { panic!("expected List") };
    assert!(rows[0].dict_get("damage").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    assert!(rows[0].dict_get("totalRepairCost").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    let rep = repair_service::handle_repair_items_in_location(
        &ctx,
        &sess,
        false,
        &[PyRep::Integer(ship_id)],
    )
    .unwrap();
    let paid = rep.dict_get("totalCostISK").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(paid > 0.0, "Station repair of combat-damaged ship must charge positive ISK");
    let bal_after = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    assert!(bal_after < IskCents::from_isk(initial_isk));
    ctx.db.with_snapshot(|s| {
        assert!(
            !s.sp6_lifecycle.ship_combat_hp.contains_key(&ship_id),
            "ship_combat_hp must be cleared on repair so undock does not restore stale damage"
        );
    });

    // 3. Undock after repair -> ship spawns with full HP (not overwritten by stale ship_combat_hp)
    crate::services::space_destiny::record_session_change_timer(&ctx, char_id, 1, sys_id, 0, "dock");
    ctx.advance_secs(20);
    let _ = crate::services::station_structure::handle(&ctx, &mut sess, "stationSvc", "Undock", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let cs_undock = ctx.systems.get_or_spawn(sys_id).get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_undock.shield_hp, cs_undock.max_shield_hp);
    assert_eq!(cs_undock.armor_hp, cs_undock.max_armor_hp);
    assert_eq!(cs_undock.structure_hp, cs_undock.max_structure_hp);
    let _ = UpwellStructureRecord::default();
}

#[test]
fn r7_tb_05_machonet_and_cluster_admin_rpcs_reject_regular_players_and_work_for_gm() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut player = SessionState::new(71_001);
    player.user_id = Some(500);
    player.character_id = Some(90_000_500);
    player.role = service_roles::ROLE_PLAYER;

    let mut gm = SessionState::new(71_002);
    gm.user_id = Some(1);
    gm.character_id = Some(90_000_001);
    gm.role = service_roles::ROLE_GML | service_roles::ROLE_ADMIN;

    let target_svc = "cluster".to_string();
    let restricted: &[(&str, &[PyRep])] = &[
        ("AddClusterNode", &[PyRep::Integer(3), PyRep::String("127.0.0.1:26003".into())]),
        ("ReinforceNode", &[PyRep::Integer(30_000_142), PyRep::Integer(3)]),
        ("RebalanceNodes", &[]),
        ("SetServiceState", &[PyRep::String(target_svc.clone()), PyRep::Bool(false)]),
        ("AdminBroadcast", &[PyRep::String("Fake shutdown".into())]),
        ("RemoveClusterNode", &[PyRep::Integer(2)]),
    ];

    for svc in ["cluster", "machoNet"] {
        for (method, call_args) in restricted {
            let method_name = method.to_string();
            let err = crate::services::macho_cluster_mgmt::handle(
                &ctx,
                &player,
                svc,
                &method_name,
                call_args,
                &kw,
            )
            .expect("handled")
            .unwrap_err();
            assert_eq!(err.code, "InsufficientRole", "{svc}.{method} must reject player");
        }
    }

    // Read-only machoNet queries remain accessible to authenticated players
    assert!(crate::services::macho_cluster_mgmt::handle(
        &ctx, &player, "machoNet", "GetConnectedClusterMode", &[], &kw
    )
    .unwrap()
    .is_ok());

    // GM succeeds across all privileged cluster & machoNet operations
    assert_eq!(
        crate::services::macho_cluster_mgmt::handle(
            &ctx,
            &gm,
            "cluster",
            "AddClusterNode",
            &[PyRep::Integer(3), PyRep::String("127.0.0.1:26003".into())],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(3)
    );
    assert_eq!(
        crate::services::macho_cluster_mgmt::handle(
            &ctx,
            &gm,
            "cluster",
            "ReinforceNode",
            &[PyRep::Integer(30_000_142), PyRep::Integer(3)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(3)
    );
    assert!(crate::services::macho_cluster_mgmt::handle(
        &ctx, &gm, "cluster", "RebalanceNodes", &[], &kw
    )
    .unwrap()
    .is_ok());
    assert_eq!(
        crate::services::macho_cluster_mgmt::handle(
            &ctx,
            &gm,
            "cluster",
            "SetServiceState",
            &[PyRep::String(target_svc), PyRep::Bool(false)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        crate::services::macho_cluster_mgmt::handle(
            &ctx,
            &gm,
            "cluster",
            "AdminBroadcast",
            &[PyRep::String("Scheduled cluster maintenance in 5m".into())],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        crate::services::macho_cluster_mgmt::handle(
            &ctx,
            &gm,
            "cluster",
            "RemoveClusterNode",
            &[PyRep::Integer(3)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
}
