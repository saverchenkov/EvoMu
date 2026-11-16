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
    let PyRep::Tuple(ship_state) = board_res else {
        panic!("Expected 4-tuple shipState from ship.Board, got {board_res:?}");
    };
    assert_eq!(ship_state.len(), 4);
    assert!(ship_state.iter().all(|part| matches!(part, PyRep::Dict(_))));
    assert_eq!(sess.ship_id, Some(second_ship_id));

    let board_notifs = ctx.drain_notifications_for_char(char_id);
    // The server announces the ship change as an `OnSessionChanged` `shipid` (old, new) change
    // and primes godma with `ProcessGodmaPrimeLocation`;
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
        board_notifs.iter().any(|n| n.event == "ProcessGodmaPrimeLocation"
            && n.args.first() == Some(&PyRep::Integer(second_ship_id))),
        "ship.Board must emit ProcessGodmaPrimeLocation for newly boarded ship"
    );
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
    let expected_offset = st_radius + ship_radius + 500.0;
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
        .set_ball_position(first_ship_id, station_pos + DVec3::new(150_000.0, 0.0, 0.0))
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
    assert_eq!(dock_crim_err.code, "CantDockGlobalCriminalFlag");

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

#[allow(dead_code, unused_imports)]
pub(crate) mod jump_activation_and_cyno {
    use super::*;

//  Integration tests for `SP-3` (Warp Navigation, Disruption, Capacitor Cost, & Exit Invulnerability).

use crate::common;

use eve_db::types::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_sp3_warp_to_disrupted_capacitor_cost_and_exit_invulnerability() {
    // parity: plan:SP-3
    // parity: feat:navigation.warp-to, feat:navigation.warp-disrupted, feat:navigation.warp-capacitor-cost, feat:navigation.warp-exit-invulnerability, rpc:beyonce.UpdateStateRequest, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:navigation.warp-to", "feat:navigation.warp-disrupted", "feat:navigation.warp-capacitor-cost", "feat:navigation.warp-exit-invulnerability"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(3002);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            89_001_001,
            InventoryItemRecord {
                item_id: 89_001_001,
                type_id: 53977,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
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

    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    let beyonce_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // 1. Warp Disruption & Interdiction Bubble checks (`feat:navigation.warp-disrupted`)
    let station_pos = ctx.sde.stations[&60003760].position;
    let start_pos = station_pos - DVec3::new(10_000_000.0, 0.0, 0.0);
    sys_handle.set_ball_position(ship_id, start_pos).await;

    let mut cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    cs.warp_scramble_strength = 2;
    cs.warp_core_strength = 0;
    sys_handle.set_combat_state(ship_id, cs.clone()).await.unwrap();

    let scram_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToStuff",
            &[
                PyRep::String("item".into()),
                PyRep::Integer(60003760),
                PyRep::Real(20_000.0),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(scram_err.code, "WarpScrambled");

    cs.warp_scramble_strength = 0;
    sys_handle.set_combat_state(ship_id, cs.clone()).await.unwrap();
    let bubble_id = 920_001;
    sys_handle
        .add_warp_bubble(bubble_id, start_pos, 20_000.0)
        .await
        .expect("add warp bubble");

    let bubble_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToStuff",
            &[
                PyRep::String("item".into()),
                PyRep::Integer(60003760),
                PyRep::Real(20_000.0),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(bubble_err.code, "WarpScrambled");
    sys_handle
        .remove_warp_bubble(bubble_id)
        .await
        .expect("remove warp bubble");

    // 2. Warp Capacitor Cost & Partial Warp (`feat:navigation.warp-capacitor-cost`)
    let mut low_cap_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    let full_cap = low_cap_cs.max_capacitor;
    low_cap_cs.capacitor = 0.000_001;
    sys_handle
        .set_combat_state(ship_id, low_cap_cs.clone())
        .await
        .unwrap();

    let cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdWarpToStuff",
            &[
                PyRep::String("item".into()),
                PyRep::Integer(60003760),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(cap_err.code, "NotEnoughCapacitor");

    let far_warp_start = station_pos - DVec3::new(2.0 * eve_destiny::AU_METERS, 0.0, 0.0);
    sys_handle.set_ball_position(ship_id, far_warp_start).await;
    let (_, ship_b) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let warp_cap_need = ctx
        .sde
        .types
        .get(&ship_b.type_id)
        .and_then(|t| t.default_attributes.get(&153).copied())
        .unwrap_or(1e-6);
    let wdo_lvl = ctx.db.with_snapshot(|snap| {
        snap.skills
            .get(&char_id)
            .and_then(|m| m.get(&3455))
            .map(|s| s.level)
            .unwrap_or(0)
    });
    let eff_warp_cap_need = warp_cap_need * (1.0 - 0.10 * (wdo_lvl.min(5) as f64));
    let full_2au_cost = eff_warp_cap_need * ship_b.mass * 2.0;
    low_cap_cs.capacitor = full_2au_cost * 0.5;
    sys_handle
        .set_combat_state(ship_id, low_cap_cs.clone())
        .await
        .unwrap();

    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[
            PyRep::String("item".into()),
            PyRep::Integer(60003760),
            PyRep::Real(0.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Partial warp with 50% capacitor");

    let (_, partial_warp_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let partial_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(partial_cs.capacitor < 1e-6);
    let MovementMode::Warp {
        destination: partial_dest,
        total_distance: partial_dist,
        ..
    } = partial_warp_ball.mode
    else {
        panic!("Expected MovementMode::Warp for partial warp");
    };
    assert!((partial_dist - eve_destiny::AU_METERS).abs() < 1e6);
    assert!(
        partial_dest.distance(far_warp_start + DVec3::new(eve_destiny::AU_METERS, 0.0, 0.0)) < 1e6
    );
    let cap_notes = sess.drain_notifications();
    assert!(cap_notes.iter().any(|n| n.0 == "OnCapacitorChange"));
    assert!(!cap_notes.iter().any(|n| n.0 == "OnWarpStarted"));

    // 3. Full Warp with minRange (30 km), phases Accelerating -> Cruising -> Decelerating, smartbomb hit in Decelerating,
    //    and 10s Post-Warp Invulnerability (`feat:navigation.warp-to`, `feat:navigation.warp-exit-invulnerability`)
    let mut full_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    full_cs.capacitor = full_cap;
    sys_handle.set_combat_state(ship_id, full_cs).await.unwrap();
    let (_, mut reset_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    reset_ball.position = start_pos;
    reset_ball.velocity = DVec3::ZERO;
    reset_ball.speed_fraction = 0.0;
    reset_ball.mode = MovementMode::Stop;
    sys_handle.spawn_ball(reset_ball).await.unwrap();

    let min_range = 30_000.0;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[
            PyRep::String("item".into()),
            PyRep::Integer(60003760),
            PyRep::Real(min_range),
        ],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuff at 30 km minRange");

    let station_radius = ctx
        .sde
        .types
        .get(&ctx.sde.stations[&60003760].station_type_id)
        .map(|t| t.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or(0.0);
    let expected_landing = station_pos
        - DVec3::new(
            station_radius
                + eve_server::services::nav_warp_interdiction::STATION_DOCKING_RING_STANDOFF_M
                + min_range,
            0.0,
            0.0,
        );
    let mut saw_accel = false;
    let mut saw_decel_and_tested_smartbomb = false;
    for _ in 0..40 {
        sys_handle.step_manual(1.0).await.expect("step warp progression");
        let (_, b_w) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        match b_w.mode {
            MovementMode::Warp {
                phase: eve_destiny::WarpPhase::Accelerating,
                ..
            } => {
                saw_accel = true;
            }
            MovementMode::Warp {
                phase: eve_destiny::WarpPhase::Decelerating,
                ..
            } => {
                if !saw_decel_and_tested_smartbomb {
                    saw_decel_and_tested_smartbomb = true;
                    let cs_pre_sb = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
                    sys_handle
                        .apply_damage(
                            ship_id,
                            eve_destiny::DamageVector {
                                em: 50.0,
                                thermal: 0.0,
                                kinetic: 0.0,
                                explosive: 0.0,
                            },
                        )
                        .await
                        .unwrap();
                    let cs_post_sb = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
                    assert!(cs_post_sb.shield_hp < cs_pre_sb.shield_hp);
                }
            }
            MovementMode::Stop => break,
            _ => {}
        }
    }
    assert!(saw_accel);
    assert!(saw_decel_and_tested_smartbomb);

    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "UpdateStateRequest",
        &[],
        &kw,
        None,
    )
    .await
    .expect("UpdateStateRequest after warp");
    let fin_notes = sess.drain_notifications();
    assert!(fin_notes.iter().any(|n| n.0 == "OnWarpFinished"));

    let (_, landed_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(landed_ball.mode, MovementMode::Stop);
    assert!(landed_ball.position.distance(expected_landing) <= 2_500.0);

    let post_warp_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(post_warp_cs.warp_exit_invulnerable_remaining_ticks, 10);

    let hp_at_warp_exit = post_warp_cs.shield_hp;
    sys_handle
        .apply_damage(
            ship_id,
            eve_destiny::DamageVector {
                em: 200.0,
                thermal: 200.0,
                kinetic: 200.0,
                explosive: 200.0,
            },
        )
        .await
        .unwrap();
    let cs_after_invuln_hit = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_after_invuln_hit.shield_hp, hp_at_warp_exit);

    let attacker_ball_id = 920_099;
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: attacker_ball_id,
                type_id: 587,
                position: landed_ball.position + DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await
        .unwrap();
    assert!(
        sys_handle
            .add_target_lock(
                attacker_ball_id,
                eve_destiny::TargetLock {
                    target_id: ship_id,
                    lock_time_ms: 1_000.0,
                    remaining_lock_ms: 1_000.0,
                    locked: false,
                    max_range_m: Some(50_000.0),
                },
            )
            .await
            .is_err()
    );
    let attacker_locks = sys_handle.get_target_locks(attacker_ball_id).await.unwrap();
    assert!(attacker_locks.is_empty());

    ctx.advance_secs(12);
    let fil_err = ctx
        .dispatch_rpc(
            &mut sess,
            "randomJumpMgr",
            None,
            "ActivateRandomJumpFilament",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(fil_err.code, "ShipInvulnerable");

    let abyss_unimpl = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceDeployment",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(abyss_unimpl.code, "MissingRequiredArgument");

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
    .expect("CmdSetSpeedFraction breaks post-warp invulnerability");
    let cs_after_move = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_after_move.warp_exit_invulnerable_remaining_ticks, 0);
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod station_docking_and_tether {
    use super::*;

//  Integration tests for `SP-4` (Stargate Capital Restrictions, Session-Change Timers, & Jump Cloak Immunities).

use crate::common;

use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_sp4_stargate_capital_restriction_session_timers_and_jump_cloak_immunities() {
    // parity: plan:SP-4
    // parity: feat:navigation.stargate-capital-restriction, feat:navigation.jump-cloak-immunities, rpc:beyonce.SetSessionCheck, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:navigation.stargate-capital-restriction", "feat:navigation.jump-cloak-immunities"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(3003);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
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

    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    let beyonce_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let jita_sys = 30_000_142;
    let jita_handle = ctx.systems.get_or_spawn(jita_sys);

    let jita_gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == jita_sys)
        .cloned()
        .expect("Jita stargate");
    let gate_id = jita_gate.stargate_id;
    let gate_pos = jita_gate.position;

    jita_handle
        .set_ball_position(ship_id, gate_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;

    // 1. Stargate Capital Ship Restrictions (`feat:navigation.stargate-capital-restriction`)
    for &(cap_type_id, expected_group_id) in &[
        (23757_u32, 547_u32),  // Archon (Carrier)
        (19720_u32, 485_u32),  // Revelation (Dreadnought)
        (37604_u32, 1538_u32), // Apostle (Force Auxiliary)
        (23919_u32, 659_u32),  // Aeon (Supercarrier)
        (11567_u32, 30_u32),   // Avatar (Titan)
        (77281_u32, 4594_u32), // Bane (Lancer Dreadnought)
        (28352_u32, 883_u32),  // Rorqual (Capital Industrial Ship)
    ] {
        assert_eq!(ctx.sde.types[&cap_type_id].group_id, expected_group_id);
        let (_, mut b) = jita_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        b.type_id = cap_type_id;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&ship_id) {
                it.type_id = cap_type_id;
            }
        });
        jita_handle.spawn_ball(b).await.unwrap();

        ctx.advance_secs(12);
        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(beyonce_id),
                "CmdStargateJump",
                &[PyRep::Integer(gate_id)],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "CapitalShipCannotJumpStargate");
    }

    for &(allowed_type_id, expected_group_id) in &[
        (20183_u32, 513_u32), // Providence (Freighter)
        (28844_u32, 902_u32), // Ark (Jump Freighter)
        (28606_u32, 941_u32), // Orca (Industrial Command Ship)
        (34328_u32, 513_u32), // Bowhead (Freighter)
    ] {
        assert_eq!(ctx.sde.types[&allowed_type_id].group_id, expected_group_id);
    }

    let (_, mut orca_ball) = jita_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    orca_ball.type_id = 28606; // Orca
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.type_id = 28606;
        }
    });
    jita_handle.spawn_ball(orca_ball).await.unwrap();

    let jump_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(gate_id)],
            &kw,
            None,
        )
        .await
        .expect("Orca (28606) must be allowed to jump normal stargates");
    let PyRep::KeyVal(jkv) = jump_ok else {
        panic!("Expected KeyVal from CmdStargateJump");
    };
    let dest_sys_id = jkv.get("solarSystemID").unwrap().as_i64().unwrap() as u32;
    let dest_beyonce_id = jkv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    assert_eq!(dest_sys_id, jita_gate.to_solar_system_id);
    assert_eq!(jkv.get("jumpFatigueMinutes").unwrap().as_f64().unwrap(), 0.0);
    assert_eq!(jkv.get("jumpCooldownMinutes").unwrap().as_f64().unwrap(), 0.0);

    let dest_handle = ctx.systems.get_or_spawn(dest_sys_id);

    // 2. Post-Jump Cloak (60s) Immunities (`feat:navigation.jump-cloak-immunities`)
    let dest_cs = dest_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(dest_cs.is_cloaked);
    assert_eq!(dest_cs.gate_cloak_remaining_ticks, 60);

    let shield_before_bomb = dest_cs.shield_hp;
    dest_handle
        .apply_damage(
            ship_id,
            eve_destiny::DamageVector {
                em: 500.0,
                thermal: 500.0,
                kinetic: 500.0,
                explosive: 500.0,
            },
        )
        .await
        .unwrap();
    let dest_cs_after_bomb = dest_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(dest_cs_after_bomb.shield_hp, shield_before_bomb);

    let (_, jumped_ball) = dest_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let ecm_source_id = 930_001;
    dest_handle
        .spawn_ball_with_combat(
            Ball {
                id: ecm_source_id,
                type_id: 587,
                position: jumped_ball.position + DVec3::new(100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await
        .unwrap();
    let jammed_ids = dest_handle
        .apply_ecm_burst(
            ecm_source_id,
            5_000.0,
            [100.0, 100.0, 100.0, 100.0],
            1.0,
            0.0,
        )
        .await
        .unwrap();
    assert!(!jammed_ids.contains(&ship_id));

    let bumper_id = 930_002;
    dest_handle
        .spawn_ball_with_combat(
            Ball {
                id: bumper_id,
                type_id: 642,
                position: jumped_ball.position + DVec3::new(10.0, 0.0, 0.0),
                velocity: DVec3::new(-500.0, 0.0, 0.0),
                max_velocity: 500.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 200.0,
                speed_fraction: 1.0,
                mode: MovementMode::GotoDirection {
                    direction: DVec3::NEG_X,
                },
            },
            ctx.build_type_combat_state(642),
        )
        .await
        .unwrap();
    dest_handle
        .step_manual(1.0)
        .await
        .expect("step with overlapping bumper");
    let (_, jumped_ball_after_bump) = dest_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let cs_after_bump = dest_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_after_bump.is_cloaked && cs_after_bump.gate_cloak_remaining_ticks == 59);
    assert_eq!(jumped_ball_after_bump.velocity, DVec3::ZERO);
    let _ = dest_handle.remove_ball(bumper_id).await;
    let _ = dest_handle.remove_ball(ecm_source_id).await;

    // 3. 10s Session-Change Timer Triggers (`feat:navigation.session-change-timer-triggers`)
    let return_gate_id = jita_gate.destination_stargate_id;
    let return_gate_pos = ctx.sde.jumps[&return_gate_id].position;
    dest_handle
        .set_ball_position(ship_id, return_gate_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;

    let early_jump_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(dest_beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(return_gate_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(early_jump_err.code, "SessionChangeInProgress");

    ctx.advance_secs(eve_server::services::space_destiny::STARGATE_SESSION_CHANGE_TIMER_SECS);
    for _ in 0..10 {
        dest_handle.step_manual(1.0).await.unwrap();
    }

    for trigger in [
        "login",
        "wormhole_jump",
        "jump_bridge",
        "jump_drive",
        "dock",
        "undock",
        "fleet_join",
        "fleet_position_change",
        "ship_board",
        "ship_eject",
        "pod_transition",
        "clone_jump",
        "clone_swap",
        "corp_join",
        "corp_leave",
        "wallet_division_change",
    ] {
        let trig_res = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(dest_beyonce_id),
                "SetSessionCheck",
                &[PyRep::String(trigger.into())],
                &kw,
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("SetSessionCheck({trigger}) failed: {e:?}"));
        let PyRep::KeyVal(tkv) = trig_res else {
            panic!("Expected KeyVal from SetSessionCheck");
        };
        assert_eq!(
            tkv.get("sessionChangeTimerSeconds")
                .unwrap()
                .as_i64()
                .unwrap(),
            10
        );
        if trigger == "jump_drive" {
            assert!(
                !eve_server::services::space_destiny::is_location_change_invulnerable(
                    &ctx,
                    char_id,
                    ctx.now_unix()
                ),
                "#48: client SetSessionCheck(\"jump_drive\") must not grant location-change invulnerability"
            );
        }

        let blocked = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(dest_beyonce_id),
                "SetSessionCheck",
                &[PyRep::String(trigger.into())],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(blocked.code, "SessionChangeInProgress");

        for _ in 0..10 {
            dest_handle.step_manual(1.0).await.unwrap();
        }
    }

    let jump_back = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(dest_beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(return_gate_id)],
            &kw,
            None,
        )
        .await
        .expect("Jump back to Jita after 10s session timer elapsed");
    let PyRep::KeyVal(jb_kv) = jump_back else {
        panic!("Expected KeyVal from CmdStargateJump");
    };
    let jita_b_id = jb_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let cs_cloaked = jita_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_cloaked.is_cloaked && cs_cloaked.gate_cloak_remaining_ticks == 60);

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(jita_b_id),
        "CmdSetSpeedFraction",
        &[PyRep::Real(1.0)],
        &kw,
        None,
    )
    .await
    .expect("Move ship to break gate cloak");
    let cs_uncloaked = jita_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_uncloaked.is_cloaked && cs_uncloaked.gate_cloak_remaining_ticks == 0);
}

#[tokio::test]
async fn test_r_nav_01_structure_fuel_flag_172_ansiblex_and_jettison() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(3004);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");
    let corp_id = ctx.db.get_character(char_id).unwrap().corporation_id;
    let ship_id = ctx.db.get_character(char_id).unwrap().active_ship_id;
    let structure_id = 1_000_000_000_001i64;
    let fuel_item_id = 990_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(structure_id);
            ch.solar_system_id = 30_000_142;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = structure_id;
            ship.flag = eve_db::InventoryFlag::Hangar;
        }
        snap.structures.insert(structure_id, eve_db::UpwellStructureRecord {
            structure_id,
            solar_system_id: 30_000_142,
            type_id: 35832,
            owner_corp_id: corp_id,
            name: "Test Structure".to_string(),
            position: [0.0, 0.0, 0.0],
            state: "ShieldVulnerable".to_string(),
            quantum_core_installed: true,
            fuel_blocks: 0,
            shield_hp: 10_000_000.0,
            armor_hp: 10_000_000.0,
            hull_hp: 10_000_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        });
        snap.items.insert(fuel_item_id, eve_db::InventoryItemRecord {
            item_id: fuel_item_id,
            type_id: 4247, // Nitrogen Fuel Block
            owner_id: char_id,
            location_id: ship_id,
            flag: eve_db::InventoryFlag::Cargo,
            quantity: 500,
            singleton: false,
            ..Default::default()
        });
    });

    assert_eq!(eve_db::InventoryFlag::StructureFuel as u16, 172);
    assert_eq!(eve_db::InventoryFlag::from_u16(172), eve_db::InventoryFlag::StructureFuel);

    sess.structure_id = Some(structure_id);
    sess.station_id = Some(structure_id);
    sess.corporation_id = Some(corp_id);
    sess.ship_id = Some(ship_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.entry(corp_id).or_default().member_roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER);
    });

    // 1. Verify invbroker.Add whitelist rejects restricted flags 158..=163
    for bad_flag in 158..=163 {
        let err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(fuel_item_id), PyRep::Integer(structure_id), PyRep::Integer(bad_flag)], &kw, None).await.expect_err("rejected");
        assert_eq!(err.code, "InvalidInventoryFlag");
    }

    // 2. R-FUEL-03: Deposit fuel at flag 172 -> verify atomic fuel_blocks increment from 0 to 500
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(fuel_item_id), PyRep::Integer(structure_id), PyRep::Integer(172)], &kw, None).await.expect("invbroker.Add flag 172");
    let st_after_dep = ctx.db.with_snapshot(|s| s.structures.get(&structure_id).cloned().unwrap());
    assert_eq!(st_after_dep.fuel_blocks, 500, "R-FUEL-03: deposit at 172 must increment fuel_blocks to 500");

    // 3. JettisonStructureFuel -> verify post-jettison state and fuel_blocks decremented to 0
    ctx.dispatch_rpc(&mut sess, "structure", None, "JettisonStructureFuel", &[PyRep::List(vec![PyRep::Integer(fuel_item_id)])], &kw, None).await.expect("JettisonStructureFuel");
    let moved_fuel = ctx.db.get_item(fuel_item_id).expect("fuel item post-jettison");
    assert_eq!(moved_fuel.flag, eve_db::InventoryFlag::Cargo);
    assert_ne!(moved_fuel.location_id, structure_id);
    assert_eq!(moved_fuel.quantity, 500);
    let spawned_container = ctx.db.get_item(moved_fuel.location_id).expect("spawned space container");
    assert_eq!(spawned_container.type_id, 23);
    assert_eq!(spawned_container.location_id, 30_000_142);
    let st_after_jet = ctx.db.with_snapshot(|s| s.structures.get(&structure_id).cloned().unwrap());
    assert_eq!(st_after_jet.fuel_blocks, 0, "R-FUEL-03: jettison must decrement fuel_blocks to 0");

    // 4. R-FUEL-01: Withdrawing fuel from flag 172 requires fuel bay corp role
    let fuel2_id = 990_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&structure_id) { st.fuel_blocks = 20; }
        snap.items.insert(fuel2_id, eve_db::InventoryItemRecord {
            item_id: fuel2_id, type_id: 4247, owner_id: corp_id,
            location_id: structure_id, flag: eve_db::InventoryFlag::StructureFuel,
            quantity: 20, singleton: false, ..Default::default()
        });
    });
    sess.corp_role = 0;
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.entry(corp_id).or_default().member_roles.clear();
    });
    let denied = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(fuel2_id), PyRep::Integer(ship_id), PyRep::Integer(5)], &kw, None).await;
    let err_denied = denied.expect_err("R-FUEL-01: withdrawal without role must be rejected");
    assert_eq!(err_denied.code, "CorpRoleDenied");
    let item_denied = ctx.db.get_item(fuel2_id).unwrap();
    assert_eq!(item_denied.owner_id, corp_id);
    assert_eq!(item_denied.location_id, structure_id);

    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.entry(corp_id).or_default().member_roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
    });
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(fuel2_id), PyRep::Integer(ship_id), PyRep::Integer(5)], &kw, None).await.expect("withdrawal with role");
    let item_withdrawn = ctx.db.get_item(fuel2_id).unwrap();
    assert_eq!(item_withdrawn.owner_id, char_id);
    assert_eq!(item_withdrawn.location_id, ship_id);
    let st_after_w = ctx.db.with_snapshot(|s| s.structures.get(&structure_id).cloned().unwrap());
    assert_eq!(st_after_w.fuel_blocks, 0, "R-FUEL-01: withdrawal must decrement fuel_blocks");

    // 5. R-FUEL-02: Legacy quantum core stored at flag 172 is locked against removal
    let legacy_core_id = 990_003i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(legacy_core_id, eve_db::InventoryItemRecord {
            item_id: legacy_core_id, type_id: 56201, // Astrahus Upwell Quantum Core
            owner_id: corp_id, location_id: structure_id,
            flag: eve_db::InventoryFlag::StructureFuel, // legacy flag 172
            quantity: 1, singleton: false, ..Default::default()
        });
    });
    let core_rem = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(legacy_core_id), PyRep::Integer(ship_id), PyRep::Integer(5)], &kw, None).await;
    let core_err = core_rem.expect_err("R-FUEL-02: removal of quantum core at flag 172 must fail");
    assert_eq!(core_err.code, "ItemLocked");
    let core_item = ctx.db.get_item(legacy_core_id).unwrap();
    assert_eq!(core_item.location_id, structure_id, "core must remain in structure");
}

