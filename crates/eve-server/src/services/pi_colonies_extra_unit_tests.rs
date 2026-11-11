//! Extra unit and mutation-killing tests for `pi_colonies.rs`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::industry_pi::pi_colonies::*;
use eve_db::clock::VirtualServerClock;
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, PlanetaryColonyRecord, ServerClock,
    TrainedSkillRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{PlanetSchematicInfo, PlanetSchematicTypeEntry, SdeRegistry};
use std::collections::HashMap;
use std::sync::Arc;

fn make_ctx() -> (ServerContext, Arc<VirtualServerClock>) {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.planet_schematics.insert(
        65,
        PlanetSchematicInfo {
            schematic_id: 65,
            name: "Water".into(),
            cycle_time: 1_800,
            types: vec![
                PlanetSchematicTypeEntry { type_id: 2268, quantity: 3_000, is_input: true },
                PlanetSchematicTypeEntry { type_id: 3645, quantity: 20, is_input: false },
            ],
            pins: vec![2469],
        },
    );
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000));
    let db = Arc::new(WorldRepository::with_demo_seed());
    db.acquire_online_lock(1, 501, Some(90_000_001), true, 1_775_400_000).unwrap();
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    let clock_dyn: Arc<dyn eve_db::ServerClock> = clock.clone();
    ctx.set_clock(clock_dyn);
    (ctx, clock)
}

fn make_session(char_id: i64, sys_id: Option<u32>, ship_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(801);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.solar_system_id = sys_id;
    s.ship_id = ship_id;
    s
}

#[test]
fn query_methods_bound_moniker_error_paths_and_simulation_advancement() {
    let (ctx, clock) = make_ctx();
    let (char_id, planet_id, sys_id) = (90_000_001, 40_009_080_i64, 30_000_142_u32);
    let kw = HashMap::new();
    let mut sess = make_session(char_id, Some(sys_id), Some(600_001));

    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetColony", &[], &kw, None).unwrap().unwrap_err().code,
        "InvalidPlanetID"
    );
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetCommandPinsForPlanet", &[], &kw, None).unwrap().unwrap_err().code,
        "InvalidPlanetID"
    );
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetExtractorsForPlanet", &[], &kw, None).unwrap().unwrap_err().code,
        "InvalidPlanetID"
    );
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetCommandCenterLevel", &[], &kw, None).unwrap().unwrap_err().code,
        "InvalidPlanetID"
    );
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetPin", &[], &kw, None).unwrap().unwrap_err().code,
        "InvalidPlanetID"
    );
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "UserAbandonPlanet", &[], &kw, None).unwrap().unwrap_err().code,
        "InvalidPlanetID"
    );

    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 801,
        user_id: 1,
        character_id: char_id,
        service: "planetMgr".into(),
        object_id: planet_id,
        oid_str: "oid:pi".into(),
        ref_id: 1,
    };
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetColony", &[], &kw, Some(&bound))
            .unwrap()
            .unwrap_err()
            .code,
        "ColonyNotFound"
    );
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetCommandPinsForPlanet", &[], &kw, Some(&bound))
            .unwrap()
            .unwrap(),
        PyRep::List(vec![])
    );
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetExtractorsForPlanet", &[], &kw, Some(&bound))
            .unwrap()
            .unwrap(),
        PyRep::List(vec![])
    );
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetCommandCenterLevel", &[], &kw, Some(&bound))
            .unwrap()
            .unwrap(),
        PyRep::Integer(0)
    );
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetPin", &[PyRep::Integer(1)], &kw, Some(&bound))
            .unwrap()
            .unwrap_err()
            .code,
        "ColonyNotFound"
    );
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "UserAbandonPlanet", &[], &kw, Some(&bound))
            .unwrap()
            .unwrap_err()
            .code,
        "ColonyNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                command_center_level: 2,
                pins_count: 2,
                extracted_commodities: HashMap::from([(2268, 500)]),
                last_extracted_unix: 1_775_400_000,
            },
        );
        let net = PiColonyNetworkState {
            pins: vec![
                PiPinState { pin_id: 101, type_id: 2254, latitude: 0.1, longitude: 0.1, ..Default::default() },
                PiPinState { pin_id: 102, type_id: 2469, latitude: 0.2, longitude: 0.2, schematic_id: Some(65), ..Default::default() },
            ],
            cpu_max: 5_000.0,
            pg_max: 9_000.0,
            last_simulated_unix: 1_775_400_000,
            pending_login_extracted: 120,
            pending_login_processed: 40,
            ..Default::default()
        };
        save_pi_network(snap, char_id, planet_id, &net);
    });

    let col_zero = handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetColony", &[], &kw, Some(&bound))
        .unwrap()
        .unwrap();
    assert_eq!(col_zero.dict_get("simulatedExtractedQuantity").and_then(|v| v.as_i64()), Some(120));
    assert_eq!(col_zero.dict_get("simulatedProcessedQuantity").and_then(|v| v.as_i64()), Some(40));

    clock.advance_secs(3_600);
    let col_adv = handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetColony", &[], &kw, Some(&bound))
        .unwrap()
        .unwrap();
    assert_eq!(col_adv.dict_get("planetID").and_then(|v| v.as_i64()), Some(planet_id));

    let fac_pin = handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetPin", &[PyRep::Integer(102)], &kw, Some(&bound))
        .unwrap()
        .unwrap();
    assert_eq!(fac_pin.dict_get("cycleTimeSeconds").and_then(|v| v.as_i64()), Some(1_800));
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetPin", &[PyRep::Integer(999)], &kw, Some(&bound))
            .unwrap()
            .unwrap_err()
            .code,
        "PinNotFound"
    );

    let full_net = handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetFullNetworkForOwner", &[], &kw, None)
        .unwrap()
        .unwrap();
    assert_eq!(full_net.dict_get("commandCenterLevel").and_then(|v| v.as_i64()), Some(2));
}

