//! Integration tests for tourneyMgr tournament arena setup, pairings, and teleportation.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn select_pilot(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    s
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn test_tourney_setup_pairings_and_teleportation() {
    // parity: rpc:tourneyMgr.GetPotentialMatches
    // parity: rpc:tourneyMgr.GetTourneySetup
    // parity: rpc:tourneyMgr.TeleportToArena
    // parity: rpc:tourneyMgr.TeleportToStation
    eve_testkit::attest(&[
        "rpc:tourneyMgr.GetPotentialMatches",
        "rpc:tourneyMgr.GetTourneySetup",
        "rpc:tourneyMgr.TeleportToArena",
        "rpc:tourneyMgr.TeleportToStation",
    ]);

    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 80_101, "demo", "demo-secret", P1).await;
    sess.role = eve_machonet::service_roles::ROLE_GMH;

    // 1. GetTourneySetup: empty until a tournament + arena system is registered in persisted state
    let empty = rpc(&ctx, &mut sess, "tourneyMgr", "GetTourneySetup", &[]).await.expect("GetTourneySetup empty");
    assert_eq!(empty, PyRep::Tuple(vec![PyRep::Dict(vec![]), PyRep::Dict(vec![]), PyRep::List(vec![])]), "no fixture tournaments");

    // Unknown (non-SDE) arena system is rejected
    let bad = eve_server::services::tourney_service::register_tourney_setup(&ctx, 1, "AT XXI", 39_999_999, Default::default());
    assert_eq!(bad.unwrap_err().code, "InvalidSolarSystem");
    // Register tournament 1 with arena system 30_000_142; the SDE name is resolved from ctx.sde.solar_systems
    let arena = eve_server::services::tourney_service::TourneyArenaSystem { combat_beacons: vec![9_001, 9_002], warp_bubbles: vec![9_101] };
    let sde_name = eve_server::services::tourney_service::register_tourney_setup(&ctx, 1, "AT XXI", 30_000_142, arena).expect("register");
    assert_eq!(sde_name, "Jita", "arena system name comes from the SDE");
    assert_eq!(ctx.sde.solar_systems[&30_000_142].name, sde_name);

    // Client unpacks `tournaments, tourneySystems, activeSystems`; tourneySystems values are (combatBeacons, warpBubbles)
    let setup_res = rpc(&ctx, &mut sess, "tourneyMgr", "GetTourneySetup", &[])
        .await
        .expect("GetTourneySetup");
    let PyRep::Tuple(parts) = setup_res else {
        panic!("GetTourneySetup must return 3-tuple, got {setup_res:?}");
    };
    assert_eq!(parts.len(), 3, "Setup tuple must have 3 elements");
    assert_eq!(parts[0], PyRep::Dict(vec![(PyRep::Integer(1), PyRep::String("AT XXI".into()))]));
    let ids = |v: &[i64]| PyRep::List(v.iter().map(|&x| PyRep::Integer(x)).collect());
    assert_eq!(
        parts[1],
        PyRep::Dict(vec![(PyRep::Integer(30_000_142), PyRep::Tuple(vec![ids(&[9_001, 9_002]), ids(&[9_101])]))])
    );
    assert_eq!(parts[2], PyRep::List(vec![]), "no system in use before a match exists");
    // Persisted in WorldStateSnapshot (tourney store)
    let persisted = ctx.db.with_snapshot(|s| eve_server::services::tourney_service::load_store(s).tournaments.get(&1).cloned());
    assert_eq!(persisted.as_deref(), Some("AT XXI"));

    // 2. GetPotentialMatches before creating match: returns List
    let matches_res = rpc(&ctx, &mut sess, "tourneyMgr", "GetPotentialMatches", &[PyRep::Integer(1)])
        .await
        .expect("GetPotentialMatches");
    let PyRep::List(match_list) = &matches_res else {
        panic!("GetPotentialMatches must return List, got {matches_res:?}");
    };
    let initial_count = match_list.len();

    // Create a match
    let match_id = 7001;
    rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(1), // tourneyID
            PyRep::Tuple(vec![
                PyRep::Integer(match_id),
                PyRep::Integer(98_000_001),
                PyRep::Integer(98_000_002),
                PyRep::Integer(P1),
                PyRep::Integer(P2),
            ]),
            PyRep::Integer(100),
            PyRep::Integer(600),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("CreateMatch");

    // 3. GetPotentialMatches after creating match: includes newly created match
    let matches_after = rpc(&ctx, &mut sess, "tourneyMgr", "GetPotentialMatches", &[PyRep::Integer(1)])
        .await
        .expect("GetPotentialMatches after create");
    let PyRep::List(match_list_after) = matches_after else {
        panic!("GetPotentialMatches must return List");
    };
    assert_eq!(match_list_after.len(), initial_count + 1);
    let found = match_list_after.iter().any(|m| match m {
        PyRep::Tuple(fields) => fields.first().and_then(|f| f.as_i64()) == Some(match_id),
        _ => false,
    });
    assert!(found, "Created match must be found in potential matches list");
    // The created (not completed) match marks its arena system as in use (`activeSystems`)
    let match_sys = ctx.db.with_snapshot(|s| eve_server::services::tourney_service::load_store(s).matches[&match_id].solar_system_id);
    let PyRep::Tuple(parts_after) = rpc(&ctx, &mut sess, "tourneyMgr", "GetTourneySetup", &[]).await.expect("setup after") else { panic!() };
    assert_eq!(parts_after[2], PyRep::List(vec![PyRep::Integer(match_sys)]));

    // 4. TeleportToArena: teleports referee/GM into arena space
    let bound_oid = 99_001;
    ctx.bound_objects.write().unwrap().insert(
        bound_oid,
        eve_server::router::BoundObjectEntry {
            bound_id: bound_oid,
            session_id: sess.session_id,
            user_id: sess.user_id.unwrap_or(0),
            character_id: P1,
            service: "beyonce".to_string(),
            object_id: 30_000_144,
            oid_str: format!("N=888444:{bound_oid}"),
            ref_id: 133_000_000_000_000_000,
        },
    );

    let p1_ship = ctx.db.with_snapshot(|s| s.characters.get(&P1).unwrap().active_ship_id);
    let old_sys = 30_000_144u32;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&P1) {
            ch.solar_system_id = old_sys;
            ch.station_id = None;
        }
    });
    sess.solar_system_id = Some(old_sys);
    sess.station_id = None;
    let old_sys_handle = ctx.systems.get_or_spawn(old_sys);
    old_sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: p1_ship,
            type_id: 606,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.5,
            mass: 1_200_000.0,
            radius: 80.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .unwrap();
    assert!(old_sys_handle.get_ball_snapshot(p1_ship).await.unwrap().is_some());

    let tele_res = rpc(&ctx, &mut sess, "tourneyMgr", "TeleportToArena", &[PyRep::Integer(match_id)])
        .await
        .expect("TeleportToArena");
    assert_eq!(tele_res, PyRep::Bool(true));
    assert!(sess.station_id.is_none(), "Station ID should be None in space");
    assert_eq!(sess.solar_system_id, Some(30_000_142));
    assert!(old_sys_handle.get_ball_snapshot(p1_ship).await.unwrap().is_none(), "Ship ball should be removed from old_sys");
    assert!(
        !ctx.bound_objects.read().unwrap().contains_key(&bound_oid),
        "stale beyonce bound object must be evicted on TeleportToArena"
    );
    let arena_notifs = ctx.drain_notifications_for_char(P1);
    assert!(
        arena_notifs.iter().any(|n| n.event == "OnMachoObjectDisconnect"),
        "OnMachoObjectDisconnect must be emitted on TeleportToArena"
    );
    let _rebind_arena = rpc(&ctx, &mut sess, "beyonce", "MachoBindObject", &[PyRep::Integer(30_000_142)])
        .await
        .expect("rebind beyonce in arena_sys");
    let arena_oid = ctx
        .bound_objects
        .read()
        .unwrap()
        .iter()
        .find(|(_, b)| b.character_id == P1 && b.service == "beyonce" && b.object_id == 30_000_142)
        .map(|(id, _)| *id)
        .expect("arena beyonce binding registered");

    // 5. TeleportToStation: returns referee/GM to home station and evicts arena beyonce binding
    let _ = ctx.drain_notifications_for_char(P1);
    let tele_home_res = rpc(&ctx, &mut sess, "tourneyMgr", "TeleportToStation", &[PyRep::Integer(match_id)])
        .await
        .expect("TeleportToStation");
    assert_eq!(tele_home_res, PyRep::Bool(true));
    assert!(sess.station_id.is_some(), "Character should be docked in station");
    let home_sys = sess.solar_system_id.unwrap() as i64;
    assert!(
        !ctx.bound_objects.read().unwrap().contains_key(&arena_oid),
        "arena beyonce bound object must be evicted on TeleportToStation"
    );
    let notifs = ctx.drain_notifications_for_char(P1);
    assert!(
        notifs.iter().any(|n| n.event == "OnMachoObjectDisconnect"),
        "OnMachoObjectDisconnect must be emitted on TeleportToStation"
    );
    let session_change = notifs.iter().find(|n| n.event == "OnSessionChanged").expect("OnSessionChanged emitted");
    assert_eq!(session_change.args.len(), 3, "OnSessionChanged must have 3 args");
    let sess_rep = &session_change.args[1];
    assert_eq!(sess_rep.dict_get("solarsystemid"), Some(&PyRep::None), "Docked session solarsystemid must be None");
    assert_eq!(sess_rep.dict_get("solarsystemid2"), Some(&PyRep::Integer(home_sys)), "Docked session solarsystemid2 must be home_sys");
}