#[tokio::test]
async fn test_r_nav_02_sync_tether_immobilizing_module_lock_order() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142;
    let structure_id = 1_000_000_000_002i64;

    let corp_id = ctx.db.get_character(char_id).unwrap().corporation_id;
    let structure_pos = DVec3::new(100.0, 0.0, 0.0);

    let siege_mod_id = 880_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
        snap.structures.insert(structure_id, eve_db::UpwellStructureRecord {
            structure_id,
            solar_system_id: sys_id,
            type_id: 35832, // Astrahus
            owner_corp_id: corp_id, // matching pilot corp for structure access
            name: "Test Tether Structure".to_string(),
            position: structure_pos.to_array(),
            state: "ShieldVulnerable".to_string(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            shield_hp: 10_000_000.0,
            armor_hp: 10_000_000.0,
            hull_hp: 10_000_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        });
        // Siege module fitted on ship (initially inactive)
        snap.items.insert(siege_mod_id, eve_db::InventoryItemRecord {
            item_id: siege_mod_id,
            type_id: 20280, // Siege Module I (disallowTethering)
            owner_id: char_id,
            location_id: ship_id,
            flag: eve_db::InventoryFlag::HiSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        });
    });

    let handle = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id: 28659, // Paladin
        position: structure_pos, // inside tether range
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 28659);
    handle.spawn_ball_with_combat(ball, cs).await.unwrap();

    // 1. Positive control: ship within range of accessible cored structure must tether
    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, char_id, sys_id, ship_id).await;
    let cs_tethered = handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_tethered.is_tethered, true, "Ship within range of accessible structure must tether (positive control)");

    // 2. Negative control & lock order: activate siege module (disallowTethering)
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(siege_mod_id);
    });

    // Verify sync_tether completes promptly without deadlock or recursive lock panic
    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, char_id, sys_id, ship_id).await;

    let cs_untethered = handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_untethered.is_tethered, false, "Ship with active siege module must break tether");
}

}


