//! Space physics, cyno jumps, scans, SDE celestials as balls (`SP-1`), spatial bubbles (`SP-2`),
//! and binary Destiny streams (`P-7`) integration tests.

use eve_destiny::wire::decode_add_balls;
use eve_destiny::{Ball, DestinyEvent, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_phase_0_s3_cyno_jump_unimplemented_rpc() {
    // parity: plan:S-3, rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:structureCynoBeaconMgr.CmdJumpToStructureBeacon
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(903);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let s3_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[
                PyRep::Integer(30_002_187),
                PyRep::Integer(1_040_000_000_001),
            ],
            &kw,
            None,
        )
        .await;
    assert!(matches!(
        s3_err.unwrap_err().code.as_str(),
        "UnimplementedRPC" | "ShipDocked" | "BeaconNotFound"
    ));
    assert_eq!(sess.solar_system_id, Some(30_000_142));
    assert_eq!(
        ctx.db
            .get_character(90_000_001)
            .ok()
            .map(|c| (c.solar_system_id, c.station_id)),
        Some((30_000_142, Some(60_003_760)))
    );
}

#[tokio::test]
async fn test_phase_0_s5_no_unconditional_demo_spawns_on_empty_world() {
    // parity: plan:S-5
    use eve_db::{IskCents, WorldRepository};

    let empty_repo = WorldRepository::new_empty();
    empty_repo.with_snapshot(|snap| {
        assert!(snap.market_orders.is_empty());
        assert!(snap.system_states.is_empty());
    });

    let ctx = ServerContext::new();
    assert!(
        ctx.living_world.list_convoys().is_empty(),
        "S-5: Fresh ServerContext must not spawn demo convoys 800_000_001..003 by default"
    );
    ctx.db.with_snapshot(|snap| {
        for (sys_id, st) in &snap.system_states {
            assert_eq!(
                st.ess_main_bank,
                IskCents(0),
                "S-5: System {sys_id} ess_main_bank must start at 0"
            );
            assert_eq!(
                st.ess_reserve_bank,
                IskCents(0),
                "S-5: System {sys_id} ess_reserve_bank must start at 0"
            );
            assert_eq!(
                st.wormhole_destination_system_id, None,
                "S-5: System {sys_id} wormhole destination must be None without fixture seed"
            );
            assert_eq!(
                st.signature_position, None,
                "S-5: System {sys_id} signature_position must be None without fixture seed"
            );
            assert_eq!(
                st.fw_complex_position, None,
                "S-5: System {sys_id} fw_complex_position must be None without fixture seed"
            );
        }
    });
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let balls = sys.list_balls().await.expect("list_balls");
    assert!(
        balls
            .iter()
            .all(|b| matches!(b.mode, MovementMode::Rigid) && b.id < 700_000_000),
        "S-5: Fresh SystemActor ballpark must contain 0 hardcoded NPC/demo balls (only static SDE celestials)"
    );
}

#[tokio::test]
async fn test_phase_0_s6_request_scans_unimplemented_rpc() {
    // parity: plan:S-6, rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(906);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let s6_err = ctx
        .dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[], &kw, None)
        .await;
    assert_eq!(s6_err.unwrap_err().code, "UnimplementedRPC");

    let sys = ctx.systems.get_or_spawn(30_000_142);
    let balls = sys.list_balls().await.expect("list_balls");
    assert!(
        !balls.iter().any(|b| b.id == 750_000_001),
        "S-6: RequestScans must not spawn hardcoded wormhole ball 750_000_001"
    );
}