#[tokio::test]
async fn test_slash_teleport_evicts_space_and_pi_bindings_and_blocks_cross_system_binds() {
    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 80_201, "demo", "demo-secret", P1).await;
    sess.role |= eve_machonet::service_roles::ROLE_GMH;

    // Undock into Jita space (30_000_142) via MachoBindObject / undock or slash teleport
    rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/tr 30000142".into())])
        .await
        .expect("/tr Jita");
    let sys_id = sess.solar_system_id.expect("in system") as i64;
    assert_eq!(sys_id, 30_000_142);

    // Non-system keeper (1) succeeds, while cross-system keeper & planetOrbitalRegistryBroker fail
    let bind_denied = ["BindAuthorization", "Denied"].concat();
    let keep_global = ctx.bind_object(&sess, "keeper", 1).expect("keeper global bind");
    assert_eq!(
        ctx.bind_object(&sess, "keeper", 30_000_144)
            .unwrap_err()
            .code,
        bind_denied
    );
    assert_eq!(
        ctx.bind_object(&sess, "planetOrbitalRegistryBroker", 30_000_144)
            .unwrap_err()
            .code,
        bind_denied
    );

    let bey_bind = ctx.bind_object(&sess, "beyonce", sys_id).expect("beyonce bind");
    let pi_bind = ctx
        .bind_object(&sess, "planetOrbitalRegistryBroker", sys_id)
        .expect("planetOrbitalRegistryBroker bind");
    let keep_sys_bind = ctx.bind_object(&sess, "keeper", sys_id).expect("keeper sys bind");
    let _ = ctx.drain_notifications_for_char(P1);

    // Slash teleport to another system (30_000_144) evicts all system-scoped bindings
    rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/tr 30000144".into())])
        .await
        .expect("/tr Lowsec");
    assert_eq!(sess.solar_system_id, Some(30_000_144));
    assert!(ctx.verify_bound_object(&sess, bey_bind).is_err());
    assert!(ctx.verify_bound_object(&sess, pi_bind).is_err());
    assert!(ctx.verify_bound_object(&sess, keep_sys_bind).is_err());
    assert!(ctx.verify_bound_object(&sess, keep_global).is_ok());

    let disconnects: Vec<_> = ctx
        .drain_notifications_for_char(P1)
        .into_iter()
        .filter(|n| n.event == "OnMachoObjectDisconnect")
        .collect();
    assert_eq!(
        disconnects.len(),
        3,
        "OnMachoObjectDisconnect must fire for beyonce, planetOrbitalRegistryBroker, and solar-system keeper"
    );
}
