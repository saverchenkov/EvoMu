//! Comprehensive integration tests for Planet Manager (`planetMgr`) covering all 35 RPC methods:
//! colony queries, network construction/updates, resource surveys, spherical-harmonic heatmaps,
//! colony simulation, commodity extraction/flushing, processing transfers, orbital launches,
//! GM diagnostics, and bound proxy accessors.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const JITA_PLANET_ID: i64 = 40_009_082;

const COMMAND_CENTER_UPGRADES_SKILL: TypeId = 2505;
const INTERPLANETARY_CONSOLIDATION_SKILL: TypeId = 2495;
const REMOTE_SENSING_SKILL: TypeId = 13279;
const PLANETOLOGY_SKILL: TypeId = 2406;
const ADV_PLANETOLOGY_SKILL: TypeId = 2403;

const GAS_COMMAND_CENTER_TYPE_ID: TypeId = 2534;
const GAS_EXTRACTOR_TYPE_ID: TypeId = 2848;
const GAS_BASIC_FACILITY_TYPE_ID: TypeId = 2492;
const GAS_LAUNCHPAD_TYPE_ID: TypeId = 2543;
const WATER_P1_TYPE_ID: TypeId = 2389;

async fn setup_pilot(ctx: &ServerContext, session_id: u64, username: &str, password: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    sess
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

async fn rpc_kw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, kwargs, None).await
}

fn seed_cargo_item(snap: &mut eve_db::WorldStateSnapshot, type_id: TypeId, owner_id: i64, location_id: i64, quantity: i32) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag: InventoryFlag::Cargo,
            quantity,
            singleton: false,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        },
    );
    item_id
}

fn grant_pi_skills(snap: &mut eve_db::WorldStateSnapshot, char_id: i64) {
    if let Some(ch) = snap.characters.get_mut(&char_id) {
        ch.balance = IskCents::from_isk(100_000_000);
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
    }
    let sk = snap.skills.entry(char_id).or_default();
    for (stid, lvl) in [
        (COMMAND_CENTER_UPGRADES_SKILL, 5),
        (INTERPLANETARY_CONSOLIDATION_SKILL, 4),
        (REMOTE_SENSING_SKILL, 4),
        (PLANETOLOGY_SKILL, 3),
        (ADV_PLANETOLOGY_SKILL, 2),
    ] {
        sk.insert(stid, TrainedSkillRecord { type_id: stid, level: lvl, skill_points: 256_000.0 });
    }
}

fn get_i64(rep: &PyRep, key: &str) -> Option<i64> {
    rep.dict_get(key).and_then(|v| v.as_i64())
}

fn get_f64(rep: &PyRep, key: &str) -> Option<f64> {
    rep.dict_get(key).and_then(|v| v.as_f64())
}

fn get_bool(rep: &PyRep, key: &str) -> Option<bool> {
    rep.dict_get(key).and_then(|v| v.as_bool())
}

fn get_str<'a>(rep: &'a PyRep, key: &str) -> Option<&'a str> {
    rep.dict_get(key).and_then(|v| v.as_str())
}

fn cmd_pin(id: i64, tid: TypeId, lat: f64, lon: f64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(id), PyRep::Integer(tid as i64), PyRep::Real(lat), PyRep::Real(lon)])
}

fn cmd_link(a: i64, b: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(a), PyRep::Integer(b), PyRep::Integer(0)])
}

fn cmd_upgrade_link(a: i64, b: i64, lvl: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(a), PyRep::Integer(b), PyRep::Integer(lvl)])
}

fn cmd_upgrade_cc(id: i64, lvl: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(id), PyRep::Integer(lvl)])
}

fn cmd_schematic(id: i64, sid: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(id), PyRep::Integer(sid)])
}

fn cmd_route(id: i64, path: &[i64], tid: TypeId, qty: i64) -> PyRep {
    PyRep::Tuple(vec![
        PyRep::Integer(6),
        PyRep::Integer(id),
        PyRep::List(path.iter().map(|&p| PyRep::Integer(p)).collect()),
        PyRep::Integer(tid as i64),
        PyRep::Integer(qty),
    ])
}

fn cmd_extractor_program(id: i64, tid: TypeId, radius: f64, heads: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(id), PyRep::Integer(tid as i64), PyRep::Real(radius), PyRep::Integer(heads)])
}

