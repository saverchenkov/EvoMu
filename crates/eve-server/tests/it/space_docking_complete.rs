//! Integration tests for session change timers and module cycle cleanup on leave space.
//!
//! Validates:
//! - `feat:lifecycle.module-cycles-stop-on-leave-space`: module cycles and active states cleared on dock/ball removal.
//! - `feat:navigation.session-change-timer-triggers`: 10s timer triggers on login, clone jump, jump drive, fleet changes, board/eject, dock/undock.
//! - `feat:lifecycle.session-change-timer-coverage`: coverage across handlers and 60s jump-drive invulnerability.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::router::ServerContext;
use eve_server::services::space_destiny::{
    check_session_change_timer_active, get_session_change_timer,
    grant_location_change_invulnerability, is_location_change_invulnerable,
};
use eve_server::system_actor::ActiveModuleCycle;
use glam::DVec3;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

#[tokio::test]
async fn test_module_cycles_stop_on_leave_space() {
    // parity: feat:lifecycle.module-cycles-stop-on-leave-space
    eve_testkit::attest(&["feat:lifecycle.module-cycles-stop-on-leave-space"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let sys_id = 30_000_142u32;
    let module_id = 999_888i64;
    let station_id = 60_003_760i64;

    // 1. Setup pilot, ship, and module in space
    let sys = ctx.systems.get_or_spawn(sys_id);
    let station_pos = ctx.sde.stations[&station_id].position;
    let ball = Ball { id: ship_id, type_id: 587, position: station_pos + DVec3::new(100.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
    sys.spawn_ball_with_meta(ball, ctx.build_ship_combat_state(char_id, ship_id, 587), ctx.build_ship_slim_meta(char_id, 587)).await.expect("spawn_ball");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(module_id, eve_db::InventoryItemRecord {
            item_id: module_id, type_id: 3528, owner_id: char_id, location_id: ship_id,
            flag: eve_db::InventoryFlag::LoSlot0, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });

    let mut sess = SessionState {
        user_id: Some(1),
        session_id: 8881,
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    let _ = ctx.db.acquire_online_lock(1, 8881, Some(char_id), false, ctx.now_unix());

    // 2. Activate module via dogmaIM.Activate RPC and step system
    let act_rep = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(module_id), PyRep::String("repair".into())])
        .await
        .expect("dogmaIM.Activate");
    assert_eq!(act_rep.dict_get("active"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&module_id)));

    let _ = sys.step_manual(1.0).await.unwrap();

    // 3. Dock via stationSvc.Dock RPC after advancing clock past session timer
    ctx.advance_secs(12);
    let _ = rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(station_id)]).await.expect("stationSvc.Dock");
    assert_eq!(sess.station_id, Some(station_id));

    // 4. Assert module_active_states cleared, active cycle removed from system actor, and OnGodmaShipEffect emitted
    assert!(
        !ctx.db.with_snapshot(|s| s.module_active_states.contains(&module_id)),
        "module_active_states must be cleared on docking"
    );
    assert_eq!(
        sys.update_module_overload(ship_id, module_id, false).await,
        Ok(false),
        "ActiveModuleCycle must be removed from system actor on docking"
    );
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnGodmaShipEffect"),
        "OnGodmaShipEffect deactivation must be emitted on docking"
    );

    // 5. Test space exit via RemoveBall with deactivate_ship_modules_on_leave_space
    let ship_id_2 = 100_000_002i64;
    let module_id_2 = 999_889i64;
    let ball_2 = Ball { id: ship_id_2, type_id: 587, position: DVec3::new(2000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
    sys.spawn_ball_with_meta(ball_2, ctx.build_ship_combat_state(char_id, ship_id_2, 587), ctx.build_ship_slim_meta(char_id, 587)).await.expect("spawn second ball");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(module_id_2, eve_db::InventoryItemRecord {
            item_id: module_id_2, type_id: 3528, owner_id: char_id, location_id: ship_id_2,
            flag: eve_db::InventoryFlag::LoSlot1, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.module_active_states.insert(module_id_2);
    });
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id: ship_id_2, module_id: module_id_2, module_type_id: 3528, effect_name: "armorRepair".into(), repeat: 0, duration_s: 10.0, ..Default::default()
    }).await.expect("add second cycle");

    assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&module_id_2)));
    eve_server::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(&ctx, char_id, ship_id_2);
    sys.remove_ball(ship_id_2).await;

    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&module_id_2)));
    assert_eq!(
        sys.update_module_overload(ship_id_2, module_id_2, false).await,
        Ok(false)
    );
    let notifs_2 = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs_2.iter().any(|n| n.event == "OnGodmaShipEffect"),
        "OnGodmaShipEffect deactivation must be emitted on remove_ball"
    );
}