#[tokio::test]
// parity: plan:SP-1, feat:navigation.celestials-balls
async fn test_sp1_jita_and_perimeter_celestials_gates_belts_stations_as_balls() {
    let ctx = ServerContext::new();

    // 1. Verify Jita (`30000142`) loads Star, 8 Planets, 33 Moons, 7 Stargates, and 18 Stations
    let jita = ctx.systems.get_or_spawn(30_000_142);
    let jita_balls = jita.list_balls().await.expect("list_balls in Jita");
    assert_eq!(
        jita_balls.len(),
        67,
        "Jita must contain 49 celestials/gates + 18 stations = 67 static balls"
    );

    // Verify Jita Star (`40009076`)
    let (_, star) = jita
        .get_ball_snapshot(40_009_076)
        .await
        .expect("query star")
        .expect("Jita star 40009076 must exist");
    assert_eq!(star.type_id, 3796);
    assert_eq!(star.position, DVec3::ZERO);
    assert!((star.radius - 513_100_000.0).abs() < 1.0);
    assert_eq!(jita.get_ball_group_id(40_009_076).await.unwrap(), Some(6));

    // Verify Jita IV (`40009082`, Planet group 7) and Jita IV - Moon 4 (`40009087`, Moon group 8)
    let (_, jita_iv) = jita
        .get_ball_snapshot(40_009_082)
        .await
        .expect("query Jita IV")
        .expect("Jita IV 40009082 must exist");
    assert_eq!(jita_iv.type_id, 13);
    assert!(
        (jita_iv.position - DVec3::new(-107_354_576_606.0, -18_753_785_170.0, 436_797_007_078.0))
            .length()
            < 1.0
    );
    assert_eq!(jita.get_ball_group_id(40_009_082).await.unwrap(), Some(7));

    let (_, moon_4) = jita
        .get_ball_snapshot(40_009_087)
        .await
        .expect("query Jita IV - Moon 4")
        .expect("Jita IV - Moon 4 40009087 must exist");
    assert_eq!(moon_4.type_id, 52673);
    assert_eq!(jita.get_ball_group_id(40_009_087).await.unwrap(), Some(8));

    // Verify Stargate (Perimeter) (`50001248`, Stargate group 10) and Station (`60003760`, Station group 15)
    let (_, perim_gate) = jita
        .get_ball_snapshot(50_001_248)
        .await
        .expect("query Perimeter gate")
        .expect("Perimeter stargate 50001248 must exist");
    assert_eq!(perim_gate.type_id, 16);
    assert!(perim_gate.radius > 0.0);
    assert_eq!(jita.get_ball_group_id(50_001_248).await.unwrap(), Some(10));

    let (_, cnap) = jita
        .get_ball_snapshot(60_003_760)
        .await
        .expect("query Jita 4-4 CNAP")
        .expect("Jita 4-4 station 60003760 must exist");
    assert_eq!(cnap.type_id, 52678);
    assert!(cnap.radius > 0.0);
    assert_eq!(jita.get_ball_group_id(60_003_760).await.unwrap(), Some(15));

    // 2. Verify Perimeter (`30000144`) loads Star, 10 Planets, 10 Moons, 3 Asteroid Belts, 5 Stargates, 5 Stations
    let perim = ctx.systems.get_or_spawn(30_000_144);
    let perim_balls = perim.list_balls().await.expect("list_balls in Perimeter");
    assert_eq!(
        perim_balls.len(),
        34,
        "Perimeter must contain 29 celestials/belts/gates + 5 stations = 34 static balls"
    );
    for belt_id in [40_009_232_i64, 40_009_245, 40_009_249] {
        let (_, belt) = perim
            .get_ball_snapshot(belt_id)
            .await
            .expect("query Perimeter asteroid belt")
            .unwrap_or_else(|| panic!("Perimeter asteroid belt {belt_id} must exist"));
        assert_eq!(belt.type_id, 15);
        assert_eq!(perim.get_ball_group_id(belt_id).await.unwrap(), Some(9));
    }
}

