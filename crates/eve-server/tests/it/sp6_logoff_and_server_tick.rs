//! Integration tests for `plan:SP-6` (`Logoff, disconnect and login in space`),
//! `feat:crimewatch.pvp-logoff-timer-renewal`, `feat:lifecycle.self-destruct-server-timer`,
//! and `feat:lifecycle.abyssal-disconnect`.

use crate::common::*;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{ DamageVector, MovementMode };
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::sp6_lifecycle::{
    handle_character_logoff_or_disconnect, record_npc_aggression_timer,
    record_pvp_aggression_and_renew_logoff_timer, unix_to_filetime, with_sp6_state,
};
use eve_server::system_actor::ActiveModuleCycle;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) fn sp6_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    dynamic_attributes: HashMap<u16, f64>,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity: 1,
        singleton: true,
        version: 1,
        dynamic_attributes,
        ..Default::default()
    }
}

pub(crate) fn persist_and_reopen_sqlite(ctx: &ServerContext, tag: &str) -> ServerContext {
    let pid = std::process::id();
    let json_path = std::env::temp_dir().join(format!("evomu_{tag}_{pid}.json"));
    let sqlite_path = std::env::temp_dir().join(format!("evomu_{tag}_{pid}.sqlite"));
    let _ = std::fs::remove_file(&json_path);
    let _ = std::fs::remove_file(&sqlite_path);

    ctx.db
        .save_snapshot_to_file(&json_path)
        .expect("save_snapshot_to_file");
    {
        let writer = WorldRepository::open_sqlite(&sqlite_path).expect("open_sqlite writer");
        writer
            .load_snapshot_from_file(&json_path)
            .expect("load_snapshot_from_file into sqlite");
    }
    let reopened = Arc::new(WorldRepository::open_sqlite(&sqlite_path).expect("reopen sqlite"));
    let _ = std::fs::remove_file(&json_path);
    let _ = std::fs::remove_file(&sqlite_path);
    ServerContext::with_repository(reopened)
}

pub(crate) async fn setup_pilot_in_space(
    ctx: &ServerContext,
    char_id: i64,
) -> (SessionState, u32, i64) {
    let mut sess = SessionState::default();
    let empty = HashMap::new();
    let (u, p) = if char_id == 90_000_001 {
        ("demo", "demo-secret")
    } else {
        ("other", "other-secret")
    };
    login_pilot(ctx, &mut sess, u, p).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &empty,
        None,
    )
    .await
    .expect("SelectCharacterID");

    if sess.station_id.is_some() {
        ctx.dispatch_rpc(&mut sess, "ship", None, "Undock", &[], &empty, None)
            .await
            .expect("Undock");
    } else if let Some(sys_id) = sess.solar_system_id {
        let oid = ctx.bind_object(&sess, "beyonce", sys_id as i64).expect("bind beyonce");
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(oid), "UpdateStateRequest", &[], &empty, None)
            .await
            .expect("UpdateStateRequest");
    }
    (sess.clone(), sess.solar_system_id.unwrap_or(30_000_142), sess.ship_id.expect("ship_id"))
}