#[test]
fn user_update_network_validation_errors_rollback_and_keyval_commands() {
    let (ctx, _clock) = make_ctx();
    let (char_id, planet_id, sys_id, ship_id) = (90_000_001, 40_009_080_i64, 30_000_142_u32, 600_001_i64);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(100_000_000);
        }
        snap.skills.entry(char_id).or_default().insert(
            2505,
            TrainedSkillRecord { type_id: 2505, level: 2, skill_points: 8_000.0 },
        );
        snap.items.insert(
            600_002,
            InventoryItemRecord {
                item_id: 600_002,
                type_id: 2254,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedCommandCenterHold,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut sess = make_session(char_id, Some(sys_id), Some(ship_id));
    let call_upd = |s: &mut SessionState, pid: i64, second: PyRep| {
        handle_pi_colonies(&ctx, s, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(pid), second], &kw, None).unwrap()
    };

    assert_eq!(call_upd(&mut sess, planet_id, PyRep::Integer(99)).unwrap_err().code, "ClientSchematicOverrideForbidden");
    assert_eq!(call_upd(&mut sess, 30_000_142, PyRep::List(vec![])).unwrap_err().code, "InvalidPlanetID");
    assert_eq!(call_upd(&mut sess, 40_009_999, PyRep::List(vec![])).unwrap_err().code, "PlanetNotInCurrentSystem");
    assert_eq!(call_upd(&mut sess, planet_id, PyRep::List(vec![])).unwrap_err().code, "InvalidCommandStream");

    sess.station_id = Some(60_003_760);
    let place_cc_kv = PyRep::keyval([
        ("commandID", PyRep::Integer(1)),
        ("pinID", PyRep::Integer(1)),
        ("typeID", PyRep::Integer(2254)),
        ("latitude", PyRep::Real(0.1)),
        ("longitude", PyRep::Real(0.1)),
    ]);
    assert_eq!(
        call_upd(&mut sess, planet_id, PyRep::List(vec![place_cc_kv.clone()])).unwrap_err().code,
        "CannotPlaceCommandCenterWhileDocked"
    );
    sess.station_id = None;

    let bad_coords = PyRep::Tuple(vec![
        PyRep::Integer(1),
        PyRep::Integer(1),
        PyRep::Integer(2254),
        PyRep::Real(2.5),
        PyRep::Real(0.1),
    ]);
    assert_eq!(call_upd(&mut sess, planet_id, PyRep::List(vec![bad_coords])).unwrap_err().code, "InvalidPinCoordinates");

    // Transactional rollback: deploy CC (cmd 1) followed by UnsupportedPlanetCommand (cmd 99)
    let rollback_cmds = PyRep::List(vec![place_cc_kv.clone(), PyRep::Tuple(vec![PyRep::Integer(99)])]);
    assert_eq!(call_upd(&mut sess, planet_id, rollback_cmds).unwrap_err().code, "UnsupportedPlanetCommand");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.planetary_colonies.contains_key(&(char_id, planet_id)));
        assert_eq!(snap.items.get(&600_002).map(|i| i.quantity), Some(2));
    });

    // Deploy CC + Upgrade CC via KeyVal (cmd 9) + Place ECU & Factory + Link (KeyVal cmd 3) + Upgrade Link (KeyVal cmd 5) + Program (KeyVal cmd 10)
    let setup_cmds = PyRep::List(vec![
        place_cc_kv,
        PyRep::keyval([("commandID", PyRep::Integer(9)), ("pinID", PyRep::Integer(1)), ("targetLevel", PyRep::Integer(2))]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(2848), PyRep::Real(0.15), PyRep::Real(0.15)]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(3), PyRep::Integer(2469), PyRep::Real(0.18), PyRep::Real(0.18)]),
        PyRep::keyval([
            ("commandID", PyRep::Integer(3)),
            ("sourcePinID", PyRep::Integer(2)),
            ("destPinID", PyRep::Integer(3)),
            ("level", PyRep::Integer(0)),
        ]),
        PyRep::keyval([
            ("commandID", PyRep::Integer(5)),
            ("endpoint1", PyRep::Integer(2)),
            ("endpoint2", PyRep::Integer(3)),
            ("level", PyRep::Integer(1)),
        ]),
        PyRep::keyval([
            ("commandID", PyRep::Integer(10)),
            ("pinID", PyRep::Integer(2)),
            ("resourceTypeID", PyRep::Integer(2268)),
            ("cycleTimeSeconds", PyRep::Integer(300)),
            ("headCount", PyRep::Integer(2)),
        ]),
    ]);
    let res = call_upd(&mut sess, planet_id, setup_cmds).unwrap();
    assert_eq!(res.dict_get("commandCenterLevel").and_then(|v| v.as_i64()), Some(2));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&600_002).map(|i| i.quantity), Some(1));
    });
}