#[tokio::test]
// parity: plan:P-7, plan:SP-1, plan:SP-4, feat:navigation.session-change-timer, feat:navigation.destiny-updates, feat:navigation.celestials-balls, feat:serverops.server-tick
// parity: rpc:beyonce.AdminGetBubbleID
// parity: notify:DoDestinyUpdate
// parity: destiny:AddBalls2, destiny:RemoveBalls, destiny:RemoveBall
// parity: destiny:GotoDirection, destiny:FollowBall, destiny:Orbit, destiny:Stop
async fn test_p7_destiny_binary_stream_undock_movement_and_stargate_jump() {
    // parity: rpc:authentication.Login, rpc:beyonce.AdminGetBubbleID, rpc:beyonce.GetFormations, rpc:beyonce.UpdateStateRequest, rpc:charUnboundMgr.SelectCharacterID
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(970);

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. Undock in Jita -> returns beyonceBoundID, shipID, solarSystemID, spawnTick (without invented destinyState/destinyUpdates keys)
    // and queues SetState as a DoDestinyUpdate notification
    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(undock_kv) = &undock else {
        panic!("Expected KeyVal from Undock");
    };
    assert!(!undock_kv.contains_key("destinyState"));
    assert!(!undock_kv.contains_key("destinyUpdates"));
    let beyonce_id = undock_kv["beyonceBoundID"].as_i64().unwrap() as u64;
    let ship_id = undock_kv["shipID"].as_i64().unwrap();

    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    let (_tick, undock_evs, undock_updates_pyrep) = jita_handle
        .drain_observer_updates(ship_id)
        .await
        .expect("drain Undock SetState update");
    assert_eq!(undock_evs.len(), 1);
    assert!(matches!(
        undock_evs[0],
        DestinyEvent::SetState {
            observer_ball_id
        } if observer_ball_id == ship_id
    ));
    let PyRep::List(undock_updates_list) = &undock_updates_pyrep else {
        panic!("Expected List of DoDestinyUpdate tuples from Undock");
    };
    assert_eq!(undock_updates_list.len(), 1);

    // 2. Call beyonce.UpdateStateRequest -> returns PyRep::None and queues SetState via DoDestinyUpdate
    let update_state_ret = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "UpdateStateRequest",
            &[],
            &kw,
            None,
        )
        .await
        .expect("UpdateStateRequest");
    assert_eq!(
        update_state_ret,
        PyRep::None,
        "BeyonceService::Handle_UpdateStateRequest must return PyNone and push SetState via DoDestinyUpdate"
    );

    let (_tick, state_evs, update_state) = jita_handle
        .drain_observer_updates(ship_id)
        .await
        .expect("drain UpdateStateRequest DoDestinyUpdate");
    assert_eq!(state_evs.len(), 1);
    assert!(matches!(
        state_evs[0],
        DestinyEvent::SetState {
            observer_ball_id
        } if observer_ball_id == ship_id
    ));

    let marshaled = eve_proto::marshal::encode(&update_state);
    assert_eq!(marshaled[0], 0x7E);
    let unmarshaled =
        eve_proto::marshal::decode(&marshaled).expect("UpdateStateRequest must unmarshal cleanly");
    assert_eq!(unmarshaled, update_state);

    let PyRep::List(updates) = &update_state else {
        panic!("Expected List of DoDestinyUpdate tuples");
    };
    assert_eq!(updates.len(), 1);
    let PyRep::Tuple(entry) = &updates[0] else {
        panic!("Expected (stamp, action) tuple");
    };
    let PyRep::Tuple(action) = &entry[1] else {
        panic!("Expected (action_name, args) tuple");
    };
    assert_eq!(action[0].as_str(), Some("SetState"));
    let PyRep::Tuple(args) = &action[1] else {
        panic!("Expected args tuple");
    };
    let PyRep::KeyVal(state_kv) = &args[0] else {
        panic!("Expected SetState KeyVal");
    };
    assert_eq!(state_kv["ego"].as_i64(), Some(ship_id));
    assert!(state_kv.contains_key("aggressors"));
    assert!(state_kv.contains_key("droneState"));
    assert!(state_kv.contains_key("solItem"));
    let PyRep::Bytes(raw_state_bytes) = &state_kv["state"] else {
        panic!("Expected PyRep::Bytes under SetState['state']");
    };
    let (pkt_type, _stamp, decoded_balls) =
        decode_add_balls(raw_state_bytes).expect("decode_add_balls on SetState");
    assert_eq!(pkt_type, 0);
    assert_eq!(
        decoded_balls.len(),
        68,
        "SetState must include all 67 Jita static balls + 1 player ship ball"
    );
    assert!(decoded_balls.iter().any(|b| b.id == 40_009_076));
    assert!(decoded_balls.iter().any(|b| b.id == 50_001_248));
    assert!(decoded_balls.iter().any(|b| b.id == 60_003_760));
    assert!(decoded_balls.iter().any(|b| b.id == ship_id));

    // Verify SlimItem metadata is sourced from SDE and world DB (not hardcoded ownerID=1 / groupID=25)
    let PyRep::List(slims) = &state_kv["slims"] else {
        panic!("Expected PyRep::List under SetState['slims']");
    };
    assert_eq!(slims.len(), 68);
    let find_slim = |target_id: i64| -> &PyRep {
        slims
            .iter()
            .find(|s| {
                if let PyRep::Object { type_name, args } = s {
                    assert_eq!(type_name, "foo.SlimItem");
                    // foo.SlimItem has no __setstate__: its state must be a plain dict.
                    assert!(matches!(args.as_ref(), PyRep::Dict(_)));
                    return s.dict_get("itemID").and_then(|v| v.as_i64()) == Some(target_id);
                }
                false
            })
            .unwrap_or_else(|| panic!("SlimItem for {target_id} must exist"))
    };
    let ship_slim = find_slim(ship_id);
    assert_eq!(ship_slim.dict_get("ownerID").and_then(PyRep::as_i64), Some(90_000_001));
    assert_eq!(ship_slim.dict_get("charID").and_then(PyRep::as_i64), Some(90_000_001));
    assert_eq!(ship_slim.dict_get("corpID").and_then(PyRep::as_i64), Some(1_000_170));
    assert_eq!(ship_slim.dict_get("groupID").and_then(PyRep::as_i64), Some(25));
    assert_eq!(ship_slim.dict_get("categoryID").and_then(PyRep::as_i64), Some(6));

    let cnap_slim = find_slim(60_003_760);
    assert_eq!(cnap_slim.dict_get("groupID").and_then(PyRep::as_i64), Some(15));
    assert_eq!(cnap_slim.dict_get("categoryID").and_then(PyRep::as_i64), Some(3));

    let star_slim = find_slim(40_009_076);
    assert_eq!(star_slim.dict_get("groupID").and_then(PyRep::as_i64), Some(6));
    assert_eq!(star_slim.dict_get("categoryID").and_then(PyRep::as_i64), Some(2));

    // 3. Verify beyonce.GetFormations returns the V24 _destiny.so 2-tuple and invented beyonce.GetState returns UnimplementedRPC
    let formations = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "GetFormations",
            &[],
            &kw,
            None,
        )
        .await
        .expect("beyonce.GetFormations must return V24 formations tuple");
    let PyRep::Tuple(form_tuple) = &formations else {
        panic!("Expected 2-tuple from beyonce.GetFormations");
    };
    assert_eq!(form_tuple.len(), 2);

    let get_state_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "GetState",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("Invented beyonce.GetState must return UnimplementedRPC");
    assert_eq!(get_state_err.code, "UnimplementedRPC");

    // 4. Verify beyonce.AdminGetBubbleID role gate, visibility check, and real GridBubble.id
    let non_gm_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "AdminGetBubbleID",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("Non-GM caller must be rejected by AdminGetBubbleID");
    assert_eq!(non_gm_err.code, "InsufficientRoles");

    sess.role |= eve_machonet::service_roles::ROLE_GML;
    let bubble_id = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "AdminGetBubbleID",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GM AdminGetBubbleID")
        .as_i64()
        .expect("bubble_id integer");
    let expected_grid_id = jita_handle
        .get_bubble_id_for_ball(ship_id)
        .await
        .unwrap()
        .unwrap() as i64;
    assert_eq!(bubble_id, expected_grid_id);

    // Spawn an invisible ball in a distant grid bubble and verify AdminGetBubbleID rejects querying it
    let distant_ball_id = 100_999_001;
    jita_handle
        .spawn_ball(Ball {
            id: distant_ball_id,
            type_id: 587,
            position: DVec3::new(50_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    let invis_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "AdminGetBubbleID",
            &[PyRep::Integer(distant_ball_id)],
            &kw,
            None,
        )
        .await
        .expect_err("AdminGetBubbleID must reject balls outside caller's visible bubble");
    assert_eq!(invis_err.code, "BallNotVisible");
    let _ = jita_handle.remove_ball(distant_ball_id).await;

    // 5. Issue movement commands (CmdSetSpeedFraction, CmdGotoDirection, CmdFollowBall, CmdOrbit, CmdStop, CmdWarpToStuff to Perimeter gate 50001248)
    let _ = jita_handle
        .drain_observer_updates(ship_id)
        .await
        .expect("clear observer updates");

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdSetSpeedFraction",
        &[PyRep::Real(0.75)],
        &kw,
        None,
    )
    .await
    .expect("CmdSetSpeedFraction");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdGotoDirection",
        &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .expect("CmdGotoDirection");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdFollowBall",
        &[PyRep::Integer(60_003_760), PyRep::Real(2_500.0)],
        &kw,
        None,
    )
    .await
    .expect("CmdFollowBall");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdOrbit",
        &[PyRep::Integer(60_003_760), PyRep::Real(5_000.0)],
        &kw,
        None,
    )
    .await
    .expect("CmdOrbit");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdStop",
        &[],
        &kw,
        None,
    )
    .await
    .expect("CmdStop");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_id),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(50_001_249)],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuff to Perimeter Stargate 50001249");

    // While warping, UpdateStateRequest must return PyRep::None without pushing SetState (BeyonceService.cpp:790-792)
    let warp_ustate_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "UpdateStateRequest",
            &[],
            &kw,
            None,
        )
        .await
        .expect("UpdateStateRequest while warping must return PyRep::None");
    assert_eq!(warp_ustate_res, PyRep::None);

    let (_tick, evs, pyrep_updates) = jita_handle
        .drain_observer_updates(ship_id)
        .await
        .expect("drain movement observer updates");
    // SetSpeedFraction, GotoDirection, FollowBall, Orbit, Stop, WarpTo + OnSpecialFX('effects.Warping')
    assert_eq!(evs.len(), 7);
    let encoded_updates = eve_proto::marshal::encode(&pyrep_updates);
    let decoded_updates =
        eve_proto::marshal::decode(&encoded_updates).expect("marshal roundtrip on movement updates");
    assert_eq!(decoded_updates, pyrep_updates);

    let PyRep::List(action_items) = &pyrep_updates else {
        panic!("Expected List of DoDestinyUpdate entries");
    };
    let extract_action = |idx: usize| -> (&str, &[PyRep]) {
        let PyRep::Tuple(outer) = &action_items[idx] else {
            panic!("Expected (stamp, action) tuple");
        };
        let PyRep::Tuple(inner) = &outer[1] else {
            panic!("Expected (name, args) tuple");
        };
        let name = inner[0].as_str().expect("action name");
        let PyRep::Tuple(args) = &inner[1] else {
            panic!("Expected args tuple");
        };
        (name, args.as_slice())
    };
    let (a0, args0) = extract_action(0);
    assert_eq!(a0, "SetSpeedFraction");
    assert_eq!(args0, &[PyRep::Integer(ship_id), PyRep::Real(0.75)]);

    let (a1, args1) = extract_action(1);
    assert_eq!(a1, "GotoDirection");
    assert_eq!(
        args1,
        &[
            PyRep::Integer(ship_id),
            PyRep::Real(1.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0)
        ]
    );

    let (a2, args2) = extract_action(2);
    assert_eq!(a2, "FollowBall");
    assert_eq!(
        args2,
        &[
            PyRep::Integer(ship_id),
            PyRep::Integer(60_003_760),
            PyRep::Real(2_500.0)
        ]
    );

    let (a3, args3) = extract_action(3);
    assert_eq!(a3, "Orbit");
    assert_eq!(
        args3,
        &[
            PyRep::Integer(ship_id),
            PyRep::Integer(60_003_760),
            PyRep::Real(5_000.0)
        ]
    );

    let (a4, args4) = extract_action(4);
    assert_eq!(a4, "Stop");
    assert_eq!(args4, &[PyRep::Integer(ship_id)]);

    let (a5, args5) = extract_action(5);
    assert_eq!(a5, "WarpTo");
    assert_eq!(args5[0], PyRep::Integer(ship_id));
    assert_eq!(args5.len(), 6);
    assert_eq!(args5[4], PyRep::Integer(0));
    assert_eq!(args5[5], PyRep::Integer(50));

    // Warp tunnel visual goes out as OnSpecialFX('effects.Warping'); OnActivatingWarp is raised by
    // the client's own destiny tick and must never appear on the wire.
    let (a6, args6) = extract_action(6);
    assert_eq!(a6, "OnSpecialFX");
    assert_eq!(args6[0], PyRep::Integer(ship_id));
    assert!(args6.iter().any(|a| a.as_str() == Some("effects.Warping")));
    assert!((0..7).all(|i| extract_action(i).0 != "OnActivatingWarp"));

    // 6. Step Jita ticks until ship completes warp at Perimeter Stargate (50001249), verifying 1-second tick advancement and per-tick DoDestinyUpdate stamps
    let mut prev_tick = jita_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .unwrap()
        .0;
    for _ in 0..40 {
        let tb = jita_handle.step_manual(1.0).await.expect("step_manual");
        assert!(tb.tick > prev_tick, "1-second step must advance destiny tick counter");
        prev_tick = tb.tick;
        let (_, snap) = jita_handle
            .get_ball_snapshot(ship_id)
            .await
            .unwrap()
            .unwrap();
        if matches!(snap.mode, MovementMode::Stop) {
            break;
        }
    }
    let _ = jita_handle.drain_observer_updates(ship_id).await;
    let tb1 = jita_handle.step_manual(1.0).await.expect("step_manual t1");
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_id), "CmdStop", &[], &kw, None)
        .await
        .expect("CmdStop at t1");
    let (_t1, _ev1, py1) = jita_handle.drain_observer_updates(ship_id).await.unwrap();
    let tb2 = jita_handle.step_manual(1.0).await.expect("step_manual t2");
    assert_eq!(tb2.tick, tb1.tick + 1);
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_id), "CmdStop", &[], &kw, None)
        .await
        .expect("CmdStop at t2");
    let (_t2, _ev2, py2) = jita_handle.drain_observer_updates(ship_id).await.unwrap();
    let PyRep::List(l1) = &py1 else { panic!("Expected list") };
    let PyRep::List(l2) = &py2 else { panic!("Expected list") };
    let PyRep::Tuple(tup1) = &l1[0] else { panic!("Expected tuple") };
    let PyRep::Tuple(tup2) = &l2[0] else { panic!("Expected tuple") };
    assert_eq!(tup1[0].as_i64(), Some(tb1.tick as i64));
    assert_eq!(tup2[0].as_i64(), Some(tb2.tick as i64));
    let (_, gate_ball) = jita_handle
        .get_ball_snapshot(50_001_249)
        .await
        .unwrap()
        .unwrap();
    // Spawn a gate observer in Jita at the Perimeter stargate to verify RemoveBall is emitted on jump
    let gate_observer_id = 100_888_001;
    jita_handle
        .spawn_ball(Ball {
            id: gate_observer_id,
            type_id: 587,
            position: gate_ball.position + DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    let _ = jita_handle.drain_observer_updates(gate_observer_id).await;

    ctx.advance_secs(12);
    let jump_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(50_001_249)],
            &kw,
            None,
        )
        .await
        .expect("CmdStargateJump from Jita to Perimeter");
    let PyRep::KeyVal(jump_kv) = &jump_res else {
        panic!("Expected KeyVal from CmdStargateJump");
    };
    assert_eq!(jump_kv["solarSystemID"].as_i64(), Some(30_000_144));
    assert!(!jump_kv.contains_key("destinyState"));
    assert!(!jump_kv.contains_key("destinyUpdates"));
    assert_eq!(sess.solar_system_id, Some(30_000_144));

    // Gate observer in Jita must have received RemoveBall for the jumping ship
    let (_t, gate_obs_evs, gate_obs_pyrep) = jita_handle
        .drain_observer_updates(gate_observer_id)
        .await
        .unwrap();
    assert!(
        gate_obs_evs
            .iter()
            .any(|e| matches!(e, DestinyEvent::RemoveBall { ball_id } if *ball_id == ship_id)),
        "Jita gate observer must receive RemoveBall when ship jumps through stargate"
    );
    let PyRep::List(gate_obs_list) = &gate_obs_pyrep else {
        panic!("Expected DoDestinyUpdate list for gate observer");
    };
    assert!(!gate_obs_list.is_empty());
    let has_1arg_remove_ball = gate_obs_list.iter().any(|entry| {
        if let PyRep::Tuple(outer) = entry {
            if let Some(PyRep::Tuple(inner)) = outer.get(1) {
                if inner.first().and_then(|v| v.as_str()) == Some("RemoveBall") {
                    if let Some(PyRep::Tuple(args)) = inner.get(1) {
                        return args.as_slice() == [PyRep::Integer(ship_id)];
                    }
                }
            }
        }
        false
    });
    assert!(
        has_1arg_remove_ball,
        "RemoveBall wire action must be 1-arg (entityID,) per Destiny.xmlp:339-348"
    );

    // Jumping ship in Perimeter (30000144) must have received SetState with all 34 Perimeter static balls + 1 player ship ball
    let perim_handle = ctx.systems.get_or_spawn(30_000_144);
    let (_t, perim_evs, perim_updates) = perim_handle
        .drain_observer_updates(ship_id)
        .await
        .expect("drain Perimeter SetState after jump");
    assert_eq!(perim_evs.len(), 1);
    assert!(matches!(
        perim_evs[0],
        DestinyEvent::SetState { observer_ball_id } if observer_ball_id == ship_id
    ));
    let PyRep::List(perim_list) = &perim_updates else {
        panic!("Expected DoDestinyUpdate list in Perimeter");
    };
    let PyRep::Tuple(p_entry) = &perim_list[0] else {
        panic!("Expected tuple");
    };
    let PyRep::Tuple(p_action) = &p_entry[1] else {
        panic!("Expected action tuple");
    };
    assert_eq!(p_action[0].as_str(), Some("SetState"));
    let PyRep::Tuple(p_args) = &p_action[1] else {
        panic!("Expected args tuple");
    };
    let PyRep::KeyVal(p_kv) = &p_args[0] else {
        panic!("Expected SetState KeyVal");
    };
    let PyRep::Bytes(p_state_bytes) = &p_kv["state"] else {
        panic!("Expected SetState state bytes");
    };
    let (_, _, perim_decoded_balls) =
        decode_add_balls(p_state_bytes).expect("decode Perimeter SetState");
    assert_eq!(
        perim_decoded_balls.len(),
        35,
        "Perimeter SetState must contain 34 static balls + 1 player ship ball"
    );
}