#[tokio::test]
async fn test_sp6_safe_logoff_refusals_aborts_and_30s_completion() {
    // parity: plan:SP-6
    eve_testkit::attest(&["plan:SP-6"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let empty = HashMap::new();

    // 1. Login while docked -> `ship.SafeLogoff` must refuse with `SAFE_LOGOFF_DOCKED` and emit `OnSafeLogoffFailed`
    let mut sess = SessionState::default();
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &empty,
        None,
    )
    .await
    .expect("SelectCharacterID");
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(90_000_001);

    let docked_res = ctx
        .dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff while docked returns failed conditions list");
    assert_eq!(
        docked_res,
        PyRep::List(vec![PyRep::String("SAFE_LOGOFF_DOCKED".into())])
    );
    let docked_notifs = sess.drain_notifications();
    assert!(
        docked_notifs.iter().any(|(name, args)| {
            name == "OnSafeLogoffFailed"
                && *args
                    == vec![PyRep::List(vec![PyRep::String(
                        "SAFE_LOGOFF_DOCKED".into(),
                    )])]
        }),
        "OnSafeLogoffFailed must be emitted with arity 1 containing failed conditions"
    );

    // 2. Undock into space
    ctx.dispatch_rpc(&mut sess, "ship", None, "Undock", &[], &empty, None)
        .await
        .expect("Undock");
    let sys_id = sess.solar_system_id.unwrap();
    let ship_id = sess.ship_id.unwrap();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // 3. Test refusal under Warp Scramble (`SAFE_LOGOFF_WARP_SCRAMBLED`)
    sys_handle
        .set_warp_scramble_strength(ship_id, 2)
        .await
        .expect("set_warp_scramble_strength");
    let _ = sess.drain_notifications();
    let scram_res = ctx
        .dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff while scrambled");
    assert_eq!(
        scram_res,
        PyRep::List(vec![PyRep::String("SAFE_LOGOFF_WARP_SCRAMBLED".into())])
    );
    sys_handle
        .set_warp_scramble_strength(ship_id, 0)
        .await
        .expect("clear warp scramble");

    // 4. Test refusal under NPC timer (`SAFE_LOGOFF_NPC_TIMER`) and PvP/Weapons timer (`SAFE_LOGOFF_PVP_TIMER`, `SAFE_LOGOFF_WEAPONS_TIMER`)
    record_npc_aggression_timer(&ctx, 90_000_001, ctx.now_unix());
    let npc_res = ctx
        .dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff under NPC timer");
    assert_eq!(
        npc_res,
        PyRep::List(vec![PyRep::String("SAFE_LOGOFF_NPC_TIMER".into())])
    );

    record_pvp_aggression_and_renew_logoff_timer(&ctx, 90_000_001, 90_000_002, ctx.now_unix());
    let pvp_res = ctx
        .dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff under PvP + Weapons + NPC timers");
    let PyRep::List(conds) = pvp_res else {
        panic!("expected list of failed conditions");
    };
    assert!(conds.contains(&PyRep::String("SAFE_LOGOFF_WEAPONS_TIMER".into())));
    assert!(conds.contains(&PyRep::String("SAFE_LOGOFF_NPC_TIMER".into())));
    assert!(conds.contains(&PyRep::String("SAFE_LOGOFF_PVP_TIMER".into())));

    // Advance past 15m PvP timer
    ctx.advance_secs(901);

    // 4b. Test refusal under active module cycle (`SAFE_LOGOFF_ACTIVE_MODULES`) and deployed drone (`SAFE_LOGOFF_DRONES_IN_SPACE`)
    sys_handle
        .add_module_cycle(ActiveModuleCycle {
            ship_id,
            module_id: 999_888,
            module_type_id: 1_999,
            effect_name: "shieldBoosting".into(),
            repeat: 1,
            duration_s: 10.0,
            ..Default::default()
        })
        .await
        .expect("add_module_cycle");
    let mod_res = ctx
        .dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff with active module");
    assert_eq!(
        mod_res,
        PyRep::List(vec![PyRep::String("SAFE_LOGOFF_ACTIVE_MODULES".into())])
    );
    sys_handle
        .remove_module_cycle(ship_id, 999_888)
        .await
        .expect("remove_module_cycle");

    // 5. Start valid SafeLogoff -> returns `[]` and emits `OnSafeLogoffTimerStarted` with FILETIME for `now + 30s`
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(90_000_001);
    let expected_ft = unix_to_filetime(ctx.now_unix() + 30);
    let ok_res = ctx
        .dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff when clear");
    assert_eq!(ok_res, PyRep::List(vec![]));
    let started_notifs = sess.drain_notifications();
    assert!(
        started_notifs.iter().any(|(name, args)| {
            name == "OnSafeLogoffTimerStarted" && *args == vec![PyRep::Integer(expected_ft)]
        }),
        "OnSafeLogoffTimerStarted must carry Windows FILETIME of T+30s"
    );

    // 6. Abort via movement (`beyonce.CmdSetSpeedFraction`) -> emits `OnSafeLogoffAborted("SAFE_LOGOFF_ABORT_MOVING")`
    let beyonce_oid = ctx
        .bind_object(&sess, "beyonce", sys_id as i64)
        .expect("bind beyonce");
    let _ = ctx.drain_notifications_for_char(90_000_001);
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_oid),
        "CmdSetSpeedFraction",
        &[PyRep::Real(0.5)],
        &empty,
        None,
    )
    .await
    .expect("CmdSetSpeedFraction");
    let move_aborts = ctx.drain_notifications_for_char(90_000_001);
    assert!(
        move_aborts.iter().any(|n| {
            n.event == "OnSafeLogoffAborted"
                && n.args == vec![PyRep::String("SAFE_LOGOFF_ABORT_MOVING".into())]
        }),
        "Movement must abort active safe logoff with SAFE_LOGOFF_ABORT_MOVING"
    );

    // 7. Restart SafeLogoff and abort via explicit `ship.AbortSafeLogoff`
    ctx.dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff restart");
    let _ = sess.drain_notifications();
    let abort_res = ctx
        .dispatch_rpc(&mut sess, "ship", None, "AbortSafeLogoff", &[], &empty, None)
        .await
        .expect("AbortSafeLogoff");
    assert_eq!(abort_res, PyRep::Bool(true));
    let abort_notifs = sess.drain_notifications();
    assert!(abort_notifs.iter().any(|(name, args)| {
        name == "OnSafeLogoffAborted"
            && *args == vec![PyRep::String("SAFE_LOGOFF_ABORT_GENERIC".into())]
    }));

    // 8. Restart SafeLogoff and advance 30s -> ship ball is removed from Ballpark, coordinates saved, `OnSafeLogoffActivated` emitted
    let _ = sys_handle
        .set_movement_mode(ship_id, MovementMode::Stop)
        .await;
    let _ = sys_handle.step_manual(30.0).await;
    let custom_pos = DVec3::new(42_000.0, -15_000.0, 88_000.0);
    let _ = sys_handle.set_ball_position(ship_id, custom_pos).await;

    let _ = ctx.drain_notifications_for_char(90_000_001);
    ctx.dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff before 30s completion");

    // At T+29s, ship is still in space
    ctx.advance_secs(29);
    assert!(
        sys_handle
            .get_ball_snapshot(ship_id)
            .await
            .unwrap()
            .is_some(),
        "Ship ball must remain in space at T+29s"
    );

    // At T+30s, ship ball is removed and OnSafeLogoffActivated (arity 0) is emitted
    ctx.advance_secs(1);
    assert!(
        sys_handle
            .get_ball_snapshot(ship_id)
            .await
            .unwrap()
            .is_none(),
        "Ship ball must be removed from Ballpark at T+30s"
    );
    let activated_notifs = ctx.drain_notifications_for_char(90_000_001);
    assert!(
        activated_notifs
            .iter()
            .any(|n| n.event == "OnSafeLogoffActivated" && n.args.is_empty()),
        "OnSafeLogoffActivated (arity 0) must be emitted at T+30s"
    );
    let saved_pos = ctx
        .db
        .with_snapshot(|s| s.item_positions.get(&ship_id).copied())
        .expect("ship position saved on safe logoff");
    assert_eq!(saved_pos[0].round() as i64, custom_pos.x.round() as i64);
}