#[allow(unused_imports)]
pub mod jump_cloak {


use eve_db::{
    CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes,
    TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use glam::DVec3;
use std::collections::HashMap;

const JITA_SYSTEM: u32 = 30_000_142;
const SOV_NULL_A: u32 = 30_004_712; // NOL-M9 (Delve sov null-sec)
const SOV_NULL_B: u32 = 30_004_713; // O-IOAI (Delve sov null-sec, ~1.10 LY from NOL-M9)
const SOV_NULL_C: u32 = 30_004_727; // R5-MM8 (Delve sov null-sec, ~3.85 LY from O-IOAI)
const SOV_NULL_FAR: u32 = 30_004_752; // QY6-RK (Delve sov null-sec, ~6.36 LY > 5.0 LY from NOL-M9)
const PILOT_1: i64 = 93_100_001;
const PILOT_2: i64 = 93_100_002;
const SHIP_1: i64 = 83_100_001;
const SHIP_2: i64 = 83_100_002;

pub(crate) fn make_nav_character(char_id: i64, ship_id: i64, sys_id: u32) -> CharacterRecord {
    CharacterRecord {
        character_id: char_id, user_id: char_id as u64, name: format!("NavPilot {char_id}"),
        corporation_id: 1_000_001, alliance_id: None, war_faction_id: None,
        station_id: None, structure_id: None, solar_system_id: sys_id,
        constellation_id: 20_000_020, region_id: 10_000_002, active_ship_id: ship_id,
        balance: IskCents::from_isk(100_000_000), plex_balance: 0,
        loyalty_points: HashMap::new(), standings: HashMap::new(),
        security_status: 5.0, clone_grade: 2, home_station_id: 60_003_760,
        race_id: 1, bloodline_id: 1, ancestry_id: 1, school_id: 1,
        neural: NeuralAttributes::default(), skill_points: 15_000_000.0,
        activity_status: None, free_skill_points: 0.0, last_clone_jump_unix: 0,
        jump_fatigue_minutes: 0.0, jump_cooldown_until_unix: 0, jump_fatigue_until_unix: 0,
        description: String::new(), title: String::new(), gender: 1,
        create_date_unix: 1_700_000_000, fleet_id: None, autopilot_waypoints: Vec::new(),
        version: 1, system_epoch: 0,
    }
}

pub(crate) fn make_structure(structure_id: i64, type_id: u32, sys_id: u32, name: &str, pos: [f64; 3]) -> eve_db::types::UpwellStructureRecord {
    eve_db::types::UpwellStructureRecord {
        structure_id, type_id, owner_corp_id: 1_000_001, solar_system_id: sys_id,
        name: name.into(), position: pos, state: "ShieldVulnerable".into(),
        quantum_core_installed: true, fuel_blocks: 1000,
        shield_hp: 10_000_000.0, armor_hp: 10_000_000.0, hull_hp: 10_000_000.0,
        reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

pub(crate) async fn setup_nav_pilot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let all_skills: HashMap<u32, TrainedSkillRecord> = ctx
        .sde
        .types
        .values()
        .filter(|t| t.category_id == 16)
        .map(|t| {
            (
                t.type_id,
                TrainedSkillRecord {
                    type_id: t.type_id,
                    level: 5,
                    skill_points: 256_000.0,
                },
            )
        })
        .collect();
    let uid = ctx
        .db
        .provision_player_account(&format!("nav_pilot_{char_id}"), "nav-secret");
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.insert(char_id, all_skills);
        let mut ch = make_nav_character(char_id, ship_id, sys_id);
        ch.user_id = uid;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 10_000_000.0,
                radius: 120.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            cs,
        )
        .await;

    let session_id = 8_000 + char_id as u64;
    let _ = ctx.db.acquire_online_lock_with_computer_hash(
        uid,
        session_id,
        Some(char_id),
        Some(format!("nav_hash_{char_id}")),
        true,
        ctx.now_unix(),
    );
    let mut sess = SessionState::new(session_id);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_001);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess
}

// parity: feat:navigation.ansiblex-jump-fatigue-exemption
// parity: feat:navigation.bastion-siege-style-gate-blocks
#[tokio::test]
async fn test_space_navigation_ansiblex_and_bastion_blocks() {
    eve_testkit::attest(&[
        "feat:navigation.ansiblex-jump-fatigue-exemption",
        "feat:navigation.bastion-siege-style-gate-blocks",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let ship1_pos = DVec3::new(10_000_000.0, 0.0, 0.0);
    let mut s1 = setup_nav_pilot(&ctx, PILOT_1, SHIP_1, 28661, SOV_NULL_A, ship1_pos).await; // Kronos Marauder in NOL-M9
    let mut s2 = setup_nav_pilot(
        &ctx,
        PILOT_2,
        SHIP_2,
        11999,
        JITA_SYSTEM,
        ship1_pos + DVec3::new(250_000.0, 0.0, 0.0),
    )
    .await;

    let sov_a_sys = ctx.systems.get_or_spawn(SOV_NULL_A);
    let jita_sys = ctx.systems.get_or_spawn(JITA_SYSTEM);
    let kw = HashMap::new();

    // =========================================================================
    // 1. feat:navigation.ansiblex-jump-fatigue-exemption
    // =========================================================================
    let gate_a_id = 1_000_600_001i64;
    let gate_b1_id = 1_000_600_002i64;
    let gate_b2_id = 1_000_600_003i64;
    let gate_c_id = 1_000_600_004i64;
    let gate_far_id = 1_000_600_005i64;
    let gate_highsec_id = 1_000_600_006i64;
    let cargo_ozone_id = 91_600_000i64;
    let fuel_ozone_1a_id = 91_600_001i64;
    let fuel_ozone_1b_id = 91_600_011i64;
    let bastion_mod_id = 91_600_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(gate_a_id, make_structure(gate_a_id, 35841, SOV_NULL_A, "NOL-M9 Ansiblex", ship1_pos.to_array()));
        snap.structures.insert(gate_b1_id, make_structure(gate_b1_id, 35841, SOV_NULL_B, "O-IOAI Ansiblex A", [1_000.0, 0.0, 0.0]));
        snap.structures.insert(gate_b2_id, make_structure(gate_b2_id, 35841, SOV_NULL_B, "O-IOAI Ansiblex B", [2_500.0, 0.0, 0.0]));
        snap.structures.insert(gate_c_id, make_structure(gate_c_id, 35841, SOV_NULL_C, "R5-MM8 Ansiblex", [5_000.0, 0.0, 0.0]));
        snap.structures.insert(gate_far_id, make_structure(gate_far_id, 35841, SOV_NULL_FAR, "QY6-RK Far Ansiblex", [5_000.0, 0.0, 0.0]));
        snap.structures.insert(gate_highsec_id, make_structure(gate_highsec_id, 35841, JITA_SYSTEM, "Highsec Invalid Gate", [5_000.0, 0.0, 0.0]));
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(gate_a_id, gate_b1_id);
        ext.jump_bridge_links.insert(gate_b1_id, gate_a_id);
        ext.jump_bridge_links.insert(gate_b2_id, gate_c_id);
        ext.jump_bridge_links.insert(gate_c_id, gate_b2_id);
        save_ext_state(snap, &ext);

        // Non-fuel-bay Liquid Ozone (Cargo) MUST NOT count or be deducted
        snap.items.insert(
            cargo_ozone_id,
            InventoryItemRecord {
                item_id: cargo_ozone_id,
                type_id: 16273,
                owner_id: 1_000_001,
                location_id: gate_a_id,
                flag: InventoryFlag::Cargo,
                quantity: 10_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Fit Bastion Module I (type 33400) on Kronos (28661)
        snap.items.insert(
            bastion_mod_id,
            InventoryItemRecord {
                item_id: bastion_mod_id,
                type_id: 33400,
                owner_id: PILOT_1,
                location_id: SHIP_1,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(SHIP_1);

    // V24 GetLinkedStructure returns (linkedStructureID, linkedSolarSystemID) 2-tuple or None when unlinked
    let link_res = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "GetLinkedStructure", &[PyRep::Integer(gate_a_id)], &kw, None)
        .await;
    assert_eq!(link_res.expect("GetLinkedStructure"), PyRep::Tuple(vec![PyRep::Integer(gate_b1_id), PyRep::Integer(SOV_NULL_B as i64)]));
    let unlinked_res = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "GetLinkedStructure", &[PyRep::Integer(gate_far_id)], &kw, None)
        .await;
    assert_eq!(unlinked_res.expect("GetLinkedStructure unlinked"), PyRep::None);

    // Negative 0: Cargo Liquid Ozone alone fails with InsufficientLiquidOzone (must be StructureFuel)
    let no_fuel_res = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None)
        .await;
    assert_eq!(no_fuel_res.unwrap_err().code, "InsufficientLiquidOzone");

    // Seed StructureFuel stacks (first stack 30 units will be fully depleted & removed!)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fuel_ozone_1a_id,
            InventoryItemRecord {
                item_id: fuel_ozone_1a_id,
                type_id: 16273,
                owner_id: 1_000_001,
                location_id: gate_a_id,
                flag: InventoryFlag::StructureFuel,
                quantity: 30,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            fuel_ozone_1b_id,
            InventoryItemRecord {
                item_id: fuel_ozone_1b_id,
                type_id: 16273,
                owner_id: 1_000_001,
                location_id: gate_a_id,
                flag: InventoryFlag::StructureFuel,
                quantity: 50_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Negative 0b: Link > 5.0 LY (NOL-M9 -> QY6-RK ~6.36 LY) and non-sov-null (Jita) rejected
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(gate_a_id, gate_far_id);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None).await.unwrap_err().code,
        "JumpBridgeOutOfRange"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(gate_a_id, gate_highsec_id);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None).await.unwrap_err().code,
        "JumpBridgeRequiresSovNull"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(gate_a_id, gate_b1_id);
        save_ext_state(snap, &ext);
    });

    // Negative 1: Offline / unpowered Ansiblex refused (`fuel_blocks == 0`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&gate_a_id).unwrap().fuel_blocks = 0;
    });
    let unpowered_res = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None)
        .await;
    assert_eq!(unpowered_res.unwrap_err().code, "StructureNotOperational");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&gate_a_id).unwrap().fuel_blocks = 1000;
    });

    // Negative 2: Weapons timer active refused
    let now = ctx.now_unix();
    ctx.crimewatch.write().unwrap().states.entry(PILOT_1).or_default().weapon_timer_until_unix = now + 60;
    let wt_res = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None)
        .await;
    assert_eq!(wt_res.unwrap_err().code, "WeaponsTimerActive");
    ctx.crimewatch.write().unwrap().states.entry(PILOT_1).or_default().weapon_timer_until_unix = 0;

    // Negative 3 (Clause 5): Active Bastion Module I via live dogmaIM.Activate refuses Ansiblex jump!
    let act_bastion_jb = ctx
        .dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(bastion_mod_id)], &kw, None)
        .await;
    assert!(act_bastion_jb.is_ok(), "Bastion activate via dogmaIM.Activate: {act_bastion_jb:?}");
    let bastion_jb_res = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None)
        .await;
    assert_eq!(bastion_jb_res.unwrap_err().code, "CantJumpWhileModuleActive");
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(bastion_mod_id)], &kw, None)
        .await
        .expect("Deactivate Bastion before Ansiblex jump");
    ctx.advance_secs(65); // Clear any module reactivation cooldown

    // Negative 4: Supercarrier & > 1.48 Mt refused
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 23919; // Aeon Supercarrier
    });
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ShipNotAllowedThroughAnsiblex"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 28661;
    });
    if let Ok(Some((_, mut b))) = sov_a_sys.get_ball_snapshot(SHIP_1).await {
        b.mass = 1.50e9;
        let _ = sov_a_sys.spawn_ball(b).await;
    }
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ShipNotAllowedThroughAnsiblex"
    );
    if let Ok(Some((_, mut b))) = sov_a_sys.get_ball_snapshot(SHIP_1).await {
        b.mass = 10_000_000.0;
        let _ = sov_a_sys.spawn_ball(b).await;
    }

    let now = ctx.now_unix();
    let initial_fatigue_mins = 20.0;
    let initial_cooldown = now + 600;
    let initial_fatigue_until = now + 1200;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.jump_fatigue_minutes = initial_fatigue_mins;
            ch.jump_cooldown_until_unix = initial_cooldown;
            ch.jump_fatigue_until_unix = initial_fatigue_until;
        }
    });

    // Positive 1: Jump 1 in Delve sov null-sec (NOL-M9 -> O-IOAI, ~1.10 LY <= 5.0 LY, mass 10 Mt)
    let dist_1_ly = eve_server::services::space_destiny::space_nav_travel::distance_ly(&ctx, SOV_NULL_A, SOV_NULL_B).max(0.01);
    let ship1_mass = 10_000_000.0;
    let exp_cost_1 = ((ship1_mass * dist_1_ly * 0.000003 + 50.0).round() as i32).max(50);

    let jump_res = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_a_id)], &kw, None)
        .await;
    assert!(jump_res.is_ok(), "Ansiblex jump 1 succeeded: {jump_res:?}");
    assert_eq!(s1.solar_system_id, Some(SOV_NULL_B));

    let (cargo_unchanged, depleted_removed, ozone_left_1b) = ctx.db.with_snapshot(|snap| {
        (
            snap.items.get(&cargo_ozone_id).map(|i| i.quantity),
            !snap.items.contains_key(&fuel_ozone_1a_id),
            snap.items.get(&fuel_ozone_1b_id).map(|i| i.quantity).unwrap_or(0),
        )
    });
    assert_eq!(cargo_unchanged, Some(10_000), "Cargo Liquid Ozone must never be consumed");
    assert!(depleted_removed, "Depleted 30-unit StructureFuel stack must be removed");
    assert_eq!(50_030 - ozone_left_1b, exp_cost_1, "Exact StructureFuel ozone consumed for dist {dist_1_ly:.2} LY");

    // Verify 60s gate cloak applied in destination system
    let sov_b_sys = ctx.systems.get_or_spawn(SOV_NULL_B);
    let cs_arrived = sov_b_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
    assert!(cs_arrived.is_cloaked && cs_arrived.gate_cloak_remaining_ticks == 60, "60s gate cloak applied on Ansiblex arrival");

    // Positive 2: Jump 2 across a genuinely distinct LY pair in Delve sov null-sec (O-IOAI -> R5-MM8, ~3.85 LY <= 5.0 LY, mass 100 Mt)
    let dist_2_ly = eve_server::services::space_destiny::space_nav_travel::distance_ly(&ctx, SOV_NULL_B, SOV_NULL_C).max(0.01);
    assert!((dist_2_ly - dist_1_ly).abs() > 1.5, "Jump 2 must test a genuinely distinct LY distance ({dist_1_ly:.2} vs {dist_2_ly:.2})");
    let ozone_item_id_2 = 91_600_005i64;
    let ship2_mass = 100_000_000.0;
    let exp_cost_2 = ((ship2_mass * dist_2_ly * 0.000003 + 50.0).round() as i32).max(50);
    assert_ne!(exp_cost_1, exp_cost_2);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ozone_item_id_2,
            InventoryItemRecord {
                item_id: ozone_item_id_2,
                type_id: 16273,
                owner_id: 1_000_001,
                location_id: gate_b2_id,
                flag: InventoryFlag::StructureFuel,
                quantity: 50_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 645;
    });
    let _ = sov_b_sys
        .spawn_ball(eve_destiny::Ball {
            id: SHIP_1,
            type_id: 645,
            position: DVec3::new(2_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 150.0,
            inertia: 1.5,
            mass: ship2_mass,
            radius: 150.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);

    let jump_res_2 = ctx
        .dispatch_rpc(&mut s1, "structureJumpBridgeMgr", None, "CmdJumpThroughStructureStargate", &[PyRep::Integer(gate_b2_id)], &kw, None)
        .await;
    assert!(jump_res_2.is_ok(), "Ansiblex jump 2 succeeded: {jump_res_2:?}");
    assert_eq!(s1.solar_system_id, Some(SOV_NULL_C));
    let ozone_left_2 = ctx.db.with_snapshot(|snap| snap.items.get(&ozone_item_id_2).map(|it| it.quantity).unwrap_or(0));
    assert_eq!(50_000 - ozone_left_2, exp_cost_2, "Exact StructureFuel ozone consumed for dist {dist_2_ly:.2} LY");

    let (fatigue_2, cd_2, fu_2) = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&PILOT_1)
            .map(|c| (c.jump_fatigue_minutes, c.jump_cooldown_until_unix, c.jump_fatigue_until_unix))
            .unwrap_or((0.0, 0, 0))
    });
    assert_eq!((fatigue_2, cd_2, fu_2), (initial_fatigue_mins, initial_cooldown, initial_fatigue_until));

    // =========================================================================
    // 2. feat:navigation.bastion-siege-style-gate-blocks
    // =========================================================================
    let client_dialogs = ctx.sde.load_dialogs().expect("load V24.01 dialogs.static");
    assert_eq!(client_dialogs["CantDockWhileModuleActive"].dialog_id, 2094);
    assert_eq!(client_dialogs["CantJumpWhileModuleActive"].dialog_id, 2095);
    assert_eq!(client_dialogs["WarpDisrupted"].dialog_id, 1680);
    assert_eq!(client_dialogs["DeniedTethered"].dialog_id, 4899);

    let station_id = 60_000_361i64;
    let station_pos = DVec3::new(-107_072_593_920.0, -18_704_916_480.0, 436_343_562_240.0);
    let astrahus_id = 1_000_092_001i64;
    s1.solar_system_id = Some(JITA_SYSTEM);
    s1.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.solar_system_id = JITA_SYSTEM;
            ch.station_id = None;
            ch.jump_cooldown_until_unix = 0;
        }
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 28661;
        snap.items.get_mut(&SHIP_1).unwrap().location_id = JITA_SYSTEM as i64;
        snap.item_positions.insert(SHIP_1, (station_pos + DVec3::new(500.0, 0.0, 0.0)).to_array());
        snap.structures.insert(
            astrahus_id,
            make_structure(astrahus_id, 35832, JITA_SYSTEM, "Jita Tether Astrahus", (station_pos + DVec3::new(1000.0, 0.0, 0.0)).to_array()),
        );
    });
    let fleet_id = ctx
        .dispatch_rpc(&mut s1, "fleetObjectHandler", None, "CreateFleet", &[], &kw, None)
        .await
        .expect("CreateFleet")
        .as_i64()
        .expect("fleetID");
    s1.fleet_id = Some(fleet_id);
    ctx.dispatch_rpc(
        &mut s1,
        "fleetObjectHandler",
        None,
        "Invite",
        &[PyRep::Integer(PILOT_2), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)],
        &kw,
        None,
    )
    .await
    .expect("Invite PILOT_2");
    ctx.dispatch_rpc(&mut s2, "fleetObjectHandler", None, "AcceptInvite", &[PyRep::Integer(fleet_id)], &kw, None)
        .await
        .expect("AcceptInvite PILOT_2");
    s2.fleet_id = Some(fleet_id);
    ctx.invalidate_ship_fitting(SHIP_1);
    let _ = jita_sys
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: SHIP_1,
                type_id: 28661,
                position: station_pos + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 10_000_000.0,
                radius: 120.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(PILOT_1, SHIP_1, 28661),
        )
        .await;
    let _ = jita_sys
        .spawn_ball(eve_destiny::Ball {
            id: SHIP_2,
            type_id: 11999,
            position: station_pos + DVec3::new(300_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    // Activate Bastion Module I via live dogmaIM.Activate RPC
    let act_bastion = ctx
        .dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(bastion_mod_id)], &kw, None)
        .await;
    assert!(act_bastion.is_ok(), "Live dogmaIM.Activate on Bastion Module I: {act_bastion:?}");

    let bid = ctx.bind_object(&mut s1, "beyonce", JITA_SYSTEM as i64).expect("Bind beyonce");
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);

    // Negative 1: V24 beyonce.CmdDock, structureDocking.Dock & CmdTurboDock refused (`CantDockWhileModuleActive`, 2094)
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "beyonce", Some(bid), "CmdDock", &[PyRep::Integer(station_id), PyRep::Integer(SHIP_1)], &kw, None).await.unwrap_err().code,
        "CantDockWhileModuleActive"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "structureDocking", None, "Dock", &[PyRep::Integer(astrahus_id), PyRep::Integer(SHIP_1)], &kw, None).await.unwrap_err().code,
        "CantDockWhileModuleActive"
    );
    s1.role = eve_db::service_roles::ROLE_GM_DEFAULT;
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "beyonce", Some(bid), "CmdTurboDock", &[PyRep::Integer(station_id)], &kw, None).await.unwrap_err().code,
        "CantDockWhileModuleActive"
    );
    s1.role = 0;

    // Negative 2: Stargate jump refused (`CantJumpWhileModuleActive`, 2095)
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "beyonce", Some(bid), "CmdStargateJump", &[PyRep::Integer(50013876)], &kw, None).await.unwrap_err().code,
        "CantJumpWhileModuleActive"
    );

    // Negative 3: CmdWarpToStuff, WarpToMember & FleetWarp refused/skipped while Bastion active
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "beyonce", Some(bid), "CmdWarpToStuff", &[PyRep::String("station".into()), PyRep::Integer(station_id), PyRep::Real(0.0)], &kw, None).await.unwrap_err().code,
        "WarpDisrupted"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut s1, "fleetMgr", None, "WarpToMember", &[PyRep::Integer(PILOT_2), PyRep::Real(0.0)], &kw, None).await.unwrap_err().code,
        "WarpDisrupted"
    );
    let fw_res = ctx
        .dispatch_rpc(&mut s1, "fleetObjectHandler", None, "FleetWarp", &[PyRep::Real(station_pos.x + 1_000_000.0), PyRep::Real(station_pos.y), PyRep::Real(station_pos.z), PyRep::Real(0.0)], &kw, None)
        .await
        .expect("FleetWarp skips Bastion ship");
    let (_, b1_after_fw) = jita_sys.get_ball_snapshot(SHIP_1).await.unwrap().unwrap();
    let (_, b2_after_fw) = jita_sys.get_ball_snapshot(SHIP_2).await.unwrap().unwrap();
    assert!(!matches!(b1_after_fw.mode, eve_destiny::MovementMode::Warp { .. }), "Bastion ship must not enter warp on FleetWarp");
    assert!(matches!(b2_after_fw.mode, eve_destiny::MovementMode::Warp { .. }), "Non-Bastion fleetmate must enter warp on FleetWarp");
    let _ = fw_res;

    // Negative 4: Tethering refused via SDE attributes (`DeniedTethered`, 4899) even with no weapons timer
    ctx.advance_secs(61);
    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, PILOT_1, JITA_SYSTEM, SHIP_1).await;
    assert!(!jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_tethered, "Tether refused while Bastion active");
    assert_eq!(
        eve_server::services::space_navigation_advanced::check_immobilizing_module_active(&ctx, SHIP_1, "tether").unwrap_err().code,
        "DeniedTethered"
    );

    // Positive: Deactivate Bastion via dogmaIM.Deactivate -> tether, V24 CmdDock, and warp succeed!
    let deact_bastion = ctx
        .dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(bastion_mod_id)], &kw, None)
        .await;
    assert!(deact_bastion.is_ok(), "Bastion deactivation succeeds: {deact_bastion:?}");
    assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_tethered, "Tether automatically synced on Deactivate");

    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);
    let dock_ok = ctx
        .dispatch_rpc(&mut s1, "beyonce", Some(bid), "CmdDock", &[PyRep::Integer(station_id), PyRep::Integer(SHIP_1)], &kw, None)
        .await;
    assert!(dock_ok.is_ok(), "V24 CmdDock succeeds once Bastion deactivates: {dock_ok:?}");
    assert_eq!(s1.station_id, Some(station_id));

    s1.station_id = None;
    let bid = ctx.bind_object(&s1, "beyonce", JITA_SYSTEM as i64).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.station_id = None;
        }
    });
    let _ = jita_sys
        .spawn_ball(eve_destiny::Ball {
            id: SHIP_1,
            type_id: 28661,
            position: station_pos + DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    let warp_ok = ctx
        .dispatch_rpc(
            &mut s1,
            "beyonce",
            Some(bid),
            "CmdWarpToStuff",
            &[PyRep::String("stargate".into()), PyRep::Integer(50013876), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await;
    assert!(warp_ok.is_ok(), "Warp succeeds once Bastion deactivates: {warp_ok:?}");
}

}

#[allow(unused_imports)]
pub mod jump_cloak_targeting_delay_and_overrides {


use super::jump_cloak::setup_nav_pilot;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const JITA_SYSTEM: u32 = 30_000_142;
const PILOT_1: i64 = 93_200_001;
const PILOT_2: i64 = 93_200_002;
const SHIP_1: i64 = 83_200_001;
const SHIP_2: i64 = 83_200_002;

// parity: feat:navigation.cloak-targeting-delay
#[tokio::test]
async fn test_space_navigation_cloak_targeting_delay_and_overrides() {
    eve_testkit::attest(&["feat:navigation.cloak-targeting-delay"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let origin_pos = DVec3::new(15_000_000.0, 0.0, 0.0);
    let mut s1 = setup_nav_pilot(&ctx, PILOT_1, SHIP_1, 28661, JITA_SYSTEM, origin_pos).await; // Kronos
    let _s2 = setup_nav_pilot(
        &ctx,
        PILOT_2,
        SHIP_2,
        11999,
        JITA_SYSTEM,
        origin_pos + DVec3::new(10_000.0, 0.0, 0.0),
    )
    .await;

    let jita_sys = ctx.systems.get_or_spawn(JITA_SYSTEM);
    let kw = HashMap::new();

    // =========================================================================
    // 1. Kronos (28661) + Prototype Cloaking Device I (11370, 30s) + Cloaking II (-20% -> 24.0s)
    //    Verified via direct dogmaIM.Activate -> dogmaIM.Deactivate WITHOUT UpdateStateRequest,
    //    followed by ticking past 24s so AddTarget succeeds!
    // =========================================================================
    let cloak_item_id = 92_600_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT_1).or_default().insert(
            11579,
            TrainedSkillRecord {
                type_id: 11579,
                level: 2,
                skill_points: 8_000.0,
            },
        );
        snap.items.insert(
            cloak_item_id,
            InventoryItemRecord {
                item_id: cloak_item_id,
                type_id: 11370, // Prototype Cloaking Device I (SDE attr 560 = 30000ms)
                owner_id: PILOT_1,
                location_id: SHIP_1,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(SHIP_1);

    // Activate cloak directly via dogmaIM.Activate (no beyonce.UpdateStateRequest!)
    let act_res = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(cloak_item_id)],
            &kw,
            None,
        )
        .await;
    assert!(act_res.is_ok(), "Cloak activate: {act_res:?}");
    let cs_cloaked = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
    assert!(
        cs_cloaked.is_cloaked,
        "CombatState.is_cloaked must sync directly on dogmaIM.Activate"
    );
    assert!(
        (cs_cloaked.cloak_configured_targeting_delay_s - 24.0).abs() < 0.1,
        "Configured targeting delay must be 24.0s on Kronos with Cloaking II"
    );

    // AddTargetOBO must also fail while cloaked WITHOUT stripping undock/location-change invulnerability (`R-CLOAK-OBO-01`)
    {
        let mut cs_inv = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
        cs_inv.undock_invulnerable_remaining_ticks = 30;
        jita_sys.set_combat_state(SHIP_1, cs_inv).await.unwrap();
    }
    let obo_cloaked = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "AddTargetOBO",
            &[PyRep::Integer(SHIP_1), PyRep::Integer(SHIP_2)],
            &kw,
            None,
        )
        .await;
    assert_eq!(obo_cloaked.unwrap_err().code, "TargetingFailed");
    assert_eq!(
        jita_sys
            .get_combat_state(SHIP_1)
            .await
            .unwrap()
            .unwrap()
            .undock_invulnerable_remaining_ticks,
        30,
        "Rejected AddTargetOBO while cloaked must not clear undock_invulnerable_remaining_ticks"
    );
    assert!(jita_sys.get_target_locks(SHIP_1).await.unwrap().is_empty());

    // Deactivate cloak directly via dogmaIM.Deactivate
    let deact_res = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "Deactivate",
            &[PyRep::Integer(cloak_item_id)],
            &kw,
            None,
        )
        .await;
    assert!(deact_res.is_ok(), "Cloak deactivate: {deact_res:?}");
    let cs_decloaked = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
    assert!(!cs_decloaked.is_cloaked, "Ship is decloaked");
    assert!(
        (cs_decloaked.decloak_targeting_delay_remaining_s - 24.0).abs() < 0.1,
        "Decloak targeting delay must be 24.0s: got {:.2}s",
        cs_decloaked.decloak_targeting_delay_remaining_s
    );

    // Immediate re-cloak blocked by module reactivation delay (`P2-03`)
    let recloak_err = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(cloak_item_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(recloak_err.unwrap_err().code, "ModuleReactivationDelay");

    // Target lock blocked while sensor recalibration delay is active (`AddTarget` and `AddTargetOBO`)
    let lock_blocked = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(SHIP_2)],
            &kw,
            None,
        )
        .await;
    assert_eq!(lock_blocked.unwrap_err().code, "TargetingFailed");
    let obo_recal = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "AddTargetOBO",
            &[PyRep::Integer(SHIP_1), PyRep::Integer(SHIP_2)],
            &kw,
            None,
        )
        .await;
    assert_eq!(obo_recal.unwrap_err().code, "TargetingFailed");
    assert_eq!(
        jita_sys
            .get_combat_state(SHIP_1)
            .await
            .unwrap()
            .unwrap()
            .undock_invulnerable_remaining_ticks,
        30,
        "Rejected AddTarget/AddTargetOBO during sensor recalibration must not clear undock_invulnerable_remaining_ticks"
    );
    assert!(jita_sys.get_target_locks(SHIP_1).await.unwrap().is_empty());
    {
        let mut cs_inv = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
        cs_inv.undock_invulnerable_remaining_ticks = 0;
        jita_sys.set_combat_state(SHIP_1, cs_inv).await.unwrap();
    }

    // Tick 23s: 1.0s remaining -> AddTarget still blocked
    for _ in 0..23 {
        jita_sys.step_manual(1.0).await.unwrap();
    }
    assert!(
        jita_sys
            .get_combat_state(SHIP_1)
            .await
            .unwrap()
            .unwrap()
            .decloak_targeting_delay_remaining_s
            > 0.5
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(SHIP_2)],
            &kw,
            None
        )
        .await
        .unwrap_err()
        .code,
        "TargetingFailed"
    );

    // Tick 2 more seconds (25s total > 24s): recalibration timer expires -> AddTarget succeeds!
    jita_sys.step_manual(1.0).await.unwrap();
    jita_sys.step_manual(1.0).await.unwrap();
    assert_eq!(
        jita_sys
            .get_combat_state(SHIP_1)
            .await
            .unwrap()
            .unwrap()
            .decloak_targeting_delay_remaining_s,
        0.0
    );
    let lock_ok = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(SHIP_2)],
            &kw,
            None,
        )
        .await;
    assert!(
        lock_ok.is_ok(),
        "AddTarget must succeed once Kronos 24s decloak recalibration timer expires: {lock_ok:?}"
    );
    let _ = ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "RemoveTarget",
            &[PyRep::Integer(SHIP_2)],
            &kw,
            None,
        )
        .await;

    // =========================================================================
    // 2. Unfitting an active cloak (`invbroker.Add` to Cargo) and `TakeModuleOffline`
    //    must immediately clear `is_cloaked` (no permacloak) and start recalibration delay!
    // =========================================================================
    ctx.advance_secs(35);
    ctx.dispatch_rpc(
        &mut s1,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(cloak_item_id)],
        &kw,
        None,
    )
    .await
    .expect("Re-activate cloak after reactivation delay");
    assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_cloaked);

    // Unfit active cloak into ship Cargo via invbroker.Add while within 2,500m of a Mobile Depot
    let depot_id = 92_600_099i64;
    let (_, ship1_ball) = jita_sys.get_ball_snapshot(SHIP_1).await.unwrap().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            depot_id,
            InventoryItemRecord {
                item_id: depot_id,
                type_id: 33474, // Mobile Depot (group 1246)
                owner_id: PILOT_1,
                location_id: JITA_SYSTEM as i64,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let _ = jita_sys
        .spawn_ball(eve_destiny::Ball {
            id: depot_id,
            type_id: 33474,
            position: ship1_ball.position + DVec3::new(2_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 100_000.0,
            radius: 75.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let ship_inv_bid = ctx
        .bind_object(&mut s1, "invbroker", SHIP_1)
        .expect("Bind ship invbroker");
    let unfit_res = ctx
        .dispatch_rpc(
            &mut s1,
            "invbroker",
            Some(ship_inv_bid),
            "Add",
            &[
                PyRep::Integer(cloak_item_id),
                PyRep::Integer(SHIP_1),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await;
    assert!(unfit_res.is_ok(), "Unfit active cloak to cargo: {unfit_res:?}");
    let cs_after_unfit = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
    assert!(
        !cs_after_unfit.is_cloaked,
        "Unfitting an active cloak must immediately clear is_cloaked"
    );
    assert!(
        (cs_after_unfit.decloak_targeting_delay_remaining_s - 24.0).abs() < 0.1,
        "Unfitting an active cloak must start the 24.0s recalibration delay"
    );
    for _ in 0..25 {
        jita_sys.step_manual(1.0).await.unwrap();
    }

    // Also verify `invbroker.MultiAdd` unfitting an active cloak in space at 2,500m from Mobile Depot (`R-CLOAK-UNFIT-01`)
    ctx.dispatch_rpc(
        &mut s1,
        "invbroker",
        Some(ship_inv_bid),
        "Add",
        &[
            PyRep::Integer(cloak_item_id),
            PyRep::Integer(SHIP_1),
            PyRep::Integer(InventoryFlag::HiSlot1 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Re-fit cloak into HiSlot1 near Mobile Depot");
    ctx.advance_secs(35);
    ctx.dispatch_rpc(
        &mut s1,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(cloak_item_id)],
        &kw,
        None,
    )
    .await
    .expect("Activate re-fitted cloak");
    assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_cloaked);
    let multi_unfit = ctx
        .dispatch_rpc(
            &mut s1,
            "invbroker",
            Some(ship_inv_bid),
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(cloak_item_id)]),
                PyRep::Integer(SHIP_1),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await;
    assert!(multi_unfit.is_ok(), "MultiAdd unfit active cloak to cargo: {multi_unfit:?}");
    let cs_after_multi = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
    assert!(
        !cs_after_multi.is_cloaked,
        "MultiAdd unfitting an active cloak must immediately clear is_cloaked"
    );
    assert!(
        (cs_after_multi.decloak_targeting_delay_remaining_s - 24.0).abs() < 0.1,
        "MultiAdd unfitting an active cloak must start the 24.0s recalibration delay, got {:.2}s",
        cs_after_multi.decloak_targeting_delay_remaining_s
    );
    let _ = jita_sys.remove_ball(depot_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&depot_id);
    });
    for _ in 0..25 {
        jita_sys.step_manual(1.0).await.unwrap();
    }

    // =========================================================================
    // 3. Stealth Bomber (Nemesis 11377 -> 0s) & Covert Ops Frigate (Anathema 11188 -> 0s)
    // =========================================================================
    let bomber_cloak_id = 92_600_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&cloak_item_id);
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 11377; // Nemesis (group 834, 1035 = 0.0)
        snap.items.insert(
            bomber_cloak_id,
            InventoryItemRecord {
                item_id: bomber_cloak_id,
                type_id: 11370,
                owner_id: PILOT_1,
                location_id: SHIP_1,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(SHIP_1);
    let _ = jita_sys
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: SHIP_1,
                type_id: 11377,
                position: origin_pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_200_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(PILOT_1, SHIP_1, 11377),
        )
        .await;

    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(bomber_cloak_id)], &kw, None)
        .await
        .expect("Bomber cloak activate");
    assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_cloaked);
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(bomber_cloak_id)], &kw, None)
        .await
        .expect("Bomber cloak deactivate");
    assert_eq!(
        jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().decloak_targeting_delay_remaining_s,
        0.0
    );
    assert!(ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTarget", &[PyRep::Integer(SHIP_2)], &kw, None).await.is_ok());
    let _ = ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(SHIP_2)], &kw, None).await;

    // Covert Ops Frigate (Anathema 11188 + Covert Ops Cloaking Device II 11578 -> 0s)
    let covert_cloak_id = 92_600_004i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&bomber_cloak_id);
        snap.skills.entry(PILOT_1).or_default().insert(
            11579,
            TrainedSkillRecord {
                type_id: 11579,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.items.get_mut(&SHIP_1).unwrap().type_id = 11188; // Anathema
        snap.items.insert(
            covert_cloak_id,
            InventoryItemRecord {
                item_id: covert_cloak_id,
                type_id: 11578,
                owner_id: PILOT_1,
                location_id: SHIP_1,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(SHIP_1);
    let _ = jita_sys
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: SHIP_1,
                type_id: 11188,
                position: origin_pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_100_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(PILOT_1, SHIP_1, 11188),
        )
        .await;

    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(covert_cloak_id)], &kw, None)
        .await
        .expect("Covert Ops cloak activate");
    assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_cloaked);
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(covert_cloak_id)], &kw, None)
        .await
        .expect("Covert Ops cloak deactivate");
    assert_eq!(
        jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().decloak_targeting_delay_remaining_s,
        0.0
    );

    // =========================================================================
    // 4. Non-zero `covertOpsStealthBomberTargettingDelay` (`1035 = 5000.0` ms -> 5.0s)
    //    on Black Ops (Redeemer 22428 override) / Endurance (37135) AND T3 Strategic Cruiser
    //    (Legion 29986 + Covert Reconfiguration 45586 + Covert Ops Cloak II 11578 -> 5.0s)!
    // =========================================================================
    let blops_cloak_id = 92_600_020i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&covert_cloak_id);
        let ship_it = snap.items.get_mut(&SHIP_1).unwrap();
        ship_it.type_id = 22428; // Redeemer Black Ops
        ship_it.dynamic_attributes.insert(1035, 5000.0); // Non-zero 5000ms override
        snap.items.insert(
            blops_cloak_id,
            InventoryItemRecord {
                item_id: blops_cloak_id,
                type_id: 11370, // 30s base Prototype Cloak overridden to 5.0s by attr 1035!
                owner_id: PILOT_1,
                location_id: SHIP_1,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(SHIP_1);
    let _ = jita_sys
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: SHIP_1,
                type_id: 22428,
                position: origin_pos,
                velocity: DVec3::ZERO,
                max_velocity: 150.0,
                inertia: 2.0,
                mass: 100_000_000.0,
                radius: 300.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(PILOT_1, SHIP_1, 22428),
        )
        .await;

    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(blops_cloak_id)], &kw, None)
        .await
        .expect("Black Ops cloak activate");
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(blops_cloak_id)], &kw, None)
        .await
        .expect("Black Ops cloak deactivate");
    let cs_blops = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
    assert!(
        (cs_blops.decloak_targeting_delay_remaining_s - 5.0).abs() < 0.1,
        "Non-zero Black Ops 1035 override must yield 5.0s delay, got {:.2}s",
        cs_blops.decloak_targeting_delay_remaining_s
    );
    for _ in 0..6 {
        jita_sys.step_manual(1.0).await.unwrap();
    }

    // T3 Strategic Cruiser: Legion (29986) + Legion Defensive - Covert Reconfiguration (45586) + Covert Cloak II (11578)
    let t3c_sub_id = 92_600_030i64;
    let t3c_cloak_id = 92_600_031i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&blops_cloak_id);
        let ship_it = snap.items.get_mut(&SHIP_1).unwrap();
        ship_it.type_id = 29986; // Legion
        ship_it.dynamic_attributes.clear();
        snap.items.insert(
            t3c_sub_id,
            InventoryItemRecord {
                item_id: t3c_sub_id,
                type_id: 45586, // Legion Defensive - Covert Reconfiguration
                owner_id: PILOT_1,
                location_id: SHIP_1,
                flag: InventoryFlag::SubSystemSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            t3c_cloak_id,
            InventoryItemRecord {
                item_id: t3c_cloak_id,
                type_id: 11578, // Covert Ops Cloaking Device II (10s base * 0.5 Cloaking V = 5.0s; 0 CPU via 45586)
                owner_id: PILOT_1,
                location_id: SHIP_1,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(SHIP_1);
    let _ = jita_sys
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: SHIP_1,
                type_id: 29986,
                position: origin_pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 2.0,
                mass: 15_000_000.0,
                radius: 170.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(PILOT_1, SHIP_1, 29986),
        )
        .await;

    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(t3c_cloak_id)], &kw, None)
        .await
        .expect("T3C Covert Reconfiguration cloak activate");
    assert!(jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap().is_cloaked);
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(t3c_cloak_id)], &kw, None)
        .await
        .expect("T3C Covert Reconfiguration cloak deactivate");
    let cs_t3c = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
    assert!(
        (cs_t3c.decloak_targeting_delay_remaining_s - 5.0).abs() < 0.1,
        "T3C Covert Reconfiguration + Covert Cloak II + Cloaking V must evaluate to 5.0s via ShipFittingEngine, got {:.2}s",
        cs_t3c.decloak_targeting_delay_remaining_s
    );

    // Gate-cloak break via AddTarget & AddTargetOBO: breaks 60s jump cloak with 0s delay & starts lock
    for _ in 0..6 {
        jita_sys.step_manual(1.0).await.unwrap();
    }
    for use_obo in [false, true] {
        let mut gc = ctx.build_ship_combat_state(PILOT_1, SHIP_1, 29986);
        gc.is_cloaked = true;
        gc.gate_cloak_remaining_ticks = 60;
        let (_, b1) = jita_sys.get_ball_snapshot(SHIP_1).await.unwrap().unwrap();
        let _ = jita_sys.spawn_ball_with_combat(b1, gc).await;
        let _ = jita_sys.clear_target_locks(SHIP_1).await;
        let res = if use_obo {
            ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTargetOBO", &[PyRep::Integer(SHIP_1), PyRep::Integer(SHIP_2)], &kw, None).await
        } else {
            ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTarget", &[PyRep::Integer(SHIP_2)], &kw, None).await
        };
        assert!(res.is_ok(), "Gate-cloak target lock (obo={use_obo}) must succeed: {res:?}");
        let cs_after = jita_sys.get_combat_state(SHIP_1).await.unwrap().unwrap();
        assert!(!cs_after.is_cloaked && cs_after.gate_cloak_remaining_ticks == 0 && cs_after.decloak_targeting_delay_remaining_s == 0.0);
        assert_eq!(jita_sys.get_target_locks(SHIP_1).await.unwrap().len(), 1);
    }
}

}