#[tokio::test]
async fn test_session_change_timer_triggers_and_coverage() {
    // parity: feat:navigation.session-change-timer-triggers, feat:lifecycle.session-change-timer-coverage
    eve_testkit::attest(&[
        "feat:navigation.session-change-timer-triggers",
        "feat:lifecycle.session-change-timer-coverage",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let sys_id = 30_000_142u32;
    let now_unix = ctx.now_unix();

    // 1. Character Login / Selection trigger
    let mut sess = SessionState {
        user_id: Some(1),
        session_id: 12345,
        ..Default::default()
    };
    let _ = ctx.db.acquire_online_lock(1, 12345, None, false, now_unix);

    let sel_res = rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    assert!(matches!(sel_res, PyRep::Dict(_) | PyRep::KeyVal(_)));

    let timer = get_session_change_timer(&ctx, char_id).expect("Timer must exist after login");
    assert_eq!(timer.reason, "login");
    assert_eq!(timer.expires_unix, now_unix + 10);

    // Rapid session change check within 10s: must be active and block
    let active = check_session_change_timer_active(&ctx, char_id, now_unix + 5, None);
    assert!(active.is_some(), "Timer must be active within 10s");

    // Verify SetSessionCheck rejects while timer active
    let check_err = rpc(&ctx, &mut sess, "beyonce", "SetSessionCheck", &[PyRep::String("dock".into())]).await;
    assert!(check_err.is_err(), "SetSessionCheck must reject while 10s timer is active");

    // After 10s expiration: timer must expire
    let expired = check_session_change_timer_active(&ctx, char_id, now_unix + 10, None);
    assert!(expired.is_none(), "Timer must expire after 10s");

    // 2. CloneJump trigger via RPC
    ctx.advance_secs(12);
    let pod_id = 99_000_300_i64;
    sess.character_id = Some(char_id);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(pod_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(pod_id, eve_db::InventoryItemRecord { item_id: pod_id, type_id: 670, owner_id: char_id, location_id: 60_003_760, flag: eve_db::InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default() });
        snap.skills.entry(char_id).or_default().insert(
            24242,
            eve_db::TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = pod_id;
            ch.balance.0 += 100_000_000_00;
        }
    });

    let inst_res = rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[])
        .await
        .expect("InstallCloneInStation");
    let jc_id = match inst_res {
        PyRep::KeyVal(kv) => kv.get("jumpCloneID").and_then(|v| v.as_i64()).unwrap(),
        _ => panic!("Expected KeyVal"),
    };

    let cj_res = rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", &[PyRep::Integer(jc_id)])
        .await
        .expect("CloneJump");
    assert!(matches!(cj_res, PyRep::KeyVal(_)));
    let cj_timer = get_session_change_timer(&ctx, char_id).expect("clone_jump timer");
    assert_eq!(cj_timer.reason, "clone_swap");

    // Active timer blocks immediate second CloneJump
    let cj_blocked = rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", &[PyRep::Integer(jc_id)]).await;
    assert_eq!(cj_blocked.unwrap_err().code, "SessionChangeTimerActive");

    // 3. Jump drive / cyno beacon arrival & 60s location-change invulnerability via RPC
    ctx.advance_secs(12);
    let dest_sys_id = 30_000_204u32;
    let beacon_struct_id = 1_040_000_000_200_i64;
    let beacon_pos = DVec3::new(450_000.0, 0.0, 0.0);
    let ship_id = 100_000_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            beacon_struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: beacon_struct_id,
                type_id: 35840,
                solar_system_id: dest_sys_id,
                owner_corp_id: 1000002,
                name: "Cyno Beacon".into(),
                position: beacon_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            88_000_001,
            eve_db::InventoryItemRecord {
                item_id: 88_000_001,
                type_id: 16274,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::SpecializedFuelBay,
                quantity: 20_000,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 23757,
                owner_id: char_id,
                location_id: sys_id as i64,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.skills.entry(char_id).or_default().insert(
            21611,
            eve_db::TrainedSkillRecord { type_id: 21611, level: 5, skill_points: 256_000.0 },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball(eve_destiny::Ball {
        id: ship_id,
        type_id: 23757,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 100.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 500.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    }).await;

    let jump_res = rpc(
        &ctx,
        &mut sess,
        "structureCynoBeaconMgr",
        "CmdJumpToStructureBeacon",
        &[PyRep::Integer(dest_sys_id as i64), PyRep::Integer(beacon_struct_id)],
    )
    .await
    .expect("CmdJumpToStructureBeacon");
    assert!(matches!(jump_res, PyRep::KeyVal(_)));

    let jd_timer = get_session_change_timer(&ctx, char_id).expect("jump_drive timer");
    assert_eq!(jd_timer.reason, "jump_drive");
    let jump_time = jd_timer.expires_unix - 10;
    assert!(is_location_change_invulnerable(&ctx, char_id, jump_time + 30), "Must be invulnerable at T+30s");

    // Targeting lock check: attacker attempts to lock invulnerable character via dogmaIM.AddTarget
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.solar_system_id = dest_sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(ship) = snap.items.get_mut(&100_000_002) {
            ship.location_id = dest_sys_id as i64;
        }
    });
    let mut attacker_sess = SessionState {
        user_id: Some(2),
        character_id: Some(90_000_002),
        solar_system_id: Some(dest_sys_id),
        ship_id: Some(100_000_002),
        ..Default::default()
    };
    let dest_handle = ctx.systems.get_or_spawn(dest_sys_id);
    let _ = dest_handle.spawn_ball(eve_destiny::Ball {
        id: 100_000_002,
        type_id: 587,
        position: beacon_pos + DVec3::new(1000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 100_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    }).await;

    let lock_err = rpc(&ctx, &mut attacker_sess, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_id)]).await;
    assert_eq!(lock_err.unwrap_err().code, "TargetInvulnerable");

    // Advance clock and destiny ticks past 60s: invulnerability + jump cloak expire and targeting succeeds
    ctx.advance_secs(65);
    for _ in 0..60 {
        let _ = dest_handle.step_manual(1.0).await;
    }
    let lock_ok = rpc(&ctx, &mut attacker_sess, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_id)]).await;
    assert!(lock_ok.is_ok(), "AddTarget must succeed after 60s invulnerability window expires");

    // Invulnerability broken by movement
    grant_location_change_invulnerability(&ctx, char_id, ctx.now_unix() + 60);
    assert!(is_location_change_invulnerable(&ctx, char_id, ctx.now_unix()));
    let _ = rpc(&ctx, &mut sess, "beyonce", "CmdGotoPoint", &[PyRep::Real(100.0), PyRep::Real(200.0), PyRep::Real(300.0)]).await;
    assert!(!is_location_change_invulnerable(&ctx, char_id, ctx.now_unix()), "Movement must break invulnerability");

    // 4. Fleet join / position change triggers and blocking
    ctx.advance_secs(12);
    let fid = 987_654i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut member_details = HashMap::new();
        member_details.insert(1001i64, eve_db::FleetMemberRecord {
            character_id: 1001i64,
            role: eve_db::FleetRole::FleetCommander,
            wing_id: Some(1),
            squad_id: Some(1),
            takes_fleet_warp: true,
            accepts_regroup: true,
            accepts_conduit_jumps: true,
            job: 0,
            role_name: String::new(),
            join_time_unix: ctx.now_unix(),
        });
        let mut invites = std::collections::HashSet::new();
        invites.insert(char_id);
        snap.fleets.insert(fid, eve_db::FleetRecord {
            fleet_id: fid,
            boss_id: 1001i64,
            members: vec![1001i64],
            wings: vec![1],
            squads: vec![1],
            free_move: true,
            member_details,
            invites,
            ..Default::default()
        });
    });

    let join_res = rpc(&ctx, &mut sess, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fid)])
        .await
        .expect("AcceptInvite");
    assert!(matches!(join_res, PyRep::KeyVal(_) | PyRep::Dict(_)));
    let fl_timer = get_session_change_timer(&ctx, char_id).expect("fleet timer");
    assert_eq!(fl_timer.reason, "fleet_change");

    // Position change while active timer is running is rejected
    let move_blocked = rpc(
        &ctx,
        &mut sess,
        "fleetObjectHandler",
        "MoveMember",
        &[PyRep::Integer(char_id), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)],
    )
    .await;
    assert_eq!(move_blocked.unwrap_err().code, "SessionChangeTimerActive");

    // After 10s timer expires, position change succeeds
    ctx.advance_secs(12);
    let move_ok = rpc(
        &ctx,
        &mut sess,
        "fleetObjectHandler",
        "MoveMember",
        &[PyRep::Integer(char_id), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)],
    )
    .await
    .expect("MoveMember");
    assert!(matches!(move_ok, PyRep::KeyVal(_)));

    // 5. Ship board / eject / leave triggers and blocking
    ctx.advance_secs(12);
    let board_ship_id = 100_000_001i64;
    sess.ship_id = Some(board_ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(20533, eve_db::TrainedSkillRecord { type_id: 20533, level: 5, skill_points: 256_000.0 });
        sk.insert(3327, eve_db::TrainedSkillRecord { type_id: 3327, level: 5, skill_points: 256_000.0 });
        sk.insert(24311, eve_db::TrainedSkillRecord { type_id: 24311, level: 5, skill_points: 256_000.0 });
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
    });

    let board_ok = rpc(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(board_ship_id)])
        .await
        .expect("Board");
    assert!(matches!(board_ok, PyRep::KeyVal(_)));
    let board_timer = get_session_change_timer(&ctx, char_id).expect("board timer");
    assert_eq!(board_timer.reason, "board_or_eject");

    // Immediate Eject while timer active is rejected
    let eject_blocked = rpc(&ctx, &mut sess, "ship", "Eject", &[]).await;
    assert_eq!(eject_blocked.unwrap_err().code, "SessionChangeTimerActive");

    // After 10s timer expires, Eject succeeds
    ctx.advance_secs(12);
    let eject_ok = rpc(&ctx, &mut sess, "ship", "Eject", &[]).await.expect("Eject");
    assert!(matches!(eject_ok, PyRep::KeyVal(_)));
    let eject_timer = get_session_change_timer(&ctx, char_id).expect("eject timer");
    assert_eq!(eject_timer.reason, "board_or_eject");

    // 6. Corp create, SetAccountKey, and expulsion triggers and blocks session changes
    ctx.advance_secs(12);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3363, // Corporation Management
            eve_db::TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance.0 += 100_000_000_00;
        }
    });

    let add_corp_res = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Destiny Alpha Corp".into()), PyRep::String("DALPH".into()), PyRep::Real(0.05)],
    )
    .await
    .expect("AddCorporation");
    let new_corp_id = match &add_corp_res {
        PyRep::KeyVal(kv) => kv.get("corporationID").and_then(|v| v.as_i64()).unwrap(),
        _ => panic!("Expected KeyVal from AddCorporation"),
    };
    sess.corporation_id = Some(new_corp_id);
    let corp_timer = get_session_change_timer(&ctx, char_id).expect("corp_change timer");
    assert_eq!(corp_timer.reason, "corp_change");

    // Session change while 10s corp_change timer active is rejected with SessionChangeInProgress
    let corp_blocked = rpc(&ctx, &mut sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1004)]).await;
    let err = corp_blocked.unwrap_err();
    assert_eq!(err.code, "SessionChangeInProgress");
    assert!(err.message.contains("corp_change"));

    // Advance clock past timer: SetAccountKey succeeds and sets set_account_key timer
    ctx.advance_secs(12);
    let sak_ok = rpc(&ctx, &mut sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1004)]).await;
    assert!(sak_ok.is_ok(), "SetAccountKey must succeed after timer expiry");
    let sak_timer = get_session_change_timer(&ctx, char_id).expect("set_account_key timer");
    assert_eq!(sak_timer.reason, "set_account_key");

    // Immediate subsequent SetAccountKey rejected while set_account_key timer active
    let sak_blocked = rpc(&ctx, &mut sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1000)]).await;
    let err2 = sak_blocked.unwrap_err();
    assert_eq!(err2.code, "SessionChangeInProgress");
    assert!(err2.message.contains("set_account_key"));

    // Corp expulsion sets timer on expelled member
    ctx.advance_secs(12);
    let expelled_char_id = 90_000_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&new_corp_id) {
            corp.members.insert(expelled_char_id);
            corp.member_roles.insert(expelled_char_id, 0);
        }
        if let Some(ch) = snap.characters.get_mut(&expelled_char_id) {
            ch.corporation_id = new_corp_id;
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
        }
    });
    let kick_ok = rpc(&ctx, &mut sess, "corpRegistry", "KickOutMember", &[PyRep::Integer(expelled_char_id)]).await;
    assert!(kick_ok.is_ok(), "KickOutMember must succeed");
    let kick_timer = get_session_change_timer(&ctx, expelled_char_id).expect("kick timer");
    assert_eq!(kick_timer.reason, "corp_change");

    let mut expelled_sess = SessionState {
        user_id: Some(2),
        character_id: Some(expelled_char_id),
        corporation_id: Some(1000001),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    let kick_member_blocked = rpc(&ctx, &mut expelled_sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1000)]).await;
    let err3 = kick_member_blocked.unwrap_err();
    assert_eq!(err3.code, "SessionChangeInProgress");
    assert!(err3.message.contains("corp_change"));

    // 7. Ship loss and podding death pipeline triggers session-change timer and blocks session changes
    ctx.advance_secs(12);
    let victim_char_id = 90_000_002i64;
    let victim_ship_id = 100_000_090i64;
    let victim_pod_id = 100_000_091i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim_char_id) {
            ch.corporation_id = 1000001;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = victim_ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(victim_ship_id, eve_db::InventoryItemRecord {
            item_id: victim_ship_id, type_id: 587, owner_id: victim_char_id, location_id: sys_id as i64,
            quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.items.insert(victim_pod_id, eve_db::InventoryItemRecord {
            item_id: victim_pod_id, type_id: 670, owner_id: victim_char_id, location_id: sys_id as i64,
            quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });

    let mut victim_sess = SessionState {
        user_id: Some(2),
        session_id: 9901,
        character_id: Some(victim_char_id),
        corporation_id: Some(1000001),
        solar_system_id: Some(sys_id),
        ship_id: Some(victim_ship_id),
        ..Default::default()
    };

    // Ship destruction triggers "ship_loss" timer
    let _ = execute_death_pipeline(&ctx, sys_id, victim_char_id, victim_ship_id, char_id, DVec3::ZERO, 42)
        .await
        .expect("ship death pipeline");

    let ship_timer = get_session_change_timer(&ctx, victim_char_id).expect("ship_loss timer");
    assert_eq!(ship_timer.reason, "ship_loss");
    assert_eq!(ship_timer.expires_unix, ctx.now_unix() + 10);

    // Victim calling session change during 10s ship_loss timer is rejected with SessionChangeInProgress
    let ship_blocked = rpc(&ctx, &mut victim_sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1000)]).await;
    let err4 = ship_blocked.unwrap_err();
    assert_eq!(err4.code, "SessionChangeInProgress");
    assert!(err4.message.contains("ship_loss"));

    // Advance clock past ship_loss timer
    ctx.advance_secs(12);

    // Pod destruction triggers "podding" timer
    let _ = execute_death_pipeline(&ctx, sys_id, victim_char_id, victim_pod_id, char_id, DVec3::ZERO, 43)
        .await
        .expect("pod death pipeline");

    let pod_timer = get_session_change_timer(&ctx, victim_char_id).expect("podding timer");
    assert_eq!(pod_timer.reason, "podding");
    assert_eq!(pod_timer.expires_unix, ctx.now_unix() + 10);

    // Victim calling session change during 10s podding timer is rejected with SessionChangeInProgress
    let pod_blocked = rpc(&ctx, &mut victim_sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1000)]).await;
    let err5 = pod_blocked.unwrap_err();
    assert_eq!(err5.code, "SessionChangeInProgress");
    assert!(err5.message.contains("podding"));

    // Advance clock past podding timer: session change succeeds
    ctx.advance_secs(12);
    let post_pod_ok = rpc(&ctx, &mut victim_sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1000)]).await;
    assert!(post_pod_ok.is_ok(), "SetAccountKey must succeed after podding timer expiry");
}