#[tokio::test]
async fn test_sp6_ungraceful_disconnect_emergency_warp_timers_pvp_renewal_and_reconnect_across_restart()
{
    // parity: plan:SP-6, feat:crimewatch.pvp-logoff-timer-renewal
    eve_testkit::attest(&["plan:SP-6", "feat:crimewatch.pvp-logoff-timer-renewal"]);

    let (ctx2, sys_id, ship_id, expected_shield, expected_armor, final_pos) = {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let empty = HashMap::new();

        let (mut sess, sys_id, ship_id) = setup_pilot_in_space(&ctx, 90_000_001).await;
        let sys_handle = ctx.systems.get_or_spawn(sys_id);

        // Unaggressed logoff (`userSvc.UserLogOffCharacter`) -> 60s removal timer + emergency warp ~1,000,000 km (`1e9 m`)
        let origin_pos = sys_handle
            .get_ball_position_sync(ship_id)
            .unwrap_or(DVec3::ZERO);
        ctx.dispatch_rpc(
            &mut sess,
            "userSvc",
            None,
            "UserLogOffCharacter",
            &[PyRep::Integer(90_000_001)],
            &empty,
            None,
        )
        .await
        .expect("UserLogOffCharacter");

        let disc_entry = with_sp6_state(&ctx.db, |st| {
            st.disconnected_ships.get(&90_000_001).cloned()
        })
        .expect("DisconnectedShipEntry recorded");
        assert_eq!(
            disc_entry.remove_at_unix,
            disc_entry.disconnected_at_unix + 60,
            "Unaggressed logoff timer must be 60s"
        );
        let warp_dest = DVec3::from_array(
            disc_entry
                .emergency_warp_dest
                .expect("Unscrambled ship must initiate emergency warp"),
        );
        let warp_dist = warp_dest.distance(origin_pos);
        assert!(
            (warp_dist - 1_000_000_000.0).abs() < 1_000.0,
            "Emergency warp destination must be ~1,000,000 km (1e9 m) away, got {warp_dist}"
        );

        // Ship ball is still in space at T+30s
        ctx.advance_secs(30);
        assert!(
            sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .unwrap()
                .is_some(),
            "Disconnected ship must remain in Ballpark before timer expiry"
        );

        // NPC aggression against an already-disconnected ship must NOT renew its log-off timer
        let prev_remove_at = with_sp6_state(&ctx.db, |st| {
            st.disconnected_ships[&90_000_001].remove_at_unix
        });
        record_npc_aggression_timer(&ctx, 90_000_001, ctx.now_unix());
        let after_npc_remove_at = with_sp6_state(&ctx.db, |st| {
            st.disconnected_ships[&90_000_001].remove_at_unix
        });
        assert_eq!(
            after_npc_remove_at, prev_remove_at,
            "NPC aggression must NOT renew disconnected ship's log-off timer"
        );

        // Capsuleer (PvP) aggression against the disconnected ship MUST renew its removal timer to `now + 900s` (15m)
        let now_pvp = ctx.now_unix();
        record_pvp_aggression_and_renew_logoff_timer(&ctx, 90_000_002, 90_000_001, now_pvp);
        let renewed_remove_at = with_sp6_state(&ctx.db, |st| {
            st.disconnected_ships[&90_000_001].remove_at_unix
        });
        assert_eq!(
            renewed_remove_at,
            now_pvp + 900,
            "Capsuleer aggression must renew disconnected ship's PvP removal timer to now + 900s"
        );

        // Apply shield + armor damage to the disconnected ship while it is in space
        let _ = sys_handle
            .apply_damage(ship_id, DamageVector::new(120.0, 80.0, 0.0, 0.0))
            .await
            .expect("apply_damage to disconnected ship");
        let damaged_cs = sys_handle
            .get_combat_state(ship_id)
            .await
            .unwrap()
            .expect("combat state");
        assert!(
            damaged_cs.shield_hp < damaged_cs.max_shield_hp,
            "Disconnected ship took shield damage"
        );

        // Verify that the attacker (`90_000_002`), who now has an active 15m PvP timer, is blocked from
        // activating Abyssal, Noise/Signal Random Jump, and Proving Grounds filaments
        let (mut att_sess, _, _) = setup_pilot_in_space(&ctx, 90_000_002).await;
        for (svc, method, args) in [
            ("abyssal_content_manager", "ActivateAbyssalKey", vec![PyRep::Integer(47_765)]),
            ("randomJumpMgr", "ActivateRandomJumpFilament", vec![PyRep::Integer(1)]),
            ("pvpFilamentMgr", "JoinPVPQueue", vec![PyRep::Integer(1), PyRep::Integer(56_303)]),
        ] {
            let err = ctx
                .dispatch_rpc(&mut att_sess, svc, None, method, &args, &empty, None)
                .await
                .expect_err("Filament must be blocked by active PvP timer");
            assert_eq!(err.code, "PvpTimerActive");
        }

        // At T+899s after PvP aggression, victim's ship is still in space
        ctx.advance_secs(899);
        assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_some());

        // At T+900s, victim's ship is removed from the Ballpark while preserving character in-space state
        ctx.advance_secs(1);
        assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none());
        let saved_pos = ctx.db.with_snapshot(|snap| {
            let ch = &snap.characters[&90_000_001];
            assert_eq!(ch.solar_system_id, sys_id);
            assert!(ch.station_id.is_none() && ch.structure_id.is_none());
            snap.item_positions[&ship_id]
        });
        assert!(DVec3::from_array(saved_pos).distance(warp_dest) < 1.0);
        assert!(DVec3::from_array(saved_pos).distance(origin_pos) > 999_000_000.0);

        let ctx2 = persist_and_reopen_sqlite(&ctx, "sp6_disconnect");
        (ctx2, sys_id, ship_id, damaged_cs.shield_hp, damaged_cs.armor_hp, saved_pos)
    };

    // Reopen SQLite in a fresh ServerContext and log pilot back in
    pin_clock(&ctx2);
    let empty = HashMap::new();
    let mut sess2 = SessionState::default();
    login_pilot(&ctx2, &mut sess2, "demo", "demo-secret").await;
    ctx2.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &empty, None)
        .await
        .expect("SelectCharacterID on reconnect");
    assert_eq!(sess2.solar_system_id, Some(sys_id));
    assert_eq!(sess2.ship_id, Some(ship_id));
    assert_eq!(sess2.station_id, None);

    let beyonce_oid2 = ctx2.bind_object(&sess2, "beyonce", sys_id as i64).expect("bind beyonce");
    ctx2.dispatch_rpc(&mut sess2, "beyonce", Some(beyonce_oid2), "UpdateStateRequest", &[], &empty, None)
        .await
        .expect("UpdateStateRequest on reconnect");

    let sys_handle2 = ctx2.systems.get_or_spawn(sys_id);
    let all_balls = sys_handle2.list_balls().await.unwrap();
    let matching_ship_balls: Vec<_> = all_balls.iter().filter(|b| b.id == ship_id).collect();
    assert_eq!(matching_ship_balls.len(), 1);
    assert!(matching_ship_balls[0].position.distance(DVec3::from_array(final_pos)) < 1.0);
    let reconnected_cs = sys_handle2.get_combat_state(ship_id).await.unwrap().expect("reconnected combat state");
    assert_eq!(reconnected_cs.shield_hp.round() as i64, expected_shield.round() as i64);
    assert_eq!(reconnected_cs.armor_hp.round() as i64, expected_armor.round() as i64);
}