#[tokio::test]
async fn it_dock_enforces_2500m_perimeter_even_with_nonempty_session_address() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(9876);
    sess.address = "tcp:127.0.0.1:41234".to_string();
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let _ = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("select char");
    let char_id = sess.character_id.unwrap();
    let ship_id = sess.ship_id.unwrap();
    let sys_id = sess.solar_system_id.unwrap_or(30_000_142);

    // Undock first into space
    let _ = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("undock");
    ctx.advance_secs(15);
    let st_id = 60_003_760_i64;
    let upwell_id = 1_099_774_001_i64;
    let upwell_pos = DVec3::new(10_000_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            upwell_id,
            UpwellStructureRecord {
                structure_id: upwell_id,
                type_id: 35832,
                owner_corp_id: sess.corporation_id.unwrap_or(1_000_009),
                solar_system_id: sys_id,
                name: "Perimeter Test Citadel".into(),
                position: upwell_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                ..Default::default()
            },
        );
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let (_, station_ball) = sys_h.get_ball_snapshot(st_id).await.unwrap().expect("station ball");

    // Place ship 5,000m beyond station radius (> 2,500m docking perimeter)
    let _ = sys_h
        .set_ball_position(
            ship_id,
            station_ball.position + DVec3::new(station_ball.radius + 5_000.0, 0.0, 0.0),
        )
        .await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Dock", &[PyRep::Integer(st_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "OutsideDockingPerimeter"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "beyonce", None, "CmdDock", &[PyRep::Integer(st_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "OutsideDockingPerimeter"
    );

    // Spawn structure ball on grid and move ship 10,000m beyond structure radius (> 2,500m)
    let st_rad = 15_000.0;
    let _ = sys_h
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: upwell_id,
                type_id: 35832,
                position: upwell_pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: st_rad,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::new(),
        )
        .await;
    let _ = sys_h
        .set_ball_position(
            ship_id,
            upwell_pos + DVec3::new(st_rad + 10_000.0, 0.0, 0.0),
        )
        .await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "structureDocking", None, "Dock", &[PyRep::Integer(upwell_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "OutsideDockingPerimeter"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "beyonce", None, "CmdDock", &[PyRep::Integer(upwell_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "OutsideDockingPerimeter"
    );

    // Within 2,000m of station radius -> stationSvc.Dock succeeds even with non-empty session.address
    let _ = sys_h
        .set_ball_position(
            ship_id,
            station_ball.position + DVec3::new(station_ball.radius + 2_000.0, 0.0, 0.0),
        )
        .await;
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Dock", &[PyRep::Integer(st_id)], &kw, None)
        .await
        .expect("Dock within 2,500m perimeter succeeds");
    assert_eq!(sess.station_id, Some(st_id));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].station_id), Some(st_id));
}

