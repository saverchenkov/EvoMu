//! Integration tests for `SP-3` (Warp Navigation, Disruption, Capacitor Cost, & Exit Invulnerability),
//! `SP-4` (Stargate Capital Restrictions, Session-Change Timers, & Jump Cloak Immunities),
//! and `SP-5` (Station & Upwell Structure Docking, Undocking, & Ship Boarding).

use crate::common;

use eve_db::types::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_sp5_docking_and_undocking_station_structure_and_boarding() {
    // parity: plan:SP-5
    // parity: feat:ships.board-ship, feat:navigation.undock, feat:navigation.dock-station, feat:navigation.dock-structure, notify:OnSessionChanged, rpc:charUnboundMgr.SelectCharacterID, rpc:ship.Board, rpc:structureDocking.Dock, rpc:structureDocking.Undock
    eve_testkit::attest(&["plan:SP-5", "feat:ships.board-ship", "feat:navigation.undock", "feat:navigation.dock-station", "feat:navigation.dock-structure"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(2003);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let first_ship_id = 100_000_001;
    let second_ship_id = 100_000_002;
    let station_id = 60003760;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            second_ship_id,
            InventoryItemRecord {
                item_id: second_ship_id,
                type_id: 621, // Caracal
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    assert_eq!(sess.station_id, Some(station_id));
    assert_eq!(sess.ship_id, Some(first_ship_id));
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Board a DIFFERENT ship in station hangar (feat:ships.board-ship)
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(3334, eve_db::TrainedSkillRecord { type_id: 3334, level: 5, skill_points: 256_000.0 });
        sk.insert(3330, eve_db::TrainedSkillRecord { type_id: 3330, level: 5, skill_points: 256_000.0 });
        sk.insert(3327, eve_db::TrainedSkillRecord { type_id: 3327, level: 5, skill_points: 256_000.0 });
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
    });
    ctx.advance_secs(12);
    let board_res = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "Board",
            &[PyRep::Integer(second_ship_id)],
            &kw,
            None,
        )
        .await
        .expect("Board second ship in station");
    let PyRep::KeyVal(bkv) = board_res else {
        panic!("Expected KeyVal from ship.Board");
    };
    assert_eq!(bkv.get("shipID").unwrap().as_i64().unwrap(), second_ship_id);
    assert_eq!(bkv.get("oldShipID").unwrap().as_i64().unwrap(), first_ship_id);
    assert_eq!(sess.ship_id, Some(second_ship_id));

    let board_notifs = ctx.drain_notifications_for_char(char_id);
    // The server announces the ship change as an `OnSessionChanged` `shipid` (old, new) change;
    // the V24 client derives `ProcessActiveShipChanged` itself (`DogmaLocation.MakeShipActive`).
    let sess_change = board_notifs
        .iter()
        .find(|n| n.event == "OnSessionChanged")
        .expect("OnSessionChanged on Board");
    let Some(PyRep::Dict(changes)) = sess_change.args.last() else {
        panic!("OnSessionChanged change dict expected: {:?}", sess_change.args);
    };
    assert!(changes.iter().any(|(k, v)| *k == PyRep::String("shipid".into())
        && *v == PyRep::Tuple(vec![PyRep::Integer(first_ship_id), PyRep::Integer(second_ship_id)])));
    assert!(
        board_notifs
            .iter()
            .all(|n| n.event != "ProcessActiveShipChanged")
    );

    ctx.advance_secs(12);
    ctx.dispatch_rpc(
        &mut sess,
        "ship",
        None,
        "Board",
        &[PyRep::Integer(first_ship_id)],
        &kw,
        None,
    )
    .await
    .expect("Board first ship in station");
    assert_eq!(sess.ship_id, Some(first_ship_id));

    // 2. Undock from station along celestial-derived undock vector + 30s invulnerability (feat:navigation.undock)
    let _ = ctx.drain_notifications_for_char(char_id);
    ctx.advance_secs(12);
    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    let beyonce_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    assert_eq!(ukv.get("shipID").unwrap().as_i64().unwrap(), first_ship_id);
    assert_eq!(
        ukv.get("undockInvulnerabilityTicks")
            .unwrap()
            .as_i64()
            .unwrap(),
        30
    );
    assert_eq!(sess.station_id, None);
    let undock_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        undock_notifs.iter().any(|n| n.event == "OnSessionChanged"),
        "Undock must emit OnSessionChanged notification"
    );

    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ball) = sys_handle
        .get_ball_snapshot(first_ship_id)
        .await
        .unwrap()
        .unwrap();

    let st_rec = &ctx.sde.stations[&station_id];
    let station_pos = st_rec.position;
    let orbit_id = st_rec.orbit_id.expect("Jita 4-4 has orbit_id");
    let orbit_cel = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.celestial_id == orbit_id)
        .expect("orbit celestial in Jita");
    let expected_dir = (station_pos - orbit_cel.position).normalize();
    let st_radius = ctx.sde.types[&st_rec.station_type_id].radius;
    let ship_radius = ctx.sde.types[&ball.type_id].radius;
    let expected_offset = (st_radius * 0.015 + ship_radius).clamp(500.0, 2_000.0);
    let expected_spawn = station_pos + expected_dir * expected_offset;
    assert!(ball.position.distance(expected_spawn) < 1.0);
    assert!(
        (ball.velocity - expected_dir * (ball.max_velocity * 0.5)).length() < 1.0,
        "Undocking ship must spawn with initial velocity along undock vector"
    );
    assert_eq!(
        ball.mode,
        MovementMode::GotoDirection {
            direction: expected_dir
        }
    );

    let undock_cs = sys_handle
        .get_combat_state(first_ship_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(undock_cs.undock_invulnerable_remaining_ticks, 30);
    let hp_before_undock_hit = undock_cs.shield_hp;
    sys_handle
        .apply_damage(
            first_ship_id,
            eve_destiny::DamageVector {
                em: 250.0,
                thermal: 250.0,
                kinetic: 250.0,
                explosive: 250.0,
            },
        )
        .await
        .expect("apply damage during post-undock invulnerability");
    let undock_cs_after_hit = sys_handle
        .get_combat_state(first_ship_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(undock_cs_after_hit.shield_hp, hp_before_undock_hit);

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdSetSpeedFraction",
        &[PyRep::Real(0.5)],
        &kw,
        None,
    )
    .await
    .expect("CmdSetSpeedFraction breaks undock invulnerability");
    let undock_cs_broken = sys_handle
        .get_combat_state(first_ship_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(undock_cs_broken.undock_invulnerable_remaining_ticks, 0);

    // 3. Docking perimeter & Crimewatch checks (`feat:navigation.dock-station`)
    sys_handle
        .set_ball_position(first_ship_id, station_pos + DVec3::new(50_000.0, 0.0, 0.0))
        .await;
    let far_dock = ctx
        .dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(station_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(far_dock.unwrap_err().code, "OutsideDockingPerimeter");

    sys_handle
        .set_ball_position(first_ship_id, station_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    let now_dock = ctx.now_unix();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.trigger_weapon_timer(90_000_001, now_dock);
    }
    let dock_wpn_err = ctx
        .dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(station_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(dock_wpn_err.code, "WeaponsTimerActive");

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(90_000_001);
        st.weapon_timer_until_unix = 0;
        st.criminal_flag_until_unix = now_dock + 900;
    }
    let dock_crim_err = ctx
        .dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(station_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(dock_crim_err.code, "CriminalTimerActive");

    // Capsule (type 670) CAN dock in highsec even while a Criminal Timer is active
    let (_, mut pod_ball) = sys_handle.get_ball_snapshot(first_ship_id).await.unwrap().unwrap();
    let saved_hull_tid = pod_ball.type_id;
    pod_ball.type_id = 670;
    pod_ball.mode = MovementMode::Stop;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&first_ship_id) {
            it.type_id = 670;
        }
    });
    sys_handle.spawn_ball(pod_ball).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Dock", &[PyRep::Integer(station_id)], &kw, None)
        .await
        .expect("Capsule must be allowed to dock in highsec during Criminal Timer");
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock capsule");
    let (_, mut restored_ball) = sys_handle.get_ball_snapshot(first_ship_id).await.unwrap().unwrap();
    restored_ball.type_id = saved_hull_tid;
    restored_ball.position = station_pos + DVec3::new(1_000.0, 0.0, 0.0);
    restored_ball.mode = MovementMode::Stop;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&first_ship_id) {
            it.type_id = saved_hull_tid;
        }
    });
    sys_handle.spawn_ball(restored_ball).await.unwrap();

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(90_000_001).criminal_flag_until_unix = 0;
    }

    let _ = ctx.drain_notifications_for_char(char_id);
    ctx.advance_secs(12);
    let dock_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(station_id)],
            &kw,
            None,
        )
        .await
        .expect("Dock at station within docking perimeter");
    assert!(matches!(dock_ok, PyRep::KeyVal(_)));
    assert_eq!(sess.station_id, Some(station_id));
    assert!(
        sys_handle.get_ball_snapshot(first_ship_id).await.unwrap().is_none(),
        "Docking at station must despawn ship from space"
    );
    let dock_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        dock_notifs.iter().any(|n| n.event == "OnSessionChanged"),
        "Docking must emit OnSessionChanged notification"
    );

    // 4. Upwell structure docking: ACL denial, hull size restriction, Crimewatch, dock & undock (feat:navigation.dock-structure)
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock again");

    let struct_id = 1_040_000_000_100_i64;
    let struct_pos = DVec3::new(200_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832, // Astrahus (Medium Citadel)
                solar_system_id: sys_id,
                owner_corp_id: 98_000_099, // Hostile player corp -> ACL denied
                name: "Astrahus Staging".into(),
                position: struct_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
    });
    sess.role = eve_machonet::service_roles::ROLE_GMH;
    eve_server::services::station_structure::structure_upwell_ops::configure_structure_access_profile(
        &ctx,
        &sess,
        struct_id,
        Some(true),
        &[],
        &[],
        &[],
        &[],
    )
    .expect("configure corp_only on structure");
    sess.role = 0;

    for _ in 0..11 {
        sys_handle
            .step_manual(1.0)
            .await
            .expect("advance ticks past session timer");
    }
    let (_, mut dock_test_ball) = sys_handle
        .get_ball_snapshot(first_ship_id)
        .await
        .unwrap()
        .unwrap();
    dock_test_ball.position = struct_pos + DVec3::new(1_500.0, 0.0, 0.0);
    dock_test_ball.velocity = DVec3::ZERO;
    dock_test_ball.speed_fraction = 0.0;
    dock_test_ball.mode = MovementMode::Stop;
    sys_handle
        .spawn_ball(dock_test_ball)
        .await
        .expect("position ship at structure");

    // 4a. Deny pilot via structure docking ACL -> StructureDockingAccessDenied
    let acl_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDocking",
            None,
            "Dock",
            &[PyRep::Integer(struct_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(acl_err.code, "StructureDockingAccessDenied");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().owner_corp_id = 1_000_001;
    });
    sess.role = eve_machonet::service_roles::ROLE_GMH;
    eve_server::services::station_structure::structure_upwell_ops::configure_structure_access_profile(
        &ctx,
        &sess,
        struct_id,
        Some(false),
        &[],
        &[],
        &[],
        &[],
    )
    .expect("clear corp_only on structure");
    sess.role = 0;

    // 4b. Capital ship attempting to dock at Medium structure (Astrahus 35832) -> ShipTooLargeForStructure
    let (_, mut cap_ball) = sys_handle
        .get_ball_snapshot(first_ship_id)
        .await
        .unwrap()
        .unwrap();
    let orig_type_id = cap_ball.type_id;
    cap_ball.type_id = 23757; // Archon (Carrier, group 547)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&first_ship_id) {
            it.type_id = 23757;
        }
    });
    sys_handle
        .spawn_ball(cap_ball.clone())
        .await
        .expect("set ball to Archon");
    let size_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDocking",
            None,
            "Dock",
            &[PyRep::Integer(struct_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(size_err.code, "ShipTooLargeForStructure");
    cap_ball.type_id = orig_type_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&first_ship_id) {
            it.type_id = orig_type_id;
        }
    });
    sys_handle
        .spawn_ball(cap_ball)
        .await
        .expect("restore subcap ball");

    // 4c. Crimewatch Weapons Timer blocks structureDocking.Dock -> WeaponsTimerActive
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.trigger_weapon_timer(90_000_001, ctx.now_unix());
    }
    let struct_wpn_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDocking",
            None,
            "Dock",
            &[PyRep::Integer(struct_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(struct_wpn_err.code, "WeaponsTimerActive");
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(90_000_001).weapon_timer_until_unix = 0;
    }

    // 4d. Authorized subcap within docking perimeter docks and undocks cleanly (also exercise beyonce.CmdDock)
    ctx.advance_secs(12);
    let bid_dock = ctx.bind_object(&mut sess, "beyonce", 30_000_142).expect("bind beyonce");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(bid_dock),
        "CmdDock",
        &[PyRep::Integer(struct_id), PyRep::Integer(first_ship_id)],
        &kw,
        None,
    )
    .await
    .expect("beyonce.CmdDock at structure");
    ctx.advance_secs(12);
    ctx.dispatch_rpc(&mut sess, "structureDocking", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock after beyonce.CmdDock");
    let (_, mut dock_test_ball2) = sys_handle.get_ball_snapshot(first_ship_id).await.unwrap().unwrap();
    dock_test_ball2.position = struct_pos + DVec3::new(1_500.0, 0.0, 0.0);
    dock_test_ball2.mode = MovementMode::Stop;
    sys_handle.spawn_ball(dock_test_ball2).await.unwrap();

    ctx.advance_secs(12);
    let struct_dock = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDocking",
            None,
            "Dock",
            &[PyRep::Integer(struct_id)],
            &kw,
            None,
        )
        .await
        .expect("Dock at Upwell structure");
    assert!(matches!(struct_dock, PyRep::KeyVal(_)));
    assert_eq!(sess.structure_id, Some(struct_id));
    assert!(sys_handle.get_ball_snapshot(first_ship_id).await.unwrap().is_none());

    ctx.advance_secs(12);
    let struct_undock = ctx
        .dispatch_rpc(&mut sess, "structureDocking", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock from Upwell structure");
    assert!(matches!(struct_undock, PyRep::KeyVal(_)));
    assert_eq!(sess.structure_id, None);
    let (_, after_struct_undock_ball) = sys_handle
        .get_ball_snapshot(first_ship_id)
        .await
        .unwrap()
        .expect("Ship must be spawned back in space after undocking from Upwell structure");
    assert!(after_struct_undock_ball.position.distance(struct_pos) < 5_000.0);
    assert!(after_struct_undock_ball.velocity.length() > 10.0);
}