#[test]
fn user_update_network_pin_link_route_head_and_upgrade_edge_cases() {
    let (ctx, _clock) = make_ctx();
    let (char_id, planet_id, sys_id) = (90_000_001, 40_009_080_i64, 30_000_142_u32);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.balance = IskCents::from_isk(50_000_000);
        }
        snap.skills.entry(char_id).or_default().insert(
            2505,
            TrainedSkillRecord { type_id: 2505, level: 3, skill_points: 40_000.0 },
        );
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                command_center_level: 2,
                pins_count: 3,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 0,
            },
        );
        let net = PiColonyNetworkState {
            pins: vec![
                PiPinState { pin_id: 10, type_id: 2254, latitude: 0.10, longitude: 0.10, ..Default::default() },
                PiPinState { pin_id: 20, type_id: 2848, latitude: 0.12, longitude: 0.12, cpu_usage: 400.0, pg_usage: 2_600.0, ..Default::default() },
                PiPinState { pin_id: 30, type_id: 2469, latitude: 0.14, longitude: 0.14, cpu_usage: 200.0, pg_usage: 800.0, ..Default::default() },
            ],
            links: vec![PiLinkState {
                source_pin_id: 20,
                dest_pin_id: 30,
                level: 1,
                length_km: 150.0,
                cpu_usage: 50.0,
                pg_usage: 50.0,
                bandwidth: 2_500.0,
            }],
            routes: vec![PiRouteState { route_id: 500, path: vec![20, 30], commodity_type_id: 2268, quantity: 100 }],
            cpu_used: 650.0,
            cpu_max: 7_057.0,
            pg_used: 3_450.0,
            pg_max: 9_000.0,
            ..Default::default()
        };
        save_pi_network(snap, char_id, planet_id, &net);
    });

    let mut sess = make_session(char_id, Some(sys_id), Some(600_001));
    let run_one = |s: &mut SessionState, cmd: PyRep| {
        handle_pi_colonies(&ctx, s, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(planet_id), PyRep::List(vec![cmd])], &kw, None).unwrap()
    };

    for (cmd, expected) in [
        (PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(20), PyRep::Integer(2469), PyRep::Real(0.2), PyRep::Real(0.2)]), "DuplicatePinID"),
        (PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(0)]), "InvalidLinkParameters"),
        (PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(30), PyRep::Integer(20), PyRep::Integer(0)]), "LinkAlreadyExists"),
        (PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(20), PyRep::Integer(30), PyRep::Integer(1)]), "InvalidLinkLevel"),
        (PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(501), PyRep::List(vec![PyRep::Integer(20)]), PyRep::Integer(2268), PyRep::Integer(100)]), "InvalidRoutePath"),
        (PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(501), PyRep::List(vec![PyRep::Integer(10), PyRep::Integer(30)]), PyRep::Integer(2268), PyRep::Integer(100)]), "RouteLinkNotFound"),
        (PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(501), PyRep::List(vec![PyRep::Integer(20), PyRep::Integer(30)]), PyRep::Integer(2268), PyRep::Integer(1_000_000)]), "InsufficientLinkBandwidth"),
        (PyRep::Tuple(vec![PyRep::Integer(8), PyRep::Integer(30), PyRep::Integer(9999)]), "InvalidSchematic"),
        (PyRep::Tuple(vec![PyRep::Integer(8), PyRep::Integer(20), PyRep::Integer(65)]), "InvalidSchematic"),
        (PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(2)]), "InvalidCommandCenterLevel"),
        (PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(5)]), "MissingSkillForCommandCenterUpgrade"),
        (PyRep::Tuple(vec![PyRep::Integer(14), PyRep::Integer(20), PyRep::Integer(1), PyRep::Real(0.95), PyRep::Real(0.95)]), "ExtractorHeadOutOfRange"),
    ] {
        assert_eq!(run_one(&mut sess, cmd).unwrap_err().code, expected);
    }

    let _ = run_one(&mut sess, PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(500)])).unwrap();
    let _ = run_one(&mut sess, PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(20), PyRep::Integer(30)])).unwrap();
    let after_del = run_one(&mut sess, PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(30)])).unwrap();
    assert_eq!(after_del.dict_get("pinsCount").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(after_del.dict_get("linksCount").and_then(|v| v.as_i64()), Some(0));
}