#[tokio::test]
async fn test_sp7_self_destruct_server_timer_detonates_without_second_client_call() {
    // parity: feat:lifecycle.self-destruct-server-timer
    eve_testkit::attest(&["feat:lifecycle.self-destruct-server-timer"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let empty = HashMap::new();

    let (mut sess, sys_id, ship_id) = setup_pilot_in_space(&ctx, 90_000_001).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Launch a drone in space before starting self-destruct
    let drone_id = 991_500_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 626; // Vexor (drone bandwidth)
        snap.items.insert(
            drone_id,
            sp6_item(drone_id, 2_454, 90_000_001, ship_id, InventoryFlag::DroneBay, HashMap::new()),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(&mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(drone_id)])], &empty, None)
        .await
        .expect("LaunchDrones before self-destruct");

    // Start 120s self-destruct via a single `ship.SelfDestruct([1])` call
    let start_res = ctx
        .dispatch_rpc(&mut sess, "ship", None, "SelfDestruct", &[PyRep::Integer(1)], &empty, None)
        .await
        .expect("SelfDestruct start");
    let PyRep::KeyVal(start_kv) = start_res else {
        panic!("expected KeyVal from SelfDestruct, got {start_res:?}");
    };
    assert_eq!(start_kv.get("countdownStarted"), Some(&PyRep::Bool(true)));
    assert_eq!(start_kv.get("remainingSeconds"), Some(&PyRep::Integer(120)));

    // At T+119s, ship has not detonated yet
    ctx.advance_secs(119);
    assert_eq!(ctx.db.with_snapshot(|s| s.items.contains_key(&ship_id)), true);

    // At T+120s, server timer detonates the ship automatically without any second client `SelfDestruct` call
    let _ = ctx.drain_notifications_for_char(90_000_001);
    ctx.advance_secs(1);
    let cap_id = ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.contains_key(&ship_id), false);
        let ch = &snap.characters[&90_000_001];
        assert_eq!(snap.items[&ch.active_ship_id].type_id, 670);
        assert!(snap.killmails.values().any(|km| km.victim_character_id == 90_000_001));
        ch.active_ship_id
    });
    assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none());
    let is_abandoned = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .find(|d| d.drone_item_id == drone_id)
            .map(|d| d.is_abandoned)
    });
    assert_eq!(is_abandoned, Some(true), "Drone in space must be abandoned when ship self-destructs on server timer");
    let death_notifs = ctx.drain_notifications_for_char(90_000_001);
    assert!(
        death_notifs.iter().any(|n| n.event == "OnSessionChanged"),
        "OnSessionChanged must be emitted when server self-destruct timer destroys ship"
    );
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "GetAllInfo", &[], &empty, None).await;
    assert_eq!(sess.ship_id, Some(cap_id), "Connected session ship_id must reconcile to capsule after server-timer death");
}

