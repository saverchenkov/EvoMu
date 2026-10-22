//! Integration tests for `SP-3` (Warp Navigation, Disruption, Capacitor Cost, & Exit Invulnerability),
//! `SP-4` (Stargate Capital Restrictions, Session-Change Timers, & Jump Cloak Immunities),
//! and `SP-5` (Station & Upwell Structure Docking, Undocking, & Ship Boarding).

use crate::common;

use eve_db::types::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
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
    sys_handle.spawn_ball(pod_ball).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Dock", &[PyRep::Integer(station_id)], &kw, None)
        .await
        .expect("Capsule must be allowed to dock in highsec during Criminal Timer");
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock capsule");
    let (_, mut restored_ball) = sys_handle.get_ball_snapshot(first_ship_id).await.unwrap().unwrap();
    restored_ball.type_id = saved_hull_tid;
    restored_ball.position = station_pos + DVec3::new(1_000.0, 0.0, 0.0);
    restored_ball.mode = MovementMode::Stop;
    sys_handle.spawn_ball(restored_ball).await.unwrap();

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(90_000_001).criminal_flag_until_unix = 0;
    }

    let _ = ctx.drain_notifications_for_char(char_id);
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
                last_damage_unix: 0,
            },
        );
    });

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

    // 4b. Capital ship attempting to dock at Medium structure (Astrahus 35832) -> ShipTooLargeForStructure
    let (_, mut cap_ball) = sys_handle
        .get_ball_snapshot(first_ship_id)
        .await
        .unwrap()
        .unwrap();
    let orig_type_id = cap_ball.type_id;
    cap_ball.type_id = 23757; // Archon (Carrier, group 547)
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

    // 4d. Authorized subcap within docking perimeter docks and undocks cleanly (also exercise beyonce.Dock)
    ctx.dispatch_rpc(&mut sess, "beyonce", None, "Dock", &[PyRep::Integer(struct_id)], &kw, None)
        .await
        .expect("beyonce.Dock at structure");
    ctx.advance_secs(11);
    ctx.dispatch_rpc(&mut sess, "structureDocking", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock after beyonce.Dock");
    let (_, mut dock_test_ball2) = sys_handle.get_ball_snapshot(first_ship_id).await.unwrap().unwrap();
    dock_test_ball2.position = struct_pos + DVec3::new(1_500.0, 0.0, 0.0);
    dock_test_ball2.mode = MovementMode::Stop;
    sys_handle.spawn_ball(dock_test_ball2).await.unwrap();

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

#[tokio::test]
async fn test_sp3_warp_to_disrupted_capacitor_cost_and_exit_invulnerability() {
    // parity: plan:SP-3, plan:S-1
    // parity: feat:navigation.warp-to, feat:navigation.warp-disrupted, feat:navigation.warp-capacitor-cost, feat:navigation.warp-exit-invulnerability, rpc:beyonce.UpdateStateRequest, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["plan:S-1", "feat:navigation.warp-to", "feat:navigation.warp-disrupted", "feat:navigation.warp-capacitor-cost", "feat:navigation.warp-exit-invulnerability"]);

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
                type_id: 47763,
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

    let expected_landing = station_pos - DVec3::new(min_range, 0.0, 0.0);
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
        .unwrap();
    let attacker_locks = sys_handle.get_target_locks(attacker_ball_id).await.unwrap();
    assert!(attacker_locks.is_empty());

    let fil_err = ctx
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
    assert_eq!(fil_err.code, "ShipInvulnerable");

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

#[tokio::test]
async fn test_sp4_stargate_capital_restriction_session_timers_and_jump_cloak_immunities() {
    // parity: plan:SP-4, plan:S-1
    // parity: feat:navigation.stargate-capital-restriction, feat:navigation.jump-cloak-immunities, rpc:beyonce.SetSessionCheck, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["plan:S-1", "feat:navigation.stargate-capital-restriction", "feat:navigation.jump-cloak-immunities"]);

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
        jita_handle.spawn_ball(b).await.unwrap();

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