#[tokio::test]
// parity: plan:SP-2, feat:navigation.bubbles-grid, destiny:AddBalls2, destiny:RemoveBalls, rpc:beyonce.AdminGetBubbleID
async fn test_sp2_spatial_bubbles_10000km_isolation_and_inter_bubble_warp() {
    let ctx = ServerContext::new();
    let jita = ctx.systems.get_or_spawn(30_000_142);

    // Spawn Ship 1 at (0, 0, 0) [creates Grid 1 with fixed walls at [-8,000 km, +8,000 km]],
    // Ship 2 at (+10,000 km, 0, 0) [outside Grid 1's +8,000 km wall -> creates non-overlapping Grid 2],
    // Ship 3 at (+5,000 km, 0, 0) [inside Grid 1],
    // and Ship 5 at (-5,000 km, 0, 0) [inside Grid 1, 10,000 km from Ship 3]
    let ship_1_id = 100_500_001;
    let ship_2_id = 100_500_002;
    let ship_3_id = 100_500_003;
    let ship_5_id = 100_500_005;

    let make_ship = |id: i64, pos: DVec3| Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: DVec3::new(365.0, 0.0, 0.0),
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };

    jita.spawn_ball(make_ship(ship_1_id, DVec3::ZERO))
        .await
        .expect("spawn ship 1");
    jita.spawn_ball(make_ship(ship_2_id, DVec3::new(10_000_000.0, 0.0, 0.0)))
        .await
        .expect("spawn ship 2 at +10,000 km");
    jita.spawn_ball(make_ship(ship_3_id, DVec3::new(5_000_000.0, 0.0, 0.0)))
        .await
        .expect("spawn ship 3 at +5,000 km");
    jita.spawn_ball(make_ship(ship_5_id, DVec3::new(-5_000_000.0, 0.0, 0.0)))
        .await
        .expect("spawn ship 5 at -5,000 km");

    // 1. Verify shared fixed-boundary GridBubbles (transitivity + grid walls):
    let gid_1 = jita.get_bubble_id_for_ball(ship_1_id).await.unwrap().unwrap();
    let gid_2 = jita.get_bubble_id_for_ball(ship_2_id).await.unwrap().unwrap();
    let gid_3 = jita.get_bubble_id_for_ball(ship_3_id).await.unwrap().unwrap();
    let gid_5 = jita.get_bubble_id_for_ball(ship_5_id).await.unwrap().unwrap();
    assert_eq!(gid_1, gid_3);
    assert_eq!(gid_1, gid_5);
    assert_ne!(gid_1, gid_2);

    // Ship 3 (+5,000 km) sees Ship 5 (-5,000 km, 10,000 km away in same Grid 1),
    // but does NOT see Ship 2 (+10,000 km, only 5,000 km away across the +8,000 km grid wall in Grid 2)!
    let dyn_3: Vec<i64> = jita
        .list_dynamic_balls_in_bubble(ship_3_id)
        .await
        .expect("dyn_3")
        .into_iter()
        .map(|b| b.id)
        .collect();
    assert!(dyn_3.contains(&ship_1_id));
    assert!(dyn_3.contains(&ship_3_id));
    assert!(
        dyn_3.contains(&ship_5_id),
        "Ship 3 (+5,000 km) and Ship 5 (-5,000 km) share Grid 1 and must see each other (transitivity)"
    );
    assert!(
        !dyn_3.contains(&ship_2_id),
        "Ship 2 (+10,000 km) is across the +8,000 km grid wall in Grid 2 and must NOT be visible from Grid 1"
    );

    // 2. Verify SetState binary buffer for Ship 1 contains Ship 1, Ship 3, Ship 5 + 67 static balls, NOT Ship 2
    let (_, set_state_1, _) = jita.get_set_state(ship_1_id).await.expect("get_set_state 1");
    let PyRep::KeyVal(kv1) = &set_state_1 else {
        panic!("Expected KeyVal");
    };
    let PyRep::Bytes(bytes1) = &kv1["state"] else {
        panic!("Expected Bytes");
    };
    let (_, _, dec1) = decode_add_balls(bytes1).expect("decode SetState 1");
    assert!(dec1.iter().any(|b| b.id == ship_1_id));
    assert!(dec1.iter().any(|b| b.id == ship_3_id));
    assert!(dec1.iter().any(|b| b.id == ship_5_id));
    assert!(
        !dec1.iter().any(|b| b.id == ship_2_id),
        "SetState binary stream for Ship 1 must not include Ship 2 in Grid 2"
    );

    // Drain initial spawn events
    let _ = jita.drain_observer_updates(ship_1_id).await.unwrap();
    let _ = jita.drain_observer_updates(ship_2_id).await.unwrap();
    let _ = jita.drain_observer_updates(ship_3_id).await.unwrap();
    let _ = jita.drain_observer_updates(ship_5_id).await.unwrap();

    // 3. Ship 1 executes local GotoDirection -> Ship 3 (+5,000 km) receives it, Ship 2 (+10,000 km) does NOT
    jita.set_movement_mode(
        ship_1_id,
        MovementMode::GotoDirection {
            direction: DVec3::X,
        },
    )
    .await
    .expect("set_movement_mode");
    let (_, evs_2, _) = jita.drain_observer_updates(ship_2_id).await.unwrap();
    let (_, evs_3, _) = jita.drain_observer_updates(ship_3_id).await.unwrap();
    assert!(
        evs_2.is_empty(),
        "Ship 2 in Grid 2 must NOT receive Ship 1's local destiny updates"
    );
    assert!(
        evs_3
            .iter()
            .any(|e| matches!(e, DestinyEvent::GotoDirection { ball_id, .. } if *ball_id == ship_1_id)),
        "Ship 3 in same grid must receive Ship 1's local destiny update"
    );

    // 4. Place Ship 4 at 20,000 km and warp Ship 1 from (0,0,0) to (20,000 km, 0, 0):
    // Ship 4 receives AddBalls2 when Ship 1 enters its grid; Ship 3 receives RemoveBalls when Ship 1 leaves
    let ship_4_id = 100_500_004;
    let dest_20k = DVec3::new(20_000_000.0, 0.0, 0.0);
    jita.spawn_ball(make_ship(ship_4_id, dest_20k))
        .await
        .expect("spawn ship 4 at 20,000 km");
    let _ = jita.drain_observer_updates(ship_1_id).await.unwrap();
    let _ = jita.drain_observer_updates(ship_3_id).await.unwrap();
    let _ = jita.drain_observer_updates(ship_4_id).await.unwrap();

    jita.set_movement_mode(
        ship_1_id,
        MovementMode::new_warp(DVec3::ZERO, dest_20k, 3.0),
    )
    .await
    .expect("warp ship 1 to 20,000 km");

    let mut ship4_saw_addballs2 = false;
    let mut ship3_saw_removeballs = false;
    for _ in 0..30 {
        let broadcast = jita.step_manual(1.0).await.expect("step_manual");
        if let Some(evs) = broadcast.observer_events.get(&ship_4_id) {
            if evs.iter().any(|e| {
                matches!(e, DestinyEvent::AddBalls2 { balls } if balls.iter().any(|b| b.id == ship_1_id))
            }) {
                ship4_saw_addballs2 = true;
            }
        }
        if let Some(evs) = broadcast.observer_events.get(&ship_3_id) {
            if evs.iter().any(|e| {
                matches!(e, DestinyEvent::RemoveBalls { ball_ids } if ball_ids.contains(&ship_1_id))
            }) {
                ship3_saw_removeballs = true;
            }
        }
    }
    assert!(
        ship4_saw_addballs2,
        "Ship 4 must receive AddBalls2 when Ship 1 warps into its grid"
    );
    assert!(
        ship3_saw_removeballs,
        "Ship 3 must receive RemoveBalls when Ship 1 warps out of its grid"
    );
}