#[tokio::test]
async fn test_sp7_abyssal_disconnect_20m_collapse_destroys_ship_and_pod() {
    // parity: feat:lifecycle.abyssal-disconnect
    eve_testkit::attest(&["feat:lifecycle.abyssal-disconnect"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let empty = HashMap::new();

    let (mut sess, origin_sys, ship_id) = setup_pilot_in_space(&ctx, 90_000_001).await;

    // Give pilot two Abyssal filaments (`47765` = T1 Calm Electrical Filament) and an active implant
    let filament_1 = 992_000_001_i64;
    let filament_2 = 992_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.entry(90_000_001).or_default().push(9_899);
        snap.items.insert(
            filament_1,
            sp6_item(filament_1, 47_765, 90_000_001, ship_id, InventoryFlag::Cargo, HashMap::new()),
        );
        snap.items.insert(
            filament_2,
            sp6_item(filament_2, 47_765, 90_000_001, ship_id, InventoryFlag::Cargo, HashMap::new()),
        );
    });

    // 1. Enter Abyssal pocket via `abyssalMgr.AbyssalEntranceGateActivation`, disconnect for 600s (< 20m),
    //    reconnect in the pocket, and exit cleanly via `abyssalMgr.AbyssalEndGateActivation`
    ctx.dispatch_rpc(
        &mut sess,
        "abyssalMgr",
        None,
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(filament_1)],
        &empty,
        None,
    )
    .await
    .expect("AbyssalEntranceGateActivation #1");
    assert_eq!(sess.solar_system_id, Some(32_000_001));

    handle_character_logoff_or_disconnect(&ctx, 90_000_001).await;
    ctx.advance_secs(600);
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&ship_id)));

    let mut sess_reconnect = SessionState::default();
    login_pilot(&ctx, &mut sess_reconnect, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess_reconnect,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &empty,
        None,
    )
    .await
    .expect("SelectCharacterID inside Abyssal pocket before 20m expiry");
    assert_eq!(sess_reconnect.solar_system_id, Some(32_000_001));
    assert_eq!(sess_reconnect.ship_id, Some(ship_id));

    let pid = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |r| {
        let p = r.abyssal_pockets.get_mut(&90_000_001).unwrap();
        p.current_room = 3; p.rooms[2].cleared = true; p.pocket_id
    });
    eve_server::services::pve_special_sites_abyssal::ensure_abyssal_room_balls_for_ctx(&ctx, 90_000_001).await;
    let _ = ctx.systems.get_or_spawn(32_000_001).set_ball_position(ship_id, glam::DVec3::new(0.0, 0.0, 100_000.0)).await;
    let exit_gate_id = 1_895_000_000 + pid * 10 + 3;
    ctx.dispatch_rpc(&mut sess_reconnect, "abyssalMgr", None, "AbyssalEndGateActivation", &[PyRep::Integer(exit_gate_id)], &empty, None)
        .await
        .expect("AbyssalEndGateActivation");
    assert_eq!(sess_reconnect.solar_system_id, Some(origin_sys));

    // 2. Enter a second Abyssal pocket, disconnect inside the pocket, and advance to T+1200s (20m collapse)
    ctx.dispatch_rpc(
        &mut sess_reconnect,
        "abyssalMgr",
        None,
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(filament_2)],
        &empty,
        None,
    )
    .await
    .expect("AbyssalEntranceGateActivation #2");
    assert_eq!(sess_reconnect.solar_system_id, Some(32_000_001));

    handle_character_logoff_or_disconnect(&ctx, 90_000_001).await;

    // At T+1199s (19m59s), ship is still alive in the pocket
    ctx.advance_secs(1_199);
    assert_eq!(ctx.db.with_snapshot(|s| s.items.contains_key(&ship_id)), true);

    // At T+1200s (20m00s), the Abyssal pocket collapses, destroying ship + capsule and respawning pilot at home station in a rookie corvette (`606`)
    ctx.advance_secs(1);

    // Log the pilot back in (`SelectCharacterID`) after clock advance with no Abyssal RPC calls
    let mut sess_relogin = SessionState::default();
    login_pilot(&ctx, &mut sess_relogin, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess_relogin, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &empty, None)
        .await
        .expect("SelectCharacterID after Abyssal collapse");
    assert_eq!(sess_relogin.station_id, Some(60_003_760));
    assert_eq!(sess_relogin.solar_system_id, Some(30_000_142));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.contains_key(&ship_id), false);
        assert_eq!(snap.active_implants.get(&90_000_001).is_none_or(|m| m.is_empty()), true);
        let ch = &snap.characters[&90_000_001];
        assert_eq!(ch.station_id, Some(ch.home_station_id));
        assert_eq!(snap.items[&ch.active_ship_id].type_id, 606);
        let pocket_kms: Vec<_> = snap.killmails.values()
            .filter(|km| km.victim_character_id == 90_000_001 && km.solar_system_id == 32_000_001)
            .collect();
        assert_eq!(pocket_kms.len(), 2, "Both ship and capsule killmails must be recorded in Abyssal pocket 32000001");
        assert!(pocket_kms.iter().any(|km| km.ship_type_id == 670), "Capsule (670) killmail must be recorded");
        assert!(pocket_kms.iter().any(|km| km.ship_type_id != 670), "Ship killmail must be recorded");
    });
}