/// Test colony queries and network lifecycle:
/// UserUpdateNetwork, GetCommandCenterLevel, GetPin, GetExtractorsForPlanet,
/// GetCommandPinsForPlanet, GetColony, GetFullNetworkForOwner, UserAbandonPlanet.
#[tokio::test]
async fn test_planet_mgr_colony_queries_and_network_updates() {
    // parity: rpc:planetMgr.GetColony, rpc:planetMgr.GetCommandCenterLevel, rpc:planetMgr.GetCommandPinsForPlanet, rpc:planetMgr.GetExtractorsForPlanet, rpc:planetMgr.GetFullNetworkForOwner, rpc:planetMgr.GetPin, rpc:planetMgr.UserAbandonPlanet, rpc:planetMgr.UserUpdateNetwork
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 1101, "demo", "demo-secret", char_id).await;
    let ship_id = sess.ship_id.expect("Active ship");

    ctx.db.with_mut_snapshot(|snap| {
        grant_pi_skills(snap, char_id);
        seed_cargo_item(snap, GAS_COMMAND_CENTER_TYPE_ID, char_id, ship_id, 1);
    });

    let res_info = rpc(&ctx, &mut sess, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GetPlanetResourceInfo");
    let PyRep::List(res_list) = res_info.dict_get("resources").expect("resources list") else {
        panic!("Expected resources list");
    };
    let first_p0_tid = get_i64(&res_list[0], "resourceTypeID").expect("first P0 typeID") as TypeId;

    // 1. UserUpdateNetwork: build full colony network
    let update_commands = vec![
        cmd_pin(101, GAS_COMMAND_CENTER_TYPE_ID, 0.10, 0.10),
        cmd_upgrade_cc(101, 3),
        cmd_pin(102, GAS_EXTRACTOR_TYPE_ID, 0.11, 0.11),
        cmd_pin(103, GAS_BASIC_FACILITY_TYPE_ID, 0.12, 0.12),
        cmd_pin(104, GAS_LAUNCHPAD_TYPE_ID, 0.13, 0.13),
        cmd_link(101, 102),
        cmd_link(102, 103),
        cmd_link(103, 104),
        cmd_upgrade_link(102, 103, 1),
        cmd_schematic(103, 65),
        cmd_route(501, &[102, 103], first_p0_tid, 3_000),
        cmd_extractor_program(102, first_p0_tid, 0.02, 3),
    ];

    let net_rep = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(update_commands)])
        .await
        .expect("UserUpdateNetwork full colony setup");

    assert_eq!(get_i64(&net_rep, "commandCenterLevel"), Some(3));
    assert_eq!(get_i64(&net_rep, "pinsCount"), Some(4));
    assert_eq!(get_i64(&net_rep, "planetID"), Some(JITA_PLANET_ID));
    let PyRep::List(pins) = net_rep.dict_get("pins").expect("pins list") else { panic!("expected pins list"); };
    assert_eq!(pins.len(), 4);
    let PyRep::List(links) = net_rep.dict_get("links").expect("links list") else { panic!("expected links list"); };
    assert_eq!(links.len(), 3);

    let cc_pin = pins.iter().find(|p| get_i64(p, "typeID") == Some(GAS_COMMAND_CENTER_TYPE_ID as i64)).expect("cc pin");
    let cc_pin_id = get_i64(cc_pin, "pinID").expect("cc pin id");
    let ext_pin = pins.iter().find(|p| get_i64(p, "typeID") == Some(GAS_EXTRACTOR_TYPE_ID as i64)).expect("ext pin");
    let ext_pin_id = get_i64(ext_pin, "pinID").expect("ext pin id");
    let proc_pin = pins.iter().find(|p| get_i64(p, "typeID") == Some(GAS_BASIC_FACILITY_TYPE_ID as i64)).expect("proc pin");
    let proc_pin_id = get_i64(proc_pin, "pinID").expect("proc pin id");

    // 2. GetCommandCenterLevel
    let cc_lvl = rpc(&ctx, &mut sess, "planetMgr", "GetCommandCenterLevel", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GetCommandCenterLevel");
    assert_eq!(cc_lvl.as_i64(), Some(3));

    // 3. GetPin queries across pin types
    let pin_cc = rpc(&ctx, &mut sess, "planetMgr", "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(cc_pin_id)])
        .await
        .expect("GetPin CC");
    assert_eq!(get_i64(&pin_cc, "pinID"), Some(cc_pin_id));
    assert_eq!(get_i64(&pin_cc, "typeID"), Some(GAS_COMMAND_CENTER_TYPE_ID as i64));

    let pin_ext = rpc(&ctx, &mut sess, "planetMgr", "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ext_pin_id)])
        .await
        .expect("GetPin Extractor");
    assert_eq!(get_i64(&pin_ext, "pinID"), Some(ext_pin_id));
    assert_eq!(get_i64(&pin_ext, "typeID"), Some(GAS_EXTRACTOR_TYPE_ID as i64));
    assert_eq!(get_i64(&pin_ext, "headCount"), Some(3));
    assert_eq!(get_i64(&pin_ext, "resourceTypeID"), Some(first_p0_tid as i64));

    let pin_proc = rpc(&ctx, &mut sess, "planetMgr", "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(proc_pin_id)])
        .await
        .expect("GetPin Processor");
    assert_eq!(get_i64(&pin_proc, "schematicID"), Some(65));
    assert_eq!(get_i64(&pin_proc, "cycleTimeSeconds"), Some(1800));

    let missing_pin = rpc(&ctx, &mut sess, "planetMgr", "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(999_999)])
        .await
        .expect_err("PinNotFound");
    assert_eq!(missing_pin.code, "PinNotFound");

    // 4. GetExtractorsForPlanet
    let ext_rep = rpc(&ctx, &mut sess, "planetMgr", "GetExtractorsForPlanet", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GetExtractorsForPlanet");
    let PyRep::List(ext_pins) = ext_rep else { panic!("Expected list from GetExtractorsForPlanet"); };
    assert_eq!(ext_pins.len(), 1);
    assert_eq!(get_i64(&ext_pins[0], "pinID"), Some(ext_pin_id));
    assert_eq!(get_i64(&ext_pins[0], "typeID"), Some(GAS_EXTRACTOR_TYPE_ID as i64));
    assert_eq!(get_i64(&ext_pins[0], "headCount"), Some(3));

    // 5. GetCommandPinsForPlanet
    let cmd_pins_rep = rpc(&ctx, &mut sess, "planetMgr", "GetCommandPinsForPlanet", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GetCommandPinsForPlanet");
    assert_eq!(get_i64(&cmd_pins_rep, "planetID"), Some(JITA_PLANET_ID));
    assert_eq!(get_i64(&cmd_pins_rep, "ownerID"), Some(char_id));
    assert_eq!(get_i64(&cmd_pins_rep, "commandCenterLevel"), Some(3));
    assert_eq!(get_i64(&cmd_pins_rep, "pinsCount"), Some(4));

    // 6. GetColony
    let col_rep = rpc(&ctx, &mut sess, "planetMgr", "GetColony", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GetColony");
    assert_eq!(get_i64(&col_rep, "planetID"), Some(JITA_PLANET_ID));
    assert_eq!(get_i64(&col_rep, "ownerID"), Some(char_id));
    assert_eq!(get_i64(&col_rep, "solarSystemID"), Some(JITA_SOLAR_SYSTEM_ID as i64));
    assert_eq!(get_i64(&col_rep, "pinsCount"), Some(4));
    let PyRep::List(col_pins) = col_rep.dict_get("pins").expect("pins") else { panic!("expected pins"); };
    assert_eq!(col_pins.len(), 4);
    let PyRep::List(col_routes) = col_rep.dict_get("routes").expect("routes") else { panic!("expected routes"); };
    assert_eq!(col_routes.len(), 1);

    // 7. GetFullNetworkForOwner
    let full_net_rep = rpc(&ctx, &mut sess, "planetMgr", "GetFullNetworkForOwner", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GetFullNetworkForOwner");
    assert_eq!(get_i64(&full_net_rep, "planetID"), Some(JITA_PLANET_ID));
    assert_eq!(get_i64(&full_net_rep, "ownerID"), Some(char_id));
    assert_eq!(get_i64(&full_net_rep, "commandCenterLevel"), Some(3));
    let PyRep::List(full_routes) = full_net_rep.dict_get("routes").expect("routes") else { panic!("expected routes"); };
    assert_eq!(full_routes.len(), 1);

    // 8. UserAbandonPlanet: teardown and verify state cleared
    let abandon_rep = rpc(&ctx, &mut sess, "planetMgr", "UserAbandonPlanet", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("UserAbandonPlanet");
    assert_eq!(abandon_rep.as_bool(), Some(true));

    let has_colony = ctx.db.with_snapshot(|snap| snap.planetary_colonies.contains_key(&(char_id, JITA_PLANET_ID)));
    assert!(!has_colony);

    let err_after = rpc(&ctx, &mut sess, "planetMgr", "GetColony", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect_err("ColonyNotFound after abandon");
    assert_eq!(err_after.code, "ColonyNotFound");

    let cc_lvl_after = rpc(&ctx, &mut sess, "planetMgr", "GetCommandCenterLevel", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GetCommandCenterLevel after abandon");
    assert_eq!(cc_lvl_after.as_i64(), Some(0));
}

/// Test planetary resource surveying, spherical harmonics heatmap, extraction simulation, and planet accessors:
/// GetPlanetInfo, GetPlanetResourceInfo, GetResourceData, GetPlanetsForChar,
/// GetProgramResultInfo, GetLocalDistributionReport, id, planetID.
#[tokio::test]
async fn test_planet_mgr_resource_info_and_planet_queries() {
    // parity: rpc:planetMgr.GetLocalDistributionReport, rpc:planetMgr.GetPlanetInfo, rpc:planetMgr.GetPlanetResourceInfo, rpc:planetMgr.GetPlanetsForChar, rpc:planetMgr.GetProgramResultInfo, rpc:planetMgr.GetResourceData, rpc:planetMgr.id, rpc:planetMgr.planetID
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 1102, "demo", "demo-secret", char_id).await;

    // Undock character
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
        }
    });

    // Determine a valid resource on the planet
    let res_info_initial = rpc(&ctx, &mut sess, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GetPlanetResourceInfo");
    let PyRep::List(resources_initial) = res_info_initial.dict_get("resources").expect("resources list") else {
        panic!("expected resources list");
    };
    assert!(!resources_initial.is_empty());
    let valid_p0_tid = get_i64(&resources_initial[0], "resourceTypeID").expect("resourceTypeID");

    // 1. GetResourceData requires Remote Sensing (13279) >= 1
    let unlearned_err = rpc(&ctx, &mut sess, "planetMgr", "GetResourceData", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(valid_p0_tid)])
        .await
        .expect_err("PISkillRequiredToSurveyPlanetResources");
    assert_eq!(unlearned_err.code, "PISkillRequiredToSurveyPlanetResources");

    // Train skills for survey: Remote Sensing 4, Planetology 3, Advanced Planetology 2
    ctx.db.with_mut_snapshot(|snap| {
        grant_pi_skills(snap, char_id);
    });

    // 2. GetPlanetResourceInfo: band count = 2 + 3 + 2*2 = 9
    let res_info = rpc(&ctx, &mut sess, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GetPlanetResourceInfo");
    assert_eq!(get_i64(&res_info, "planetID"), Some(JITA_PLANET_ID));
    assert_eq!(get_i64(&res_info, "solarSystemID"), Some(JITA_SOLAR_SYSTEM_ID as i64));
    assert_eq!(get_i64(&res_info, "remoteSensingLevel"), Some(4));
    assert_eq!(get_i64(&res_info, "planetologyLevel"), Some(3));
    assert_eq!(get_i64(&res_info, "advancedPlanetologyLevel"), Some(2));
    assert_eq!(get_i64(&res_info, "numBands"), Some(9));

    // 3. GetResourceData: test tuple arguments, KeyVal arguments, and Dict arguments
    let data_tuple = rpc(&ctx, &mut sess, "planetMgr", "GetResourceData", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(valid_p0_tid)])
        .await
        .expect("GetResourceData tuple");
    assert_eq!(get_i64(&data_tuple, "planetID"), Some(JITA_PLANET_ID));
    assert_eq!(get_i64(&data_tuple, "resourceTypeID"), Some(valid_p0_tid));
    assert_eq!(get_i64(&data_tuple, "numBands"), Some(9));
    assert_eq!(get_bool(&data_tuple, "inRange"), Some(true));
    assert_eq!(get_i64(&data_tuple, "proximity"), Some(5));
    let PyRep::Bytes(sh_bytes) = data_tuple.dict_get("data").expect("spherical harmonics data") else { panic!("expected bytes"); };
    assert_eq!(sh_bytes.len(), 324); // 9 * 9 * 4

    let kv_arg = PyRep::keyval([("planetID", PyRep::Integer(JITA_PLANET_ID)), ("resourceTypeID", PyRep::Integer(valid_p0_tid))]);
    let data_kv = rpc(&ctx, &mut sess, "planetMgr", "GetResourceData", &[kv_arg]).await.expect("GetResourceData KeyVal");
    assert_eq!(get_i64(&data_kv, "numBands"), Some(9));

    let dict_arg = PyRep::Dict(vec![
        (PyRep::String("planetID".into()), PyRep::Integer(JITA_PLANET_ID)),
        (PyRep::String("resourceTypeID".into()), PyRep::Integer(valid_p0_tid)),
    ]);
    let data_dict = rpc(&ctx, &mut sess, "planetMgr", "GetResourceData", &[dict_arg]).await.expect("GetResourceData Dict");
    assert_eq!(get_i64(&data_dict, "numBands"), Some(9));

    let invalid_res = rpc(&ctx, &mut sess, "planetMgr", "GetResourceData", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(99999)])
        .await
        .expect_err("InvalidPlanetResource");
    assert_eq!(invalid_res.code, "InvalidPlanetResource");

    // 4. GetPlanetInfo before colony deployment
    let planet_info_before = rpc(&ctx, &mut sess, "planetMgr", "GetPlanetInfo", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GetPlanetInfo before");
    assert_eq!(get_i64(&planet_info_before, "planetID"), Some(JITA_PLANET_ID));
    assert_eq!(get_i64(&planet_info_before, "solarSystemID"), Some(JITA_SOLAR_SYSTEM_ID as i64));
    assert_eq!(get_bool(&planet_info_before, "hasColony"), Some(false));
    assert_eq!(get_i64(&planet_info_before, "commandCenterLevel"), Some(0));
    assert_eq!(get_i64(&planet_info_before, "pinsCount"), Some(0));
    let PyRep::List(res_types) = planet_info_before.dict_get("resourceTypes").expect("resourceTypes") else { panic!("expected list"); };
    assert!(!res_types.is_empty());

    // 5. Deploy a small colony (Command Center 201 + Extractor 202)
    let ship_id = sess.ship_id.expect("Ship ID");
    ctx.db.with_mut_snapshot(|snap| {
        seed_cargo_item(snap, GAS_COMMAND_CENTER_TYPE_ID, char_id, ship_id, 1);
    });

    let dep_rep = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![
                cmd_pin(201, GAS_COMMAND_CENTER_TYPE_ID, 0.1, 0.1),
                cmd_pin(202, GAS_EXTRACTOR_TYPE_ID, 0.2, 0.2),
                cmd_link(201, 202),
                cmd_extractor_program(202, valid_p0_tid as TypeId, 0.02, 4),
            ]),
        ],
    )
    .await
    .expect("Deploy 2-pin colony");

    let PyRep::List(dep_pins) = dep_rep.dict_get("pins").expect("pins list") else { panic!("expected pins"); };
    let ext_pin = dep_pins.iter().find(|p| get_i64(p, "typeID") == Some(GAS_EXTRACTOR_TYPE_ID as i64)).expect("ext pin");
    let ext_pin_id = get_i64(ext_pin, "pinID").expect("ext pin id");

    // Re-query GetPlanetInfo with colony active
    let planet_info_after = rpc(&ctx, &mut sess, "planetMgr", "GetPlanetInfo", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GetPlanetInfo after");
    assert_eq!(get_bool(&planet_info_after, "hasColony"), Some(true));
    assert_eq!(get_i64(&planet_info_after, "pinsCount"), Some(2));

    // 6. GetPlanetsForChar
    let char_planets = rpc(&ctx, &mut sess, "planetMgr", "GetPlanetsForChar", &[]).await.expect("GetPlanetsForChar");
    let PyRep::Rowset { columns, rows: planet_rows } = char_planets else {
        panic!("expected PyRep::Rowset from GetPlanetsForChar");
    };
    assert_eq!(columns, vec!["planetID", "solarSystemID", "commandCenterLevel", "pinsCount"]);
    assert!(!planet_rows.is_empty());
    let jita_col = planet_rows
        .iter()
        .find(|row| row.first().and_then(|v| v.as_i64()) == Some(JITA_PLANET_ID))
        .expect("Jita colony row in GetPlanetsForChar");
    assert_eq!(jita_col[0].as_i64(), Some(JITA_PLANET_ID));
    assert_eq!(jita_col[1].as_i64(), Some(JITA_SOLAR_SYSTEM_ID as i64));
    assert_eq!(jita_col[3].as_i64(), Some(2)); // 2 pins

    // 7. GetProgramResultInfo
    let prog_info = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "GetProgramResultInfo",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(ext_pin_id),
            PyRep::Integer(valid_p0_tid),
            PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(3), PyRep::Integer(4)]),
            PyRep::Real(0.02),
        ],
    )
    .await
    .expect("GetProgramResultInfo");
    assert_eq!(get_i64(&prog_info, "planetID"), Some(JITA_PLANET_ID));
    assert_eq!(get_i64(&prog_info, "pinID"), Some(ext_pin_id));
    assert!(get_i64(&prog_info, "cycleTimeSeconds").unwrap_or(0) >= 300);
    assert_eq!(get_i64(&prog_info, "headCount"), Some(4));
    let PyRep::List(yields) = prog_info.dict_get("cycleYields").expect("cycleYields") else { panic!("expected yields"); };
    assert_eq!(yields.len(), 10);
    assert!(yields[0].as_i64().unwrap_or(0) > 0);

    // 8. GetLocalDistributionReport
    let dist_rep = rpc(&ctx, &mut sess, "planetMgr", "GetLocalDistributionReport", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ext_pin_id)])
        .await
        .expect("GetLocalDistributionReport");
    assert_eq!(get_i64(&dist_rep, "planetID"), Some(JITA_PLANET_ID));
    assert_eq!(get_i64(&dist_rep, "pinID"), Some(ext_pin_id));
    let PyRep::List(dist_yields) = dist_rep.dict_get("cycleYields").expect("cycleYields") else { panic!("expected yields list"); };
    assert_eq!(dist_yields.len(), 10);

    // 9. id and planetID accessors
    let id_rep = rpc(&ctx, &mut sess, "planetMgr", "id", &[PyRep::Integer(JITA_PLANET_ID)]).await.expect("id");
    assert_eq!(id_rep.as_i64(), Some(JITA_PLANET_ID));

    let pid_rep = rpc(&ctx, &mut sess, "planetMgr", "planetID", &[PyRep::Integer(JITA_PLANET_ID)]).await.expect("planetID");
    assert_eq!(pid_rep.as_i64(), Some(JITA_PLANET_ID));
}