fn parse_destiny_action_names(updates: &PyRep) -> Vec<String> {
    let bytes = eve_proto::marshal::encode(updates);
    let decoded = eve_proto::marshal::decode(&bytes).expect("DoDestinyUpdate marshal round-trip");
    let PyRep::List(entries) = decoded else {
        panic!("DoDestinyUpdate payload must be a list, got {decoded:?}");
    };
    entries
        .into_iter()
        .map(|entry| {
            let PyRep::Tuple(outer) = entry else {
                panic!("destiny entry must be (stamp, action)");
            };
            let PyRep::Tuple(inner) = &outer[1] else {
                panic!("destiny action must be (name, args)");
            };
            inner[0]
                .as_str()
                .expect("destiny action name")
                .to_string()
        })
        .collect()
}

fn collect_tcp_destiny_action_names(rep: &PyRep, out: &mut Vec<String>) {
    match rep {
        PyRep::SubStream(inner) => collect_tcp_destiny_action_names(inner, out),
        PyRep::Tuple(items) => {
            if items.len() == 2 && items[0].as_i64().is_some() {
                if let PyRep::Tuple(act) = &items[1] {
                    if let Some(name) = act.first().and_then(|v| match v {
                        PyRep::String(s) => Some(s.as_str()),
                        PyRep::Bytes(b) => std::str::from_utf8(b).ok(),
                        _ => None,
                    }) {
                        out.push(name.to_string());
                        return;
                    }
                }
            }
            for item in items {
                collect_tcp_destiny_action_names(item, out);
            }
        }
        PyRep::List(items) => {
            for item in items {
                collect_tcp_destiny_action_names(item, out);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn test_1051_ratcheted_arriving_warp_dock_structure_and_station() {
    use eve_destiny::WarpPhase;
    use eve_testkit::client_harness::{
        extract_bound_id_from_wire_rep, ClientCallOutput, ClientWireFrameKind,
        ContractValidationMode,
    };

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let struct_id = 1_040_105_102_i64;
    let kw = HashMap::new();

    let (ship_id, corp_id) = ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        (ch.active_ship_id, ch.corporation_id)
    });
    let st_pos = ctx.sde.stations[&st_id].position;
    let struct_pos = st_pos + DVec3::new(0.0, 500_000.0, 0.0);
    let struct_radius = ctx
        .sde
        .types
        .get(&35832)
        .map(|t| t.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or(15_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                solar_system_id: sys_id,
                owner_corp_id: corp_id,
                name: "Ratchet Astrahus".into(),
                position: struct_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, st_ball) = sys_handle.get_ball_snapshot(st_id).await.unwrap().unwrap();
    let st_radius = st_ball.radius;

    let mut harness =
        common::new_wire_harness(&ctx, 105_102).with_mode(ContractValidationMode::Ratcheted);
    harness
        .call_ok(
            "authentication",
            None,
            "Login",
            &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
            &kw,
        )
        .await;
    harness
        .call_ok(
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &kw,
        )
        .await;

    let assert_warp_stop_before_release = |label: &str, out: &ClientCallOutput| {
        assert!(out.result.is_ok(), "{label} must succeed: {:?}", out.result);
        let destiny_idx = out
            .frames
            .iter()
            .position(|f| matches!(f.kind, ClientWireFrameKind::DoDestinyUpdate { .. }))
            .unwrap_or_else(|| panic!("{label} must emit DoDestinyUpdate"));
        let sess_idx = out
            .frames
            .iter()
            .position(|f| matches!(f.kind, ClientWireFrameKind::SessionChange { .. }))
            .unwrap_or_else(|| panic!("{label} must emit SessionChange"));
        let rsp_idx = out
            .frames
            .iter()
            .position(|f| matches!(f.kind, ClientWireFrameKind::CallRsp { .. }))
            .unwrap_or_else(|| panic!("{label} must emit CallRsp"));
        assert!(
            destiny_idx < sess_idx && sess_idx < rsp_idx,
            "{label}: DoDestinyUpdate ({destiny_idx}) must precede SessionChange ({sess_idx}) and CallRsp ({rsp_idx})"
        );
        let ClientWireFrameKind::DoDestinyUpdate { updates, .. } = &out.frames[destiny_idx].kind else {
            unreachable!();
        };
        let actions = parse_destiny_action_names(updates);
        let massive_idx = actions
            .iter()
            .position(|n| n == "SetBallMassive")
            .unwrap_or_else(|| panic!("{label} missing SetBallMassive in {actions:?}"));
        let fx_idx = actions
            .iter()
            .position(|n| n == "OnSpecialFX")
            .unwrap_or_else(|| panic!("{label} missing OnSpecialFX in {actions:?}"));
        let stop_idx = actions
            .iter()
            .position(|n| n == "Stop")
            .unwrap_or_else(|| panic!("{label} missing Stop in {actions:?}"));
        let vel_idx = actions
            .iter()
            .position(|n| n == "SetBallVelocity")
            .unwrap_or_else(|| panic!("{label} missing SetBallVelocity in {actions:?}"));
        let pos_idx = actions
            .iter()
            .position(|n| n == "SetBallPosition")
            .unwrap_or_else(|| panic!("{label} missing SetBallPosition in {actions:?}"));
        assert!(
            massive_idx < fx_idx && fx_idx < stop_idx && stop_idx < vel_idx && vel_idx < pos_idx,
            "{label}: expected SetBallMassive < OnSpecialFX < Stop < SetBallVelocity < SetBallPosition in {actions:?}"
        );
        assert!(
            !actions.iter().any(|n| n == "RemoveBall" || n == "RemoveBalls"),
            "{label}: self observer must not receive RemoveBall(s) before OnReleaseBallpark: {actions:?}"
        );
    };

    // 1. structureDocking.Dock while arriving from warp (300m short of landing, outside static 2,500m shell)
    ctx.advance_secs(12);
    harness.call_ok("stationSvc", None, "Undock", &[], &kw).await;
    let bind1 = harness
        .call_exchange(
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(sys_id as i64), PyRep::None],
            &kw,
        )
        .await;
    assert!(bind1.result.is_ok());

    let struct_origin = struct_pos + DVec3::new(1_000_000_000.0, 0.0, 0.0);
    let struct_landing = struct_pos + DVec3::new(struct_radius + 2_300.0, 0.0, 0.0);
    let mut ship_ball = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    ship_ball.position = struct_landing + DVec3::new(300.0, 0.0, 0.0);
    ship_ball.mode = MovementMode::Warp {
        origin: struct_origin,
        destination: struct_landing,
        warp_speed_au_s: 15.0,
        total_distance: struct_origin.distance(struct_landing),
        phase: WarpPhase::Decelerating,
        elapsed_warp_time: 5.0,
    };
    sys_handle.spawn_ball(ship_ball).await.unwrap();
    harness.drain_frames();

    ctx.advance_secs(12);
    let struct_dock_out = harness
        .call_exchange(
            "structureDocking",
            None,
            "Dock",
            &[PyRep::Integer(struct_id)],
            &kw,
        )
        .await;
    assert_warp_stop_before_release("structureDocking.Dock", &struct_dock_out);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].structure_id),
        Some(struct_id)
    );

    // 2. beyonce.CmdDock at NPC station while arriving from warp (300m short of landing)
    ctx.advance_secs(12);
    harness
        .call_ok("structureDocking", None, "Undock", &[], &kw)
        .await;
    let bind2 = harness
        .call_exchange(
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(sys_id as i64), PyRep::None],
            &kw,
        )
        .await;
    let beyonce_bid2 = extract_bound_id_from_wire_rep(&bind2.result.expect("MachoBindObject 2"))
        .expect("beyonce bound id 2");

    let st_origin = st_pos + DVec3::new(1_000_000_000.0, 0.0, 0.0);
    let st_landing = st_pos + DVec3::new(st_radius + 2_300.0, 0.0, 0.0);
    let mut ship_ball2 = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    ship_ball2.position = st_landing + DVec3::new(300.0, 0.0, 0.0);
    ship_ball2.mode = MovementMode::Warp {
        origin: st_origin,
        destination: st_landing,
        warp_speed_au_s: 15.0,
        total_distance: st_origin.distance(st_landing),
        phase: WarpPhase::Decelerating,
        elapsed_warp_time: 5.0,
    };
    sys_handle.spawn_ball(ship_ball2).await.unwrap();
    harness.drain_frames();

    ctx.advance_secs(12);
    let st_cmd_dock_out = harness
        .call_exchange(
            "beyonce",
            Some(beyonce_bid2),
            "CmdDock",
            &[PyRep::Integer(st_id)],
            &kw,
        )
        .await;
    assert_warp_stop_before_release("beyonce.CmdDock", &st_cmd_dock_out);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].station_id),
        Some(st_id)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1051_live_tcp_macho_dock_splits_warp_stop_and_session_change_settle_gap() {
    use eve_destiny::WarpPhase;
    use eve_machonet::{
        placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeResult, CryptoRequestPacket,
        CryptoServerHandshake, MachoAddress, MachoPacket, MachoPacketType, MACHO_PROXY_NODE_ID,
    };
    use eve_server::macho_listener::{handle_macho_client, read_framed_pyrep, write_framed_pyrep};
    use eve_server::services::space_warp_jump::WARP_DOCK_SETTLE_DURATION;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::{TcpListener, TcpStream};

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let st_pos = ctx.sde.stations[&st_id].position;
    let (_, st_ball) = sys_handle.get_ball_snapshot(st_id).await.unwrap().unwrap();
    let st_radius = st_ball.radius;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind macho");
    let addr = listener.local_addr().expect("local_addr");
    let ctx_server = Arc::clone(&ctx);
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = handle_macho_client(stream, ctx_server).await;
        }
    });

    let mut s = TcpStream::connect(addr).await.expect("connect macho");
    let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    let client_ver = PyRep::Tuple(vec![
        PyRep::Integer(170_472),
        PyRep::Integer(i64::from(eve_machonet::V24_MACHO_VERSION)),
        PyRep::Integer(0),
        PyRep::Real(eve_machonet::V24_VERSION_NUMBER),
        PyRep::Integer(i64::from(eve_machonet::V24_BOOT_BUILD)),
        PyRep::ascii_bytes(eve_machonet::V24_CODENAME_AT_REGION),
    ]);
    write_framed_pyrep(&mut s, &client_ver).await.unwrap();
    write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
        .await
        .unwrap();
    assert_eq!(read_framed_pyrep(&mut s).await.unwrap().unwrap().as_str(), Some("OK CC"));

    let challenge = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    write_framed_pyrep(&mut s, &challenge.to_pyrep()).await.unwrap();
    assert_eq!(read_framed_pyrep(&mut s).await.unwrap().unwrap(), PyRep::Integer(2));
    let srv_hs = CryptoServerHandshake::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap())
        .expect("CryptoServerHandshake");
    let hs_res = CryptoHandshakeResult {
        challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
        func_output: Vec::new(),
        func_result: PyRep::Integer(0),
    };
    write_framed_pyrep(&mut s, &hs_res.to_pyrep()).await.unwrap();
    let _ack = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    let init_pkt =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    let MachoAddress::Client { client_id, .. } = init_pkt.destination else {
        panic!("expected Client destination on SessionInitialStateNotification");
    };
    let _sess0 = read_framed_pyrep(&mut s).await.unwrap().unwrap();

    let send_call = |call_id: u64, service: &str, method: &str, args: Vec<PyRep>| {
        let inner_tuple = PyRep::Tuple(vec![
            PyRep::ascii_bytes(method),
            PyRep::Tuple(args),
            PyRep::Dict(Vec::new()),
        ]);
        MachoPacket {
            packet_type: MachoPacketType::CallReq,
            source: MachoAddress::Client {
                client_id,
                call_id,
                service: None,
            },
            destination: MachoAddress::Node {
                node_id: MACHO_PROXY_NODE_ID,
                service: Some(service.into()),
                call_id: 0,
            },
            user_id: Some(1001),
            payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Integer(0),
                PyRep::SubStream(Box::new(inner_tuple)),
            ])]),
            named_payload: HashMap::new(),
        }
    };

    async fn drain_until_call_rsp(
        stream: &mut TcpStream,
    ) -> Vec<(tokio::time::Instant, MachoPacket)> {
        let mut pkts = Vec::new();
        loop {
            let rep = tokio::time::timeout(
                Duration::from_secs(10),
                read_framed_pyrep(stream),
            )
            .await
            .expect("TCP read timeout waiting for CallRsp")
            .expect("frame read ok")
            .expect("non-EOF frame");
            let pkt = MachoPacket::from_pyrep(&rep).expect("MachoPacket");
            assert_ne!(
                pkt.packet_type,
                MachoPacketType::ErrorResponse,
                "unexpected ErrorResponse: {rep:?}"
            );
            let done = pkt.packet_type == MachoPacketType::CallRsp;
            pkts.push((tokio::time::Instant::now(), pkt));
            if done {
                return pkts;
            }
        }
    }

    // SelectCharacterID(90_000_001)
    write_framed_pyrep(
        &mut s,
        &send_call(1, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(char_id)])
            .to_pyrep(),
    )
    .await
    .unwrap();
    let _ = drain_until_call_rsp(&mut s).await;

    // Phase A: Undock, transition destiny stream to Live, enter arriving warp (300m short of landing),
    // and call stationSvc.Dock over live TCP.
    ctx.advance_secs(12);
    write_framed_pyrep(
        &mut s,
        &send_call(2, "stationSvc", "Undock", Vec::new()).to_pyrep(),
    )
    .await
    .unwrap();
    let _ = drain_until_call_rsp(&mut s).await;

    write_framed_pyrep(
        &mut s,
        &send_call(
            3,
            "beyonce",
            "MachoBindObject",
            vec![
                PyRep::Integer(sys_id as i64),
                PyRep::Tuple(vec![
                    PyRep::ascii_bytes("GetFormations"),
                    PyRep::Tuple(Vec::new()),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let _ = drain_until_call_rsp(&mut s).await;

    let origin = st_pos + DVec3::new(1_000_000_000.0, 0.0, 0.0);
    let landing = st_pos + DVec3::new(st_radius + 2_300.0, 0.0, 0.0);
    let mut ball = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
    ball.position = landing + DVec3::new(300.0, 0.0, 0.0);
    ball.mode = MovementMode::Warp {
        origin,
        destination: landing,
        warp_speed_au_s: 15.0,
        total_distance: origin.distance(landing),
        phase: WarpPhase::Decelerating,
        elapsed_warp_time: 5.0,
    };
    sys_handle.spawn_ball(ball).await.unwrap();

    ctx.advance_secs(12);
    let dock_sent_a = tokio::time::Instant::now();
    write_framed_pyrep(
        &mut s,
        &send_call(4, "stationSvc", "Dock", vec![PyRep::Integer(st_id)]).to_pyrep(),
    )
    .await
    .unwrap();
    let pkts_a = drain_until_call_rsp(&mut s).await;

    let mut stop_at_a = None;
    let mut sess_at_a = None;
    let mut rsp_at_a = None;
    for (ts, pkt) in &pkts_a {
        match pkt.packet_type {
            MachoPacketType::Notification => {
                if let MachoAddress::Broadcast { broadcast_id, .. } = &pkt.destination {
                    if broadcast_id == "DoDestinyUpdate" {
                        let mut actions = Vec::new();
                        collect_tcp_destiny_action_names(&pkt.payload, &mut actions);
                        if actions.iter().any(|n| n == "Stop")
                            && actions.iter().any(|n| n == "SetBallMassive")
                            && !actions.iter().any(|n| n == "RemoveBall" || n == "RemoveBalls")
                            && stop_at_a.is_none()
                        {
                            stop_at_a = Some(*ts);
                        }
                    }
                }
            }
            MachoPacketType::SessionChangeNotification if sess_at_a.is_none() => {
                sess_at_a = Some(*ts);
            }
            MachoPacketType::CallRsp if rsp_at_a.is_none() => {
                rsp_at_a = Some(*ts);
            }
            _ => {}
        }
    }
    let stop_at_a = stop_at_a.expect("immediate DoDestinyUpdate(Stop, SetBallMassive) before settle");
    let sess_at_a = sess_at_a.expect("SessionChangeNotification after settle");
    let rsp_at_a = rsp_at_a.expect("CallRsp after settle");
    assert!(
        stop_at_a.duration_since(dock_sent_a) < Duration::from_millis(500),
        "DoDestinyUpdate(Stop) must flush immediately instead of waiting for settle ({:?})",
        stop_at_a.duration_since(dock_sent_a)
    );
    assert!(
        sess_at_a.duration_since(stop_at_a)
            >= WARP_DOCK_SETTLE_DURATION.saturating_sub(Duration::from_millis(50)),
        "SessionChangeNotification must wait out WARP_DOCK_SETTLE_DURATION after Stop ({:?})",
        sess_at_a.duration_since(stop_at_a)
    );
    assert!(rsp_at_a >= sess_at_a);
    assert_eq!(
        ctx.db.with_snapshot(|db| db.characters[&char_id].station_id),
        Some(st_id)
    );

    // Phase B: Post-OnWarpArrived / natural Ballpark::step warp-exit race:
    // Undock, warp 300km to station via CmdWarpToStuff, advance physics until MovementMode::Stop,
    // then immediately call stationSvc.Dock while server has already left MovementMode::Warp.
    ctx.advance_secs(12);
    write_framed_pyrep(
        &mut s,
        &send_call(5, "stationSvc", "Undock", Vec::new()).to_pyrep(),
    )
    .await
    .unwrap();
    let _ = drain_until_call_rsp(&mut s).await;

    write_framed_pyrep(
        &mut s,
        &send_call(
            6,
            "beyonce",
            "MachoBindObject",
            vec![
                PyRep::Integer(sys_id as i64),
                PyRep::Tuple(vec![
                    PyRep::ascii_bytes("GetFormations"),
                    PyRep::Tuple(Vec::new()),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let _ = drain_until_call_rsp(&mut s).await;

    let _ = sys_handle
        .set_ball_position(ship_id, st_pos + DVec3::new(st_radius + 300_000.0, 0.0, 0.0))
        .await;
    write_framed_pyrep(
        &mut s,
        &send_call(
            7,
            "beyonce",
            "CmdWarpToStuff",
            vec![PyRep::String("item".into()), PyRep::Integer(st_id)],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let _ = drain_until_call_rsp(&mut s).await;

    let mut exited = false;
    for _ in 0..45 {
        sys_handle.step_manual(1.0).await.unwrap();
        let b = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;
        if matches!(b.mode, MovementMode::Stop) {
            exited = true;
            break;
        }
    }
    assert!(exited, "CmdWarpToStuff 300km warp must naturally exit to MovementMode::Stop");

    ctx.advance_secs(12);
    let dock_sent_b = tokio::time::Instant::now();
    write_framed_pyrep(
        &mut s,
        &send_call(8, "stationSvc", "Dock", vec![PyRep::Integer(st_id)]).to_pyrep(),
    )
    .await
    .unwrap();
    let pkts_b = drain_until_call_rsp(&mut s).await;

    let sess_at_b = pkts_b
        .iter()
        .find_map(|(ts, pkt)| {
            (pkt.packet_type == MachoPacketType::SessionChangeNotification).then_some(*ts)
        })
        .expect("Phase B SessionChangeNotification after natural warp exit");
    assert!(
        sess_at_b.duration_since(dock_sent_b)
            >= WARP_DOCK_SETTLE_DURATION.saturating_sub(Duration::from_millis(150)),
        "Dock right after natural server warp exit must still defer SessionChangeNotification ({:?})",
        sess_at_b.duration_since(dock_sent_b)
    );
    assert_eq!(
        ctx.db.with_snapshot(|db| db.characters[&char_id].station_id),
        Some(st_id)
    );
}

#[tokio::test]
async fn it_titan_keepstar_docking_and_fax_stargate_jump_use_ship_ball_radius() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(9877);
    sess.address = "tcp:127.0.0.1:41235".to_string();
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .expect("select char");
    let char_id = sess.character_id.unwrap();
    let ship_id = sess.ship_id.unwrap();
    let sys_id = sess.solar_system_id.unwrap_or(30_000_142);

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("undock");
    ctx.advance_secs(15);

    let titan_type_id = 11_567_u32;
    let keepstar_type_id = 35_834_u32;
    let titan_radius = ctx
        .sde
        .types
        .get(&titan_type_id)
        .map(|t| t.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or(6_800.0);
    let keepstar_radius = ctx
        .sde
        .types
        .get(&keepstar_type_id)
        .map(|t| t.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or(150_000.0);
    assert!(titan_radius > 2_500.0);

    let keepstar_id = 1_099_774_101_i64;
    let keepstar_pos = DVec3::new(20_000_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_it) = snap.items.get_mut(&ship_id) {
            ship_it.type_id = titan_type_id;
        }
        snap.structures.insert(
            keepstar_id,
            UpwellStructureRecord {
                structure_id: keepstar_id,
                type_id: keepstar_type_id,
                owner_corp_id: sess.corporation_id.unwrap_or(1_000_009),
                solar_system_id: sys_id,
                name: "1DQ1-A - Citadel".into(),
                position: keepstar_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                ..Default::default()
            },
        );
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: keepstar_id,
                type_id: keepstar_type_id,
                position: keepstar_pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000_000_000.0,
                radius: keepstar_radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::new(),
        )
        .await;
    let _ = sys_h
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: titan_type_id,
                position: keepstar_pos
                    + DVec3::new(keepstar_radius + titan_radius + 2_501.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 60.0,
                inertia: 0.5,
                mass: 2_400_000_000.0,
                radius: titan_radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::new(),
        )
        .await;

    // 2,501 m surface distance -> OutsideDockingPerimeter + DockingApproach
    let far_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDocking",
            None,
            "Dock",
            &[PyRep::Integer(keepstar_id), PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(far_err.code, "OutsideDockingPerimeter");
    assert_eq!(
        far_err.client_error.as_deref(),
        Some(&eve_proto::ClientError::user(
            eve_proto::UserErrorKey::DockingApproach
        ))
    );

    // 725 m surface distance (GH #1072 repro) -> structureDocking.Dock succeeds
    assert!(
        sys_h
            .set_ball_position(
                ship_id,
                keepstar_pos + DVec3::new(keepstar_radius + titan_radius + 725.0, 0.0, 0.0),
            )
            .await
    );
    ctx.dispatch_rpc(
        &mut sess,
        "structureDocking",
        None,
        "Dock",
        &[PyRep::Integer(keepstar_id), PyRep::Integer(ship_id)],
        &kw,
        None,
    )
    .await
    .expect("Avatar Titan at 725 m surface distance must dock at Keepstar");
    assert_eq!(sess.structure_id, Some(keepstar_id));

    // Undock Titan from Keepstar and verify beyonce.CmdDock also succeeds at 725 m surface distance.
    ctx.advance_secs(15);
    ctx.dispatch_rpc(&mut sess, "structureDocking", None, "Undock", &[], &kw, None)
        .await
        .expect("undock from Keepstar");
    ctx.advance_secs(15);
    assert!(
        sys_h
            .set_ball_position(
                ship_id,
                keepstar_pos + DVec3::new(keepstar_radius + titan_radius + 725.0, 0.0, 0.0),
            )
            .await
    );
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        None,
        "CmdDock",
        &[PyRep::Integer(keepstar_id), PyRep::Integer(ship_id)],
        &kw,
        None,
    )
    .await
    .expect("beyonce.CmdDock at 725 m surface distance must succeed for Avatar Titan");
    assert_eq!(sess.structure_id, Some(keepstar_id));

    // Verify Force Auxiliary (Apostle 37,604, radius 3,000 m > 2,500 m) jumps lowsec/nullsec stargate.
    ctx.advance_secs(15);
    ctx.dispatch_rpc(&mut sess, "structureDocking", None, "Undock", &[], &kw, None)
        .await
        .expect("undock before gate jump check");
    ctx.advance_secs(15);

    let gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| {
            j.from_solar_system_id != 30_100_000
                && j.to_solar_system_id != 30_100_000
                && ctx
                    .sde
                    .solar_systems
                    .get(&j.from_solar_system_id)
                    .is_some_and(|s| s.security_status < 0.45 && s.region_id != 10_000_070)
                && ctx
                    .sde
                    .solar_systems
                    .get(&j.to_solar_system_id)
                    .is_some_and(|s| s.security_status < 0.45 && s.region_id != 10_000_070)
        })
        .cloned()
        .expect("SDE must contain a non-Zarzakh non-Pochven lowsec/nullsec stargate");
    let fax_type_id = 37_604_u32;
    let fax_radius = ctx
        .sde
        .types
        .get(&fax_type_id)
        .map(|t| t.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or(3_000.0);
    assert!(fax_radius > 2_500.0);

    let _ = sys_h.take_ball(ship_id).await;
    let low_sys = gate.from_solar_system_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = low_sys;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(ship_it) = snap.items.get_mut(&ship_id) {
            ship_it.type_id = fax_type_id;
            ship_it.location_id = i64::from(low_sys);
        }
    });
    sess.solar_system_id = Some(low_sys);
    sess.station_id = None;
    sess.structure_id = None;

    let low_h = ctx.systems.get_or_spawn(low_sys);
    let gate_radius = low_h
        .get_ball_snapshot(gate.stargate_id)
        .await
        .unwrap()
        .map(|(_, b)| b.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or_else(|| {
            ctx.sde
                .types
                .get(&16)
                .map(|t| t.radius)
                .filter(|&r| r > 0.0)
                .unwrap_or(14_051.0)
        });
    let _ = low_h
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: fax_type_id,
                position: gate.position + DVec3::new(gate_radius + fax_radius + 2_501.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 60.0,
                inertia: 0.5,
                mass: 1_200_000_000.0,
                radius: fax_radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::new(),
        )
        .await;

    let far_gate_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdStargateJump",
            &[PyRep::Integer(gate.stargate_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(far_gate_err.code, "NotCloseEnoughToJump");

    assert!(
        low_h
            .set_ball_position(
                ship_id,
                gate.position + DVec3::new(gate_radius + fax_radius + 725.0, 0.0, 0.0),
            )
            .await
    );
    let jump_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdStargateJump",
            &[PyRep::Integer(gate.stargate_id)],
            &kw,
            None,
        )
        .await
        .expect("Apostle FAX at 725 m surface distance must jump stargate");
    assert_eq!(
        jump_res.dict_get("solarSystemID"),
        Some(&PyRep::Integer(gate.to_solar_system_id as i64))
    );
    assert_eq!(sess.solar_system_id, Some(gate.to_solar_system_id));
}

use jump_cloak::setup_nav_pilot;

fn is_non_special_roundtrip_gate(ctx: &ServerContext, j: &eve_sde::StargateJump) -> bool {
    j.from_solar_system_id != j.to_solar_system_id
        && j.from_solar_system_id != 30_100_000
        && j.to_solar_system_id != 30_100_000
        && ctx
            .sde
            .solar_systems
            .get(&j.from_solar_system_id)
            .is_some_and(|s| s.security_status >= 0.45 && s.region_id != 10_000_070)
        && ctx
            .sde
            .solar_systems
            .get(&j.to_solar_system_id)
            .is_some_and(|s| s.security_status >= 0.45 && s.region_id != 10_000_070)
        && ctx.sde.jumps.contains_key(&j.destination_stargate_id)
}

fn pick_non_special_roundtrip_gate(ctx: &ServerContext) -> eve_sde::StargateJump {
    ctx.sde
        .jumps
        .values()
        .filter(|j| is_non_special_roundtrip_gate(ctx, j))
        .min_by_key(|j| j.stargate_id)
        .cloned()
        .expect("SDE must contain a round-trip non-Pochven non-Zarzakh highsec stargate pair")
}

/// Resolves the exact stargate ball radius matching `populate_system_celestials` without spawning the system.
fn stargate_radius(ctx: &ServerContext, gate: &eve_sde::StargateJump) -> f64 {
    if let Some(cel) = ctx
        .sde
        .celestials_in_system_slice(gate.from_solar_system_id)
        .iter()
        .find(|c| c.celestial_id == gate.stargate_id)
    {
        if cel.radius > 0.0 {
            return cel.radius;
        }
        return ctx
            .sde
            .types
            .get(&cel.type_id)
            .map(|t| t.radius)
            .filter(|&r| r > 0.0)
            .unwrap_or(1_000.0);
    }
    let gate_type_id = ctx
        .sde
        .stargate_type_and_name(gate.stargate_id)
        .map(|(tid, _)| tid)
        .unwrap_or(16);
    ctx.sde
        .types
        .get(&gate_type_id)
        .or_else(|| ctx.sde.types.get(&16))
        .map(|t| t.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or(14_051.0)
}

async fn bind_beyonce_in_sys(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32) -> u64 {
    let kw = HashMap::new();
    let res = ctx
        .dispatch_rpc(
            sess,
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(sys_id as i64), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject(beyonce)");
    eve_testkit::client_harness::extract_bound_id_from_wire_rep(&res)
        .expect("MachoBindObject(beyonce) must return bound oid")
}

fn slim_activation_state(slim: &PyRep) -> Option<i64> {
    let entries = match slim {
        PyRep::Dict(entries) => entries,
        PyRep::Object { args, .. } => match args.as_ref() {
            PyRep::Dict(entries) => entries,
            _ => return None,
        },
        _ => return None,
    };
    entries
        .iter()
        .find(|(k, _)| matches!(k, PyRep::String(s) if s == "activationState"))
        .and_then(|(_, v)| v.as_i64())
}

#[tokio::test]
async fn gate_opens_through_activating_state() {
    // parity: feat:navigation.stargate-jump, destiny:OnSpecialFX
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.systems.set_lazy_stargate_activation(true);
    let gate = pick_non_special_roundtrip_gate(&ctx);
    let kw = HashMap::new();

    let char_id = 91_080_001_i64;
    let ship_id = 101_080_001_i64;
    let mut sess = setup_nav_pilot(
        &ctx,
        char_id,
        ship_id,
        587,
        gate.from_solar_system_id,
        gate.position + DVec3::new(30_000.0, 0.0, 0.0),
    )
    .await;
    let beyonce_id = bind_beyonce_in_sys(&ctx, &mut sess, gate.from_solar_system_id).await;
    let src_h = ctx.systems.get_handle(gate.from_solar_system_id).expect("source loaded");

    assert!(!ctx.systems.is_system_loaded(gate.to_solar_system_id));
    assert_eq!(
        src_h.stargate_activation_state(gate.stargate_id).map(|s| s.activation_state),
        Some(eve_destiny::STARGATE_ACTIVATION_STATE_CLOSED)
    );
    let closed_slim = src_h.get_ball_slim_item(gate.stargate_id).await.unwrap().unwrap();
    assert_eq!(
        slim_activation_state(&closed_slim),
        Some(eve_destiny::STARGATE_ACTIVATION_STATE_CLOSED)
    );

    // Approach intent lazily loads destination and transitions gate to ACTIVATING with deadline > now.
    let now_before_ms = ctx.now_machonet_wallclock_ms();
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdFollowBall",
        &[PyRep::Integer(gate.stargate_id), PyRep::Real(2_500.0)],
        &kw,
        None,
    )
    .await
    .expect("CmdFollowBall to stargate");

    assert!(ctx.systems.is_system_loaded(gate.to_solar_system_id));
    let activating = src_h
        .stargate_activation_state(gate.stargate_id)
        .expect("activating state");
    assert_eq!(
        activating.activation_state,
        eve_destiny::STARGATE_ACTIVATION_STATE_ACTIVATING
    );
    assert!(
        activating.activation_transition_at_ms > now_before_ms,
        "activationTransitionAt must be in the future while ACTIVATING"
    );

    // Stepping 1s stays ACTIVATING; jumping while ACTIVATING still raises GateClosed.
    src_h.step_manual(1.0).await.unwrap();
    assert_eq!(
        src_h.stargate_activation_state(gate.stargate_id).map(|s| s.activation_state),
        Some(eve_destiny::STARGATE_ACTIVATION_STATE_ACTIVATING)
    );

    // Stepping past STARGATE_ACTIVATION_TRANSITION_MS (~3s) opens the gate with transition 0.
    src_h.step_manual(1.0).await.unwrap();
    src_h.step_manual(1.0).await.unwrap();
    let open = src_h.stargate_activation_state(gate.stargate_id).unwrap();
    assert_eq!(open.activation_state, eve_destiny::STARGATE_ACTIVATION_STATE_OPEN);
    assert_eq!(open.activation_transition_at_ms, 0);
    assert_eq!(open.transition_remaining_ms, 0);
    let open_slim = src_h.get_ball_slim_item(gate.stargate_id).await.unwrap().unwrap();
    assert_eq!(
        slim_activation_state(&open_slim),
        Some(eve_destiny::STARGATE_ACTIVATION_STATE_OPEN)
    );
}

#[tokio::test]
async fn jump_closed_gate_raises_custominfo_gateclosed() {
    // parity: feat:navigation.stargate-jump
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.systems.set_lazy_stargate_activation(true);
    let gate = pick_non_special_roundtrip_gate(&ctx);
    let kw = HashMap::new();

    let gate_radius = stargate_radius(&ctx, &gate);
    let char_id = 91_080_002_i64;
    let ship_id = 101_080_002_i64;
    let mut sess = setup_nav_pilot(
        &ctx,
        char_id,
        ship_id,
        587,
        gate.from_solar_system_id,
        gate.position + DVec3::new(gate_radius + 1_000.0, 0.0, 0.0),
    )
    .await;
    let beyonce_id = bind_beyonce_in_sys(&ctx, &mut sess, gate.from_solar_system_id).await;

    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(gate.stargate_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "GateClosed");
    assert_eq!(sess.solar_system_id, Some(gate.from_solar_system_id));
    match err.to_client_error("beyonce", "CmdStargateJump") {
        eve_proto::ClientError::User { key, args } => {
            assert_eq!(key, eve_proto::UserErrorKey::CustomInfo);
            assert_eq!(
                args,
                vec![(
                    "info".to_string(),
                    PyRep::Tuple(vec![
                        PyRep::Integer(101),
                        PyRep::String(eve_proto::UI_LABEL_GATE_CLOSED.into()),
                    ]),
                )]
            );
        }
        other => panic!(
            "expected UserError('CustomInfo', info=(101, {})), got {other:?}",
            eve_proto::UI_LABEL_GATE_CLOSED
        ),
    }
}

async fn jump_outside_2500m_on_gate(
    ctx: &ServerContext,
    gate: &eve_sde::StargateJump,
    char_id: i64,
    ship_id: i64,
) -> (f64, f64, eve_proto::BridgeRpcError) {
    let kw = HashMap::new();
    let gate_radius = stargate_radius(ctx, gate);
    // setup_nav_pilot uses radius 120.0; place ship 2,600 m outside ball-to-ball surface perimeter.
    let mut sess = setup_nav_pilot(
        ctx,
        char_id,
        ship_id,
        587,
        gate.from_solar_system_id,
        gate.position + DVec3::new(gate_radius + 120.0 + 2_600.0, 0.0, 0.0),
    )
    .await;
    let src_h = ctx
        .systems
        .get_handle(gate.from_solar_system_id)
        .expect("source system loaded");
    let (_, gate_ball) = src_h
        .get_ball_snapshot(gate.stargate_id)
        .await
        .unwrap()
        .expect("stargate ball spawned");
    let beyonce_id = bind_beyonce_in_sys(ctx, &mut sess, gate.from_solar_system_id).await;

    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(gate.stargate_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    (gate_radius, gate_ball.radius, err)
}

#[tokio::test]
async fn jump_outside_2500m_raises_client_user_error() {
    // parity: feat:navigation.stargate-jump
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let gate = pick_non_special_roundtrip_gate(&ctx);
    let mut cases = vec![(gate.clone(), 91_080_003_i64, 101_080_003_i64)];
    if let Some(largest_gate) = ctx
        .sde
        .jumps
        .values()
        .filter(|j| is_non_special_roundtrip_gate(&ctx, j))
        .max_by(|a, b| {
            stargate_radius(&ctx, a)
                .total_cmp(&stargate_radius(&ctx, b))
                .then_with(|| a.stargate_id.cmp(&b.stargate_id))
        })
        .cloned()
        .filter(|g| g.stargate_id != gate.stargate_id)
    {
        cases.push((largest_gate, 91_080_103_i64, 101_080_103_i64));
    }
    for (target_gate, char_id, ship_id) in cases {
        let (gate_radius, ball_radius, err) =
            jump_outside_2500m_on_gate(&ctx, &target_gate, char_id, ship_id).await;
        assert!(
            (ball_radius - gate_radius).abs() < 1e-6,
            "stargate_radius ({gate_radius}) must match Ballpark gate ball radius ({ball_radius})"
        );
        assert_eq!(err.code, "NotCloseEnoughToJump");
        assert_eq!(
            err.client_presentation("beyonce", "CmdStargateJump"),
            eve_proto::UserErrorPresentation::Notify
        );
        match err.to_client_error("beyonce", "CmdStargateJump") {
            eve_proto::ClientError::User { key, args } => {
                assert_eq!(key, eve_proto::UserErrorKey::NotCloseEnoughToJump);
                assert!(
                    args.iter()
                        .any(|(k, v)| k == "maxdist" && *v == PyRep::Integer(2_500)),
                    "NotCloseEnoughToJump must carry maxdist=2500, got {args:?}"
                );
            }
            other => panic!("expected ClientError::User(NotCloseEnoughToJump), got {other:?}"),
        }
    }
}

#[tokio::test]
async fn closed_gate_jump_does_not_load_destination() {
    // parity: feat:navigation.stargate-jump
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.systems.set_lazy_stargate_activation(true);
    let gate = pick_non_special_roundtrip_gate(&ctx);
    let kw = HashMap::new();

    let gate_radius = stargate_radius(&ctx, &gate);
    let char_id = 91_080_004_i64;
    let ship_id = 101_080_004_i64;
    let mut sess = setup_nav_pilot(
        &ctx,
        char_id,
        ship_id,
        587,
        gate.from_solar_system_id,
        gate.position + DVec3::new(gate_radius + 800.0, 0.0, 0.0),
    )
    .await;
    let beyonce_id = bind_beyonce_in_sys(&ctx, &mut sess, gate.from_solar_system_id).await;

    assert!(!ctx.systems.is_system_loaded(gate.to_solar_system_id));
    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(gate.stargate_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "GateClosed");
    assert!(
        !ctx.systems.is_system_loaded(gate.to_solar_system_id),
        "jumping a closed gate must NOT lazily load destination system"
    );
}

#[tokio::test]
async fn gate_fx_v24_controller_events_and_cloak_isolation() {
    // parity: feat:navigation.stargate-jump, destiny:OnSpecialFX, destiny:OnClientControllerEvent, destiny:AddBalls2
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let gate = pick_non_special_roundtrip_gate(&ctx);
    let dest_gate = ctx.sde.jumps[&gate.destination_stargate_id].clone();
    let kw = HashMap::new();

    let gate_radius = stargate_radius(&ctx, &gate);
    let dest_gate_radius = stargate_radius(&ctx, &dest_gate);
    let char_id = 91_080_005_i64;
    let ship_id = 101_080_005_i64;
    let mut sess = setup_nav_pilot(
        &ctx,
        char_id,
        ship_id,
        587,
        gate.from_solar_system_id,
        gate.position + DVec3::new(gate_radius + 800.0, 0.0, 0.0),
    )
    .await;
    let beyonce_id = bind_beyonce_in_sys(&ctx, &mut sess, gate.from_solar_system_id).await;

    // Spawn source watcher and destination watcher on the respective gate grids.
    let src_watcher_id = 102_080_005_i64;
    let dest_watcher_id = 103_080_005_i64;
    let _ = setup_nav_pilot(
        &ctx,
        92_080_005,
        src_watcher_id,
        587,
        gate.from_solar_system_id,
        gate.position + DVec3::new(gate_radius + 1_200.0, 500.0, 0.0),
    )
    .await;
    let _ = setup_nav_pilot(
        &ctx,
        93_080_005,
        dest_watcher_id,
        587,
        gate.to_solar_system_id,
        dest_gate.position + DVec3::new(dest_gate_radius + 1_200.0, 500.0, 0.0),
    )
    .await;

    let src_h = ctx.systems.get_or_spawn(gate.from_solar_system_id);
    let dest_h = ctx.systems.get_or_spawn(gate.to_solar_system_id);
    src_h.push_set_state(src_watcher_id).await.unwrap();
    dest_h.push_set_state(dest_watcher_id).await.unwrap();
    let _ = src_h.drain_observer_updates(src_watcher_id).await.unwrap();
    let _ = dest_h.drain_observer_updates(dest_watcher_id).await.unwrap();

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdStargateJump",
        &[PyRep::Integer(gate.stargate_id)],
        &kw,
        None,
    )
    .await
    .expect("CmdStargateJump");

    let (_, src_events, _) = src_h.drain_observer_updates(src_watcher_id).await.unwrap();
    let (dest_tick, dest_events, _) = dest_h.drain_observer_updates(dest_watcher_id).await.unwrap();
    let (_, pilot_dest_events, _) = dest_h.drain_observer_updates(ship_id).await.unwrap();

    assert!(
        !src_events.iter().any(|ev| matches!(
            ev,
            eve_destiny::DestinyEvent::OnSpecialFXTimed { guid, .. } if guid == "effects.GateActivity"
        )),
        "source grid must not emit broken V24 effects.GateActivity (#1080), got {src_events:?}"
    );
    assert!(src_events.iter().any(|ev| matches!(
        ev,
        eve_destiny::DestinyEvent::OnClientControllerEvent {
            ball_id,
            target_id: Some(target),
            event_name,
            value: 1,
            delay: None,
        } if *ball_id == gate.stargate_id && *target == ship_id && event_name == "GateActivate"
    )));
    assert!(src_events.iter().any(|ev| matches!(
        ev,
        eve_destiny::DestinyEvent::OnSpecialFXTimed {
            ball_id,
            guid,
            graphic_info: Some(info),
            ..
        } if *ball_id == ship_id && guid == "effects.JumpOut" && info == &vec![gate.to_solar_system_id as i64]
    )));

    assert!(
        !dest_events.iter().any(|ev| matches!(
            ev,
            eve_destiny::DestinyEvent::OnSpecialFXTimed { guid, .. } if guid == "effects.GateActivity"
        )),
        "destination grid must not emit broken V24 effects.GateActivity (#1080), got {dest_events:?}"
    );
    assert!(
        dest_events.iter().any(|ev| matches!(
            ev,
            eve_destiny::DestinyEvent::OnClientControllerEvent {
                ball_id,
                target_id: Some(target),
                event_name,
                value: 1,
                delay: None,
            } if *ball_id == gate.destination_stargate_id && *target == ship_id && event_name == "Arrival"
        )),
        "destination grid must receive V24 Arrival controller event on destination stargate, got {dest_events:?}"
    );
    assert!(
        !pilot_dest_events.iter().any(|ev| matches!(
            ev,
            eve_destiny::DestinyEvent::OnSpecialFXTimed { guid, .. } if guid == "effects.GateActivity"
        )),
        "pilot destination queue must not contain effects.GateActivity (#1080): {pilot_dest_events:?}"
    );

    // #871 + #1084: while arriving pilot holds gate cloak, destination grid watcher must NOT
    // receive AddBalls2(ship_id) or ship-targeted OnSpecialFX(ship_id).
    assert!(
        !dest_events.iter().any(|ev| matches!(ev, eve_destiny::DestinyEvent::AddBalls2 { balls } if balls.iter().any(|b| b.id == ship_id))),
        "gate-cloaked arrival must not leak AddBalls2 to destination grid watcher: {dest_events:?}"
    );
    assert!(
        !dest_events.iter().any(|ev| matches!(ev, eve_destiny::DestinyEvent::OnSpecialFXTimed { ball_id, .. } if *ball_id == ship_id)),
        "gate-cloaked arrival must not emit unmaterialized OnSpecialFX(ship_id) to watcher: {dest_events:?}"
    );

    // Pilot's own destination queue starts with SetState (materializing ship_id) before long JumpIn FX.
    let pilot_set_state_idx = pilot_dest_events
        .iter()
        .position(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. }))
        .expect("pilot destination queue must begin with SetState");
    let pilot_jump_in_idx = pilot_dest_events
        .iter()
        .position(|ev| matches!(
            ev,
            eve_destiny::DestinyEvent::OnSpecialFXTimed {
                ball_id,
                guid,
                duration_ms: Some(d),
                ..
            } if *ball_id == ship_id && guid == "effects.JumpIn" && (*d - 5_000.0).abs() < f64::EPSILON
        ))
        .expect("pilot must receive 5000.0 ms JumpIn FX");
    assert!(pilot_set_state_idx < pilot_jump_in_idx);

    // #1077 E14: breaking gate cloak reveals AddBalls2 to destination watcher cleanly at/after arrival tick.
    assert!(dest_h.break_gate_cloak(ship_id).await.unwrap());
    let (decloak_tick, decloak_events, decloak_wire) =
        dest_h.drain_observer_updates(dest_watcher_id).await.unwrap();
    assert!(
        decloak_tick >= dest_tick,
        "decloak AddBalls2 tick {decloak_tick} must not precede arrival tick {dest_tick}"
    );
    assert!(
        decloak_events
            .iter()
            .any(|ev| matches!(ev, eve_destiny::DestinyEvent::AddBalls2 { balls } if balls.iter().any(|b| b.id == ship_id))),
        "destination grid watcher must receive AddBalls2 when pilot decloaks: {decloak_events:?}"
    );
    assert!(
        decloak_events
            .iter()
            .any(|ev| matches!(ev, eve_destiny::DestinyEvent::CloakBall { ball_id, cloaked: 0 } if *ball_id == ship_id)),
        "destination grid watcher must receive CloakBall(cloaked=0) on decloak: {decloak_events:?}"
    );
    let PyRep::List(wire_updates) = decloak_wire else {
        panic!("expected DoDestinyUpdate PyRep::List");
    };
    assert!(wire_updates.iter().all(|entry| match entry {
        PyRep::Tuple(pair) => pair[0].as_i64() == Some(decloak_tick as i64),
        _ => false,
    }));
}

#[tokio::test]
async fn tidi_notice_precedes_ballpark_on_jump_in() {
    // parity: feat:navigation.stargate-jump, feat:serverops.time-dilation, notify:DoSimClockRebase, destiny:AddBalls2
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let gate = pick_non_special_roundtrip_gate(&ctx);
    let kw = HashMap::new();

    let gate_radius = stargate_radius(&ctx, &gate);
    let char_id = 91_080_006_i64;
    let ship_id = 101_080_006_i64;
    let mut sess = setup_nav_pilot(
        &ctx,
        char_id,
        ship_id,
        587,
        gate.from_solar_system_id,
        gate.position + DVec3::new(gate_radius + 800.0, 0.0, 0.0),
    )
    .await;
    let beyonce_id = bind_beyonce_in_sys(&ctx, &mut sess, gate.from_solar_system_id).await;
    eve_server::services::serverops_cluster::set_system_tidi_factor(&ctx, gate.to_solar_system_id, 0.50);
    let _ = ctx.drain_notifications_for_char(char_id);

    let mut wire = eve_server::router::client_wire::ClientWireSession::from_session(&ctx, sess);
    let out = wire
        .call_wire(
            "beyonce",
            Some(beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(gate.stargate_id)],
            &kw,
        )
        .await;
    let jump_kv = out
        .rpc_result
        .as_ref()
        .expect("CmdStargateJump into TiDi failed");
    let PyRep::KeyVal(kv) = jump_kv else {
        panic!("expected KeyVal from CmdStargateJump, got {jump_kv:?}");
    };
    let dest_beyonce_id = kv
        .get("beyonceBoundID")
        .and_then(|v| v.as_i64())
        .expect("beyonceBoundID from CmdStargateJump") as u64;
    assert!(
        !out.out_frames.iter().any(|f| matches!(
            f,
            eve_server::outbound_sequencer::OutFrame::DoDestinyUpdate {
                has_set_state: true,
                ..
            }
        )),
        "CmdStargateJump must defer destination SetState until destination ballpark init"
    );
    let state_out = wire
        .call_wire("beyonce", Some(dest_beyonce_id), "UpdateStateRequest", &[], &kw)
        .await;
    assert!(
        state_out.rpc_result.is_ok(),
        "destination UpdateStateRequest after TiDi jump failed: {:?}",
        state_out.rpc_result
    );
    let all_frames: Vec<_> = out
        .out_frames
        .iter()
        .chain(state_out.out_frames.iter())
        .collect();

    let rebase_pos = all_frames
        .iter()
        .position(|f| {
            matches!(
                f,
                eve_server::outbound_sequencer::OutFrame::Notification { event, .. }
                    if event == "DoSimClockRebase"
            )
        })
        .expect("DoSimClockRebase must be emitted when jumping into dilated system");
    let set_state_pos = all_frames
        .iter()
        .position(|f| {
            matches!(
                f,
                eve_server::outbound_sequencer::OutFrame::DoDestinyUpdate {
                    has_set_state: true,
                    ..
                }
            )
        })
        .expect("SetState frame must be emitted on destination ballpark init");
    assert!(
        rebase_pos < set_state_pos,
        "DoSimClockRebase (idx {rebase_pos}) must precede first ballpark SetState (idx {set_state_pos})"
    );
}

#[tokio::test]
async fn jump_clock_continuity_across_tidi() {
    // parity: feat:navigation.stargate-jump, feat:navigation.session-change-timer, feat:serverops.time-dilation
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let gate = pick_non_special_roundtrip_gate(&ctx);
    let kw = HashMap::new();

    let gate_radius = stargate_radius(&ctx, &gate);
    let char_id = 91_080_007_i64;
    let ship_id = 101_080_007_i64;
    let mut sess = setup_nav_pilot(
        &ctx,
        char_id,
        ship_id,
        587,
        gate.from_solar_system_id,
        gate.position + DVec3::new(gate_radius + 800.0, 0.0, 0.0),
    )
    .await;
    let src_beyonce_id = bind_beyonce_in_sys(&ctx, &mut sess, gate.from_solar_system_id).await;
    eve_server::services::serverops_cluster::set_system_tidi_factor(&ctx, gate.to_solar_system_id, 0.50);
    let _ = ctx.drain_notifications_for_char(char_id);

    // Jump into 0.50 TiDi system: session change timer must remain 7,000 ms wall-clock independent of TiDi.
    let wall_before_in = ctx.now_machonet_wallclock_filetime_ticks();
    let res_in = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(src_beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(gate.stargate_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdStargateJump into 0.50 TiDi");
    let PyRep::KeyVal(kv_in) = res_in else {
        panic!("expected KeyVal from CmdStargateJump");
    };
    let dest_beyonce_id = kv_in.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    assert_eq!(
        kv_in.get("sessionChangeTimerSeconds").and_then(|v| v.as_i64()),
        Some(7)
    );
    assert_eq!(
        kv_in.get("sessionChangeTimerFiletime").and_then(|v| v.as_i64()),
        Some(wall_before_in + 70_000_000)
    );
    let _ = ctx.drain_notifications_for_char(char_id);

    // Advance 12s wall time inside 0.50 TiDi, drain intermediate clock tick notifications, move near return gate, and jump back to 1.0 TiDi.
    ctx.advance_secs(12);
    let _ = ctx.drain_notifications_for_char(char_id);
    let dest_gate = ctx.sde.jumps[&gate.destination_stargate_id].clone();
    let dest_gate_radius = stargate_radius(&ctx, &dest_gate);
    let dest_h = ctx.systems.get_handle(gate.to_solar_system_id).unwrap();
    assert!(
        dest_h
            .set_ball_position(
                ship_id,
                dest_gate.position + DVec3::new(dest_gate_radius + 800.0, 0.0, 0.0),
            )
            .await
    );

    let wall_before_out = ctx.now_machonet_wallclock_filetime_ticks();
    let res_out = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(dest_beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(gate.destination_stargate_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdStargateJump out of 0.50 TiDi");
    let PyRep::KeyVal(kv_out) = res_out else {
        panic!("expected KeyVal from return CmdStargateJump");
    };
    assert_eq!(
        kv_out.get("sessionChangeTimerSeconds").and_then(|v| v.as_i64()),
        Some(7)
    );
    assert_eq!(
        kv_out.get("sessionChangeTimerFiletime").and_then(|v| v.as_i64()),
        Some(wall_before_out + 70_000_000)
    );

    let out_notifs = ctx.drain_notifications_for_char(char_id);
    let rebase_tuple = out_notifs
        .iter()
        .find_map(|n| {
            if n.event == "DoSimClockRebase" {
                if let Some(PyRep::Tuple(pair)) = n.args.first() {
                    return Some((pair[0].as_i64().unwrap(), pair[1].as_i64().unwrap()));
                }
            }
            None
        })
        .expect("DoSimClockRebase on leaving dilated system");
    // Dilated clock ran at 0.50x for 12s (+6s = +60M ticks), jumping out advances by full 12s wall time (+120M ticks).
    assert_eq!(rebase_tuple.0, wall_before_in + 60_000_000);
    assert_eq!(rebase_tuple.1, wall_before_in + 120_000_000);
}