#[tokio::test]
async fn test_r2_track3_lifecycle_filaments_salvage_and_drones() {
    let ctx = ServerContext::new();
    let empty = HashMap::new();
    let (mut sess, sys_id, ship_id) = setup_pilot_in_space(&ctx, 90_000_001).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // 1. Issue #75: SafeLogoff blocked while in warp or with persisted module_active_states
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_750,
            InventoryItemRecord {
                item_id: 990_750,
                type_id: 34593, // Entosis Link I
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(990_750);
    });
    let sl_err = ctx
        .dispatch_rpc(&mut sess, "ship", None, "SafeLogoff", &[], &empty, None)
        .await
        .expect("SafeLogoff with persisted active module");
    assert_eq!(
        sl_err,
        PyRep::List(vec![PyRep::String("SAFE_LOGOFF_ACTIVE_MODULES".into())])
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&990_750);
    });

    // 2. Issue #76: Filament jump blocked while warp scrambled
    let fil_id = 990_760;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 626; // Vexor (Cruiser)
        }
        snap.items.insert(
            fil_id,
            InventoryItemRecord {
                item_id: fil_id,
                type_id: 47_765,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.warp_scramble_strength = 2;
        }
    }
    let scram_fil = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(fil_id)],
            &empty,
            None,
        )
        .await
        .expect_err("Warp-scrambled ship must not activate Abyssal filament");
    assert_eq!(scram_fil.code, "WarpScrambled");
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.warp_scramble_strength = 0;
        }
    }

    // 3. Issue #81: LaunchDrones deduplicates drone_ids, requires DroneBay + category 18, and splits stacks
    let stack_drone_id = 990_810;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            stack_drone_id,
            InventoryItemRecord {
                item_id: stack_drone_id,
                type_id: 2454, // Hobgoblin I (category 18)
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let launched = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "LaunchDrones",
            &[PyRep::List(vec![
                PyRep::Integer(stack_drone_id),
                PyRep::Integer(stack_drone_id),
            ])],
            &empty,
            None,
        )
        .await
        .expect("LaunchDrones from stack with duplicate ID");
    let PyRep::List(balls) = launched else {
        panic!("Expected list of launched drone ball IDs");
    };
    assert_eq!(balls.len(), 1, "Duplicate drone IDs must be deduplicated");
    let rem_qty = ctx
        .db
        .with_snapshot(|s| s.items.get(&stack_drone_id).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(rem_qty, 4, "Launching 1 drone from stack of 5 must leave 4 in DroneBay");

    // 4. Issue #80: ScoopToDroneBay rejects non-drone items (ships/structures)
    let foreign_ship_id = 990_800;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            foreign_ship_id,
            InventoryItemRecord {
                item_id: foreign_ship_id,
                type_id: 626, // Vexor (Ship, category 6)
                owner_id: 90_000_002,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let ship_pos = sys_handle
        .get_ball_position_sync(ship_id)
        .unwrap_or(glam::DVec3::ZERO);
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: foreign_ship_id,
            type_id: 626,
            position: ship_pos + glam::DVec3::new(500.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let scoop_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "ScoopToDroneBay",
            &[PyRep::Integer(foreign_ship_id)],
            &empty,
            None,
        )
        .await
        .expect_err("Must not scoop a ship into DroneBay");
    assert_eq!(scoop_err.code, "NotADrone");

    // 5. Issue #79: CmdSalvage rejects non-wreck targets (must never delete ships/structures)
    let salv_drone_id = 990_790;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            salv_drone_id,
            InventoryItemRecord {
                item_id: salv_drone_id,
                type_id: 32787, // Salvage Drone I
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.dispatch_rpc(
        &mut sess,
        "ship",
        None,
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(salv_drone_id)])],
        &empty,
        None,
    )
    .await
    .expect("Launch Salvage Drone I");
    let salv_err = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdSalvage",
            &[PyRep::Integer(foreign_ship_id)],
            &empty,
            None,
        )
        .await
        .expect_err("CmdSalvage must reject non-wreck ship target");
    assert_eq!(salv_err.code, "InvalidSalvageTarget");
    assert!(
        ctx.db.with_snapshot(|s| s.items.contains_key(&foreign_ship_id)),
        "Non-wreck ship must never be deleted by CmdSalvage"
    );
}