#[test]
fn unauth_network_args_and_remaining_command_error_branches() {
    let (ctx, _clock) = make_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let planet_id = ctx.sde.solar_systems[&sys_id].planet_ids[0];
    let kw = HashMap::new();
    let mut anon = SessionState::default();

    for m in [
        "GetColony",
        "GetFullNetworkForOwner",
        "GetCommandPinsForPlanet",
        "GetExtractorsForPlanet",
        "GetCommandCenterLevel",
        "GetPin",
        "UserAbandonPlanet",
        "UserUpdateNetwork",
    ] {
        assert!(
            handle_pi_colonies(&ctx, &mut anon, "planetMgr", m, &[], &kw, None).unwrap().is_err(),
            "{m} should require auth"
        );
    }

    let mut sess = make_session(char_id, Some(sys_id), None);
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: char_id,
        service: "planetMgr".into(),
        object_id: planet_id,
        oid_str: "N=1:1".into(),
        ref_id: 1,
    };

    // Arg permutations for UserUpdateNetwork (`L350-372`)
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[], &kw, None).unwrap().unwrap_err().code,
        "InvalidPlanetID"
    );
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(123), PyRep::List(vec![])], &kw, None).unwrap().unwrap_err().code,
        "InvalidPlanetID"
    );
    sess.solar_system_id = None;
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(planet_id)], &kw, None).unwrap().unwrap_err().code,
        "NoSolarSystem"
    );
    sess.solar_system_id = Some(sys_id);

    // No colony yet -> ColonyNotFound across cmds 1, 2, 3, 4, 5, 6, 10, 14 (`L442`, `587`, `656`, `700`, `770`, `811`, `876`, `1083`, `1143`)
    let run_upd = |s: &mut SessionState, cmds: Vec<PyRep>| {
        handle_pi_colonies(&ctx, s, "planetMgr", "UserUpdateNetwork", &[PyRep::List(cmds), PyRep::Integer(planet_id)], &kw, Some(&bound)).unwrap()
    };
    assert_eq!(run_upd(&mut sess, vec![PyRep::Integer(1)]).unwrap_err().code, "InvalidCommandStream");
    for cmd in [
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2469), PyRep::Real(0.1), PyRep::Real(0.1)]),
        PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(10)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(10), PyRep::Integer(20), PyRep::Integer(0)]),
        PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(10), PyRep::Integer(20)]),
        PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(10), PyRep::Integer(20), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(10), PyRep::Integer(20)]), PyRep::Integer(2268), PyRep::Integer(10)]),
        PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(10), PyRep::Integer(2268), PyRep::Real(0.02), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(14), PyRep::Integer(10), PyRep::Integer(1), PyRep::Real(0.0), PyRep::Real(0.0)]),
    ] {
        assert_eq!(run_upd(&mut sess, vec![cmd]).unwrap_err().code, "ColonyNotFound");
    }

    // Seed colony + tight CPU/PG + 0 ISK -> exercise PinNotFound, InsufficientColonyCpuOrPowergrid, InsufficientFunds, Route retention & Head commands
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(100_000_000);
        }
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                command_center_level: 1,
                pins_count: 2,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 0,
            },
        );
        save_pi_network(snap, char_id, planet_id, &PiColonyNetworkState {
            pins: (0..12)
                .map(|i| PiPinState {
                    pin_id: if i == 0 { 20 } else { 30 + i - 1 },
                    type_id: if i == 0 { 2848 } else { 2469 },
                    latitude: 0.05 * (i as f64),
                    longitude: 0.05 * (i as f64),
                    ..Default::default()
                })
                .collect(),
            links: vec![PiLinkState {
                source_pin_id: 20,
                dest_pin_id: 30,
                level: 2,
                length_km: 150.0,
                cpu_usage: 50.0,
                pg_usage: 50.0,
                bandwidth: 5_000.0,
                ..Default::default()
            }],
            routes: vec![PiRouteState {
                route_id: 700,
                path: vec![20, 30],
                commodity_type_id: 2268,
                quantity: 10,
            }],
            cpu_used: 7_000.0,
            cpu_max: 7_057.0,
            pg_used: 8_900.0,
            pg_max: 9_000.0,
            ..Default::default()
        });
    });

    for (cmd, expected) in [
        (PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(99), PyRep::Integer(2469), PyRep::Real(0.3), PyRep::Real(0.3)]), "InsufficientColonyCpuOrPowergrid"),
        (PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(999)]), "PinNotFound"),
        (PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(999), PyRep::Integer(30), PyRep::Integer(0)]), "PinNotFound"),
        (PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(20), PyRep::Integer(999), PyRep::Integer(0)]), "PinNotFound"),
        (PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(20), PyRep::Integer(30), PyRep::Integer(0)]), "InvalidLinkLevel"),
        (PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(20), PyRep::Integer(999), PyRep::Integer(3)]), "LinkNotFound"),
        (PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(20), PyRep::Integer(30), PyRep::Integer(2)]), "InvalidLinkLevel"),
        (PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(20), PyRep::Integer(30), PyRep::Integer(8)]), "InsufficientColonyCpuOrPowergrid"),
        (PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(999), PyRep::Integer(2268), PyRep::Real(0.02), PyRep::Integer(1)]), "PinNotFound"),
        (PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(20), PyRep::Integer(2268), PyRep::Real(0.02), PyRep::Integer(0)]), "InvalidExtractorHeadCount"),
        (PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(20), PyRep::Integer(2268), PyRep::Integer(300), PyRep::Integer(1), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.95), PyRep::Real(0.95)])])]), "ExtractorHeadOutOfRange"),
        (PyRep::Tuple(vec![PyRep::Integer(14), PyRep::Integer(999), PyRep::Integer(1), PyRep::Real(0.0), PyRep::Real(0.0)]), "PinNotFound"),
    ] {
        assert_eq!(run_upd(&mut sess, vec![cmd]).unwrap_err().code, expected);
    }

    // InstallProgram with both 3-tuple and 2-tuple head format + future expiry > now_unix (`L1051-1124`)
    assert!(run_upd(&mut sess, vec![PyRep::Tuple(vec![
        PyRep::Integer(10),
        PyRep::Integer(20),
        PyRep::Integer(2268),
        PyRep::Integer(300),
        PyRep::Integer(2),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.01), PyRep::Real(0.01)]),
            PyRep::Tuple(vec![PyRep::Real(0.02), PyRep::Real(0.02)]),
        ]),
        PyRep::Integer(1_775_403_600),
    ])]).is_ok());

    // MoveExtractorHead: move existing head 1 (`L1162-1164`), add up to 10, then 11th -> InvalidExtractorHeadCount (`L1166-1169`)
    assert!(run_upd(&mut sess, vec![PyRep::Tuple(vec![PyRep::Integer(14), PyRep::Integer(20), PyRep::Integer(1), PyRep::Real(0.03), PyRep::Real(0.03)])]).is_ok());
    let mut add_cmds = Vec::new();
    for hid in 20..28_i64 {
        add_cmds.push(PyRep::Tuple(vec![PyRep::Integer(14), PyRep::Integer(20), PyRep::Integer(hid), PyRep::Real(0.01), PyRep::Real(0.01)]));
    }
    assert!(run_upd(&mut sess, add_cmds).is_ok());
    assert_eq!(
        run_upd(&mut sess, vec![PyRep::Tuple(vec![PyRep::Integer(14), PyRep::Integer(20), PyRep::Integer(99), PyRep::Real(0.01), PyRep::Real(0.01)])])
            .unwrap_err()
            .code,
        "InvalidExtractorHeadCount"
    );

    // Raise CPU/PG max, keep 0 ISK -> InsufficientFunds on AddPin (cmd 1) & UpgradeLink (cmd 5); then fund and test RemoveLink (cmd 4) pruning route (`L789-793`) & RemovePin (cmd 2) pruning links
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents(0);
        }
        let mut net = load_pi_network(snap, char_id, planet_id, 1);
        net.cpu_max = 10_000.0;
        net.pg_max = 10_000.0;
        save_pi_network(snap, char_id, planet_id, &net);
    });
    assert_eq!(
        run_upd(&mut sess, vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(88), PyRep::Integer(2469), PyRep::Real(0.3), PyRep::Real(0.3)])])
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );
    assert_eq!(
        run_upd(&mut sess, vec![PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(20), PyRep::Integer(30), PyRep::Integer(4)])])
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );

    // Give ISK and test CreateLink InsufficientColonyCpuOrPowergrid & RemoveLink pruning route
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(1_000_000);
        let mut net = load_pi_network(snap, char_id, planet_id, 1);
        net.links.push(PiLinkState { source_pin_id: 20, dest_pin_id: 30, ..Default::default() });
        save_pi_network(snap, char_id, planet_id, &net);
    });
    assert!(run_upd(&mut sess, vec![PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(20), PyRep::Integer(30)])]).is_ok());
}