#[tokio::test]
async fn test_p7_on_slim_item_change_when_pilot_in_space_changes_corp() {
    // parity: destiny:OnSlimItemChange, rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:corpRegistry.AddCorporation
    eve_testkit::attest(&["destiny:OnSlimItemChange"]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // Pilot 1 (90_000_001) logs in, selects character, and undocks in Jita (30000142)
    let mut sess1 = SessionState::new(981);
    ctx.dispatch_rpc(
        &mut sess1,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login pilot 1");
    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID pilot 1");
    let undock1 = ctx
        .dispatch_rpc(&mut sess1, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock pilot 1");
    let PyRep::KeyVal(undock1_kv) = &undock1 else {
        panic!("Expected KeyVal from Undock pilot 1");
    };
    let ship1_id = undock1_kv["shipID"].as_i64().unwrap();

    // Pilot 2 (90_000_002) logs in, selects character, and undocks on the same Jita grid
    let mut sess2 = SessionState::new(982);
    ctx.dispatch_rpc(
        &mut sess2,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("other".into()),
            PyRep::String("other-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login pilot 2");
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID pilot 2");
    let undock2 = ctx
        .dispatch_rpc(&mut sess2, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock pilot 2");
    let PyRep::KeyVal(undock2_kv) = &undock2 else {
        panic!("Expected KeyVal from Undock pilot 2");
    };
    let ship2_id = undock2_kv["shipID"].as_i64().unwrap();
    assert_ne!(ship1_id, ship2_id);

    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    // Drain initial undock/spawn observer updates for both ships on grid
    let _ = jita_handle
        .drain_observer_updates(ship1_id)
        .await
        .expect("drain initial ship 1");
    let _ = jita_handle
        .drain_observer_updates(ship2_id)
        .await
        .expect("drain initial ship 2");

    // While undocked in space, Pilot 1 creates a new corporation via real V24 client RPC corpRegistry.AddCorporation
    let create_corp = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "AddCorporation",
            &[
                PyRep::String("Grid Observer Slim Corp".into()),
                PyRep::String("GOSC".into()),
                PyRep::Real(0.05),
            ],
            &kw,
            None,
        )
        .await
        .expect("corpRegistry.AddCorporation while in space");
    let PyRep::KeyVal(corp_kv) = &create_corp else {
        panic!("Expected KeyVal from AddCorporation");
    };
    let new_corp_id = corp_kv["corporationID"].as_i64().expect("corporationID");
    assert!(new_corp_id >= 98_000_001);

    // Observer Pilot 2 drains DoDestinyUpdate notifications and must receive OnSlimItemChange(ship1_id, foo.SlimItem)
    let (_tick, evs2, pyrep_updates2) = jita_handle
        .drain_observer_updates(ship2_id)
        .await
        .expect("drain observer updates for pilot 2 after pilot 1 corp change");
    assert!(
        evs2.iter()
            .any(|e| matches!(e, DestinyEvent::OnSlimItemChange { ball_id } if *ball_id == ship1_id)),
        "Observer ship on grid must receive DestinyEvent::OnSlimItemChange for ship1_id"
    );

    let marshaled = eve_proto::marshal::encode(&pyrep_updates2);
    let decoded = eve_proto::marshal::decode(&marshaled)
        .expect("DoDestinyUpdate OnSlimItemChange must roundtrip through marshal");
    assert_eq!(decoded, pyrep_updates2);

    let PyRep::List(entries) = &pyrep_updates2 else {
        panic!("Expected List of DoDestinyUpdate entries");
    };
    let slim_change_entry = entries
        .iter()
        .find_map(|entry| {
            let PyRep::Tuple(outer) = entry else {
                return None;
            };
            let PyRep::Tuple(action) = outer.get(1)? else {
                return None;
            };
            if action.first()?.as_str() == Some("OnSlimItemChange") {
                let PyRep::Tuple(args) = action.get(1)? else {
                    return None;
                };
                Some(args.as_slice())
            } else {
                None
            }
        })
        .expect("DoDestinyUpdate wire list must contain OnSlimItemChange action");
    assert_eq!(slim_change_entry.len(), 2);
    assert_eq!(slim_change_entry[0], PyRep::Integer(ship1_id));
    let slim_obj = &slim_change_entry[1];
    let PyRep::Object { type_name, args } = slim_obj else {
        panic!("Expected foo.SlimItem Object as second argument of OnSlimItemChange");
    };
    assert_eq!(type_name, "foo.SlimItem");
    assert!(matches!(args.as_ref(), PyRep::Dict(_)));
    assert_eq!(
        slim_obj.dict_get("itemID").and_then(PyRep::as_i64),
        Some(ship1_id)
    );
    assert_eq!(
        slim_obj.dict_get("charID").and_then(PyRep::as_i64),
        Some(90_000_001)
    );
    assert_eq!(
        slim_obj.dict_get("corpID").and_then(PyRep::as_i64),
        Some(new_corp_id)
    );
}

