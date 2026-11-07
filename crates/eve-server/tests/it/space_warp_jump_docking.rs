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
    assert!(cap_notes.iter().any(|n| n.0 == "OnWarpStarted"));

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
        if let Some(corp) = s.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER);
        }
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
        if let Some(corp) = s.corporations.get_mut(&corp_id) { corp.member_roles.clear(); }
    });
    let denied = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(fuel2_id), PyRep::Integer(ship_id), PyRep::Integer(5)], &kw, None).await;
    let err_denied = denied.expect_err("R-FUEL-01: withdrawal without role must be rejected");
    assert_eq!(err_denied.code, "CorpRoleDenied");
    let item_denied = ctx.db.get_item(fuel2_id).unwrap();
    assert_eq!(item_denied.owner_id, corp_id);
    assert_eq!(item_denied.location_id, structure_id);

    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(corp) = s.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        }
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