/// Test colony execution, extraction flush, commodity transfers, launch management, and lifecycle controls:
/// Flush, UserTransferCommodities, UserLaunchCommodities, GetMyLaunchesDetails, DeleteLaunch,
/// CancelInstallProgram, RevertChanges, StartTicking, colonies, celestialIndex.
#[tokio::test]
async fn test_planet_mgr_launches_commodities_and_lifecycle() {
    // parity: rpc:planetMgr.CancelInstallProgram, rpc:planetMgr.DeleteLaunch, rpc:planetMgr.Flush, rpc:planetMgr.GetMyLaunchesDetails, rpc:planetMgr.RevertChanges, rpc:planetMgr.StartTicking, rpc:planetMgr.UserLaunchCommodities, rpc:planetMgr.UserTransferCommodities, rpc:planetMgr.celestialIndex, rpc:planetMgr.colonies
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 1103, "demo", "demo-secret", char_id).await;
    let ship_id = sess.ship_id.expect("Active ship");

    ctx.db.with_mut_snapshot(|snap| {
        grant_pi_skills(snap, char_id);
        seed_cargo_item(snap, GAS_COMMAND_CENTER_TYPE_ID, char_id, ship_id, 1);
    });

    let res_info = rpc(&ctx, &mut sess, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GetPlanetResourceInfo");
    let PyRep::List(res_list) = res_info.dict_get("resources").expect("resources list") else {
        panic!("Expected resources list");
    };
    let first_p0_tid = get_i64(&res_list[0], "resourceTypeID").expect("first P0 typeID") as TypeId;

    // Deploy 4-pin colony with upgraded command center (providing sufficient CPU/PG)
    let col_dep = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![
                cmd_pin(301, GAS_COMMAND_CENTER_TYPE_ID, 0.1, 0.1),
                cmd_upgrade_cc(301, 3),
                cmd_pin(302, GAS_EXTRACTOR_TYPE_ID, 0.11, 0.11),
                cmd_pin(303, GAS_BASIC_FACILITY_TYPE_ID, 0.12, 0.12),
                cmd_pin(304, GAS_LAUNCHPAD_TYPE_ID, 0.13, 0.13),
                cmd_link(301, 302),
                cmd_link(302, 303),
                cmd_link(303, 304),
                cmd_extractor_program(302, first_p0_tid, 0.02, 3),
            ]),
        ],
    )
    .await
    .expect("Deploy colony");

    let PyRep::List(col_pins) = col_dep.dict_get("pins").expect("pins list") else { panic!("expected pins list"); };
    let cc_pin = col_pins.iter().find(|p| get_i64(p, "typeID") == Some(GAS_COMMAND_CENTER_TYPE_ID as i64)).expect("cc pin");
    let cc_pin_id = get_i64(cc_pin, "pinID").expect("cc pin id");
    let launch_pin = col_pins.iter().find(|p| get_i64(p, "typeID") == Some(GAS_LAUNCHPAD_TYPE_ID as i64)).expect("launch pin");
    let launch_pin_id = get_i64(launch_pin, "pinID").expect("launch pin id");

    // 1. Flush: dock at station and simulate colony extraction and deposit to hangar
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(JITA_STATION_ID);
        }
        if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, JITA_PLANET_ID)) {
            col.last_extracted_unix = 1;
        }
    });

    let flush_rep = rpc(&ctx, &mut sess, "planetMgr", "Flush", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("Flush colony");

    assert!(get_i64(&flush_rep, "producedQuantity").unwrap_or(0) > 0);
    let delivered_id = get_i64(&flush_rep, "deliveredItemID").expect("delivered item id");
    assert!(delivered_id > 0);
    assert!(get_f64(&flush_rep, "pocoExportTaxISK").is_some());

    // Verify item deposited in hangar
    let item_found = ctx.db.with_snapshot(|snap| {
        snap.items.get(&delivered_id).map(|it| {
            it.owner_id == char_id && it.location_id == JITA_STATION_ID && it.flag == InventoryFlag::Hangar && it.quantity > 0
        }).unwrap_or(false)
    });
    assert!(item_found);

    // Error path: Flush when character in different solar system
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = 30_000_001;
        }
    });
    let wrong_sys_err = rpc(&ctx, &mut sess, "planetMgr", "Flush", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect_err("ColonyNotInCurrentSystem");
    assert_eq!(wrong_sys_err.code, "ColonyNotInCurrentSystem");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
        }
    });

    // 2. UserTransferCommodities: P0 -> P1 factory processing
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, JITA_PLANET_ID)) {
            col.extracted_commodities.insert(first_p0_tid, 6_000);
        }
    });

    let xfer_rep = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserTransferCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(first_p0_tid as i64),
            PyRep::Integer(WATER_P1_TYPE_ID as i64),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("UserTransferCommodities P0 -> P1");

    assert_eq!(get_i64(&xfer_rep, "inputQuantityConsumed"), Some(40));
    assert_eq!(get_i64(&xfer_rep, "outputQuantityProduced"), Some(5));
    assert!(get_i64(&xfer_rep, "totalStoredOutput").unwrap_or(0) >= 5);

    // Expedited transfer between pins with cooldown enforcement
    let mut exp_kw = HashMap::new();
    exp_kw.insert("expedited".into(), PyRep::Bool(true));
    let exp_rep = rpc_kw(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserTransferCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(cc_pin_id),
            PyRep::Integer(launch_pin_id),
            PyRep::Integer(WATER_P1_TYPE_ID as i64),
            PyRep::Integer(10),
        ],
        &exp_kw,
    )
    .await
    .expect("Expedited transfer");

    assert_eq!(get_i64(&exp_rep, "sourcePinID"), Some(cc_pin_id));
    assert_eq!(get_i64(&exp_rep, "destPinID"), Some(launch_pin_id));
    assert_eq!(get_i64(&exp_rep, "quantity"), Some(10));
    assert!(get_i64(&exp_rep, "cooldownSeconds").unwrap_or(0) >= 300);

    let exp_cd_err = rpc_kw(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserTransferCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(cc_pin_id),
            PyRep::Integer(launch_pin_id),
            PyRep::Integer(WATER_P1_TYPE_ID as i64),
            PyRep::Integer(10),
        ],
        &exp_kw,
    )
    .await
    .expect_err("ExpeditedTransferOnCooldown");
    assert_eq!(exp_cd_err.code, "ExpeditedTransferOnCooldown");

    // 3. UserLaunchCommodities: launch from CommandCenter
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, JITA_PLANET_ID)) {
            col.extracted_commodities.insert(WATER_P1_TYPE_ID, 5_000);
        }
        // undock pilot so launch is in planetary space
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
    });

    let launch_rep = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(WATER_P1_TYPE_ID as i64),
            PyRep::Integer(300),
            PyRep::String("CommandCenterLaunch".into()),
        ],
    )
    .await
    .expect("UserLaunchCommodities");

    assert_eq!(get_i64(&launch_rep, "planetID"), Some(JITA_PLANET_ID));
    assert_eq!(get_i64(&launch_rep, "commodityTypeID"), Some(WATER_P1_TYPE_ID as i64));
    assert_eq!(get_i64(&launch_rep, "routedQuantity"), Some(300));
    assert_eq!(get_str(&launch_rep, "destination"), Some("CommandCenterLaunch"));

    // 4. GetMyLaunchesDetails
    let my_launches = rpc(&ctx, &mut sess, "planetMgr", "GetMyLaunchesDetails", &[])
        .await
        .expect("GetMyLaunchesDetails");
    let PyRep::List(launches) = my_launches else { panic!("expected launches list"); };
    assert!(!launches.is_empty());
    let active_launch = &launches[0];
    let launch_id = get_i64(active_launch, "launchID").expect("launchID");
    assert_eq!(get_i64(active_launch, "planetID"), Some(JITA_PLANET_ID));
    assert_eq!(get_i64(active_launch, "quantity"), Some(300));

    // 5. DeleteLaunch
    let del_rep = rpc(&ctx, &mut sess, "planetMgr", "DeleteLaunch", &[PyRep::Integer(launch_id)])
        .await
        .expect("DeleteLaunch");
    assert_eq!(del_rep.as_bool(), Some(true));

    let item_deleted = ctx.db.with_snapshot(|snap| !snap.items.contains_key(&launch_id));
    assert!(item_deleted);

    // 6. CancelInstallProgram, RevertChanges, StartTicking
    let cancel_rep = rpc(&ctx, &mut sess, "planetMgr", "CancelInstallProgram", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("CancelInstallProgram");
    assert_eq!(cancel_rep.as_bool(), Some(true));

    let revert_rep = rpc(&ctx, &mut sess, "planetMgr", "RevertChanges", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("RevertChanges");
    assert_eq!(revert_rep.as_bool(), Some(true));

    let tick_rep = rpc(&ctx, &mut sess, "planetMgr", "StartTicking", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("StartTicking");
    assert_eq!(tick_rep.as_bool(), Some(true));

    // 7. colonies and celestialIndex accessors
    let colonies_rep = rpc(&ctx, &mut sess, "planetMgr", "colonies", &[]).await.expect("colonies");
    let PyRep::Dict(col_entries) = colonies_rep else { panic!("expected colonies dict"); };
    assert!(!col_entries.is_empty());
    let found_char = col_entries.iter().any(|(k, _)| k.as_i64() == Some(char_id));
    assert!(found_char);

    let cel_rep = rpc(&ctx, &mut sess, "planetMgr", "celestialIndex", &[]).await.expect("celestialIndex");
    assert_eq!(cel_rep.as_i64(), Some(1));
}

/// Test Game Master administration tools, simulations, and proxy property accessors:
/// GMAddCommodity, GMConvertCommandCenter, GMForceInstallProgram, GMVerifySimulation,
/// GMGetLocalDistributionReport, GMGetSynchedServerState, GMRunDepletionSim, append, remoteHandler.
#[tokio::test]
async fn test_planet_mgr_gm_tools_and_properties() {
    // parity: rpc:planetMgr.GMAddCommodity, rpc:planetMgr.GMConvertCommandCenter, rpc:planetMgr.GMForceInstallProgram, rpc:planetMgr.GMGetLocalDistributionReport, rpc:planetMgr.GMGetSynchedServerState, rpc:planetMgr.GMRunDepletionSim, rpc:planetMgr.GMVerifySimulation, rpc:planetMgr.append, rpc:planetMgr.remoteHandler
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 1104, "demo", "demo-secret", char_id).await;
    let ship_id = sess.ship_id.expect("Active ship");

    ctx.db.with_mut_snapshot(|snap| {
        grant_pi_skills(snap, char_id);
        seed_cargo_item(snap, GAS_COMMAND_CENTER_TYPE_ID, char_id, ship_id, 1);
    });

    assert!(!sess.has_gm_role());

    // 1. Role gate: Regular player session rejected on GM methods
    let err1 = rpc(&ctx, &mut sess, "planetMgr", "GMAddCommodity", &[PyRep::Integer(JITA_PLANET_ID)]).await.unwrap_err();
    assert_eq!(err1.code, "InsufficientRole");
    let err2 = rpc(&ctx, &mut sess, "planetMgr", "GMConvertCommandCenter", &[PyRep::Integer(JITA_PLANET_ID)]).await.unwrap_err();
    assert_eq!(err2.code, "InsufficientRole");
    let err3 = rpc(&ctx, &mut sess, "planetMgr", "GMForceInstallProgram", &[PyRep::Integer(JITA_PLANET_ID)]).await.unwrap_err();
    assert_eq!(err3.code, "InsufficientRole");
    let err4 = rpc(&ctx, &mut sess, "planetMgr", "GMVerifySimulation", &[PyRep::Integer(JITA_PLANET_ID)]).await.unwrap_err();
    assert_eq!(err4.code, "InsufficientRole");

    // 2. Elevate session to GM role
    sess.role |= service_roles::ROLE_ADMIN;
    assert!(sess.has_gm_role());

    let gm_add = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "GMAddCommodity",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(WATER_P1_TYPE_ID as i64), PyRep::Integer(100)],
    )
    .await
    .expect("GMAddCommodity");
    assert_eq!(gm_add.as_bool(), Some(true));

    let gm_conv = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "GMConvertCommandCenter",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(GAS_COMMAND_CENTER_TYPE_ID as i64), PyRep::Integer(2535)],
    )
    .await
    .expect("GMConvertCommandCenter");
    assert_eq!(gm_conv.as_bool(), Some(true));

    let gm_force = rpc(&ctx, &mut sess, "planetMgr", "GMForceInstallProgram", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(401)])
        .await
        .expect("GMForceInstallProgram");
    assert_eq!(gm_force.as_bool(), Some(true));

    let gm_sim = rpc(&ctx, &mut sess, "planetMgr", "GMVerifySimulation", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GMVerifySimulation");
    assert_eq!(gm_sim.as_bool(), Some(true));

    // 3. Deploy colony for GM diagnostic and simulation reports
    rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![
                cmd_pin(401, GAS_COMMAND_CENTER_TYPE_ID, 0.1, 0.1),
                cmd_pin(402, GAS_EXTRACTOR_TYPE_ID, 0.2, 0.2),
                cmd_pin(403, GAS_BASIC_FACILITY_TYPE_ID, 0.3, 0.3),
            ]),
        ],
    )
    .await
    .expect("Deploy 3-pin colony for GM");

    // 4. GM diagnostic reports & depletion simulations
    let gm_dist = rpc(&ctx, &mut sess, "planetMgr", "GMGetLocalDistributionReport", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GMGetLocalDistributionReport");
    assert_eq!(get_i64(&gm_dist, "planetID"), Some(JITA_PLANET_ID));
    assert_eq!(get_i64(&gm_dist, "characterID"), Some(char_id));
    assert_eq!(get_i64(&gm_dist, "pinsCount"), Some(3));

    let gm_sync = rpc(&ctx, &mut sess, "planetMgr", "GMGetSynchedServerState", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GMGetSynchedServerState");
    assert!(get_i64(&gm_sync, "colonyCount").unwrap_or(0) >= 1);

    let gm_dep = rpc(&ctx, &mut sess, "planetMgr", "GMRunDepletionSim", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GMRunDepletionSim");
    assert!(get_i64(&gm_dep, "colonyCount").unwrap_or(0) >= 1);

    // 5. Proxy properties: append and remoteHandler
    let append_rep = rpc(&ctx, &mut sess, "planetMgr", "append", &[]).await.expect("append");
    assert_eq!(append_rep, PyRep::None);

    let remote_rep = rpc(&ctx, &mut sess, "planetMgr", "remoteHandler", &[]).await.expect("remoteHandler");
    assert_eq!(remote_rep, PyRep::None);
}
