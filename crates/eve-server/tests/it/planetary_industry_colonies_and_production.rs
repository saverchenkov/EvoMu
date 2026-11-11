//! Integration tests for Planetary Industry (PI) Colonies, Extraction, Factories & Storage:
//! - feat:pi.planet-info: planetMgr returns planet resource distribution maps per resource type; scan range limited by Remote Sensing.
//! - feat:pi.command-center: Place one command center per planet per character; upgrades raise CPU/PG (GetCommandCenterLevel).
//! - feat:pi.colony-limit: Interplanetary Consolidation limits number of colonies (1 + level).
//! - feat:pi.place-pins: Extractor control units, basic/advanced/high-tech factories, storage and launchpads placed within CPU/PG with ISK cost.
//! - feat:pi.links: Links between pins with length-based CPU/PG and upgradeable bandwidth.
//! - feat:pi.routes: Routes move commodities between pins within link bandwidth.
//! - feat:pi.extractor-program: Installing an ECU program with heads defines duration and per-cycle yield curve; CancelInstallProgram stops it.
//! - feat:pi.pi-depletion: Extraction depletes local resource density which regenerates over time.
//! - feat:pi.factory-cycles: Factories consume inputs per schematic and produce outputs each cycle when inputs are present.
//! - feat:pi.storage-overflow: Storage/launchpad capacity limits accumulate; excess output is lost per V24 rules.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::formulas::compute_pi_link_cost;
use eve_server::router::ServerContext;
use eve_server::services::industry_pi::{
    advance_colony_simulation, simulate_planet_depletion, PiColonyNetworkState, PiLinkState,
    PiPinState, PiRouteState,
};
use std::collections::HashMap;

const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const JITA_PLANET_ID: i64 = 40_009_082; // Gas planet
const SECOND_PLANET_ID: i64 = 40_009_098;
const THIRD_PLANET_ID: i64 = 40_009_077; // Barren planet

const COMMAND_CENTER_UPGRADES_SKILL: TypeId = 2505;
const INTERPLANETARY_CONSOLIDATION_SKILL: TypeId = 2495;
const REMOTE_SENSING_SKILL: TypeId = 13279;
const PLANETOLOGY_SKILL: TypeId = 2406;
const ADV_PLANETOLOGY_SKILL: TypeId = 2403;

const GAS_COMMAND_CENTER_TYPE: TypeId = 2534;
const BARREN_COMMAND_CENTER_TYPE: TypeId = 2524;
const EXTRACTOR_CONTROL_UNIT_TYPE: TypeId = 2848;
const BASIC_FACILITY_TYPE: TypeId = 2469;
const ADVANCED_FACILITY_TYPE: TypeId = 2480;
const HIGHTECH_FACILITY_TYPE: TypeId = 2482;
const STORAGE_FACILITY_TYPE: TypeId = 2541;
const LAUNCHPAD_TYPE: TypeId = 2544;

const AQUEOUS_LIQUIDS_P0_TYPE: TypeId = 2268;
const WATER_P1_TYPE: TypeId = 3645;

async fn setup_pilot(ctx: &ServerContext, session_id: u64, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
    sess.station_id = None;
    sess
}

fn seed_cargo_item(snap: &mut eve_db::WorldStateSnapshot, type_id: TypeId, owner_id: i64, location_id: i64, quantity: i32) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(item_id, InventoryItemRecord {
        item_id, type_id, owner_id, location_id, flag: InventoryFlag::Cargo, quantity, singleton: false, version: 1, ..Default::default()
    });
    item_id
}

fn grant_skills(snap: &mut eve_db::WorldStateSnapshot, char_id: i64, skills: &[(TypeId, i32)]) {
    let sk = snap.skills.entry(char_id).or_default();
    for &(stid, lvl) in skills {
        sk.insert(stid, TrainedSkillRecord { type_id: stid, level: lvl as u8, skill_points: 256_000.0 });
    }
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn cmd_pin_tup(tmp: i64, id: i64, tid: TypeId, lat: f64, lon: f64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Tuple(vec![PyRep::Integer(tmp), PyRep::Integer(id)]), PyRep::Integer(tid as i64), PyRep::Real(lat), PyRep::Real(lon)])
}
fn cmd_up_cc(pin: i64, lvl: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(pin), PyRep::Integer(lvl)])
}
fn cmd_up_cc_tup(tmp: i64, id: i64, lvl: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Tuple(vec![PyRep::Integer(tmp), PyRep::Integer(id)]), PyRep::Integer(lvl)])
}
fn cmd_link(a: i64, b: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(a), PyRep::Integer(b), PyRep::Integer(0)])
}
fn cmd_up_link(a: i64, b: i64, lvl: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(a), PyRep::Integer(b), PyRep::Integer(lvl)])
}
fn cmd_route(id: i64, a: i64, b: i64, tid: TypeId, qty: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(id), PyRep::List(vec![PyRep::Integer(a), PyRep::Integer(b)]), PyRep::Integer(tid as i64), PyRep::Integer(qty)])
}
fn cmd_prog(pin: i64, tid: TypeId, radius: f64, heads: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(pin), PyRep::Integer(tid as i64), PyRep::Real(radius), PyRep::Integer(heads)])
}

#[tokio::test]
async fn test_pi_planet_info_command_center_and_colony_limit() {
    // parity: feat:pi.planet-info
    // parity: feat:pi.command-center
    // parity: feat:pi.colony-limit
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 7001, char_id).await;
    let ship_id = sess.ship_id.expect("Active ship ID");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.balance = IskCents::from_isk(500_000_000); ch.station_id = None; }
        snap.skills.entry(char_id).or_default().clear();
    });

    // 1. feat:pi.planet-info: GetPlanetInfo & GetPlanetResourceInfo
    let pinfo = rpc(&ctx, &mut sess, "planetMgr", "GetPlanetInfo", &[PyRep::Integer(JITA_PLANET_ID)]).await.expect("GetPlanetInfo");
    let PyRep::KeyVal(pkv) = pinfo else { panic!("Expected KeyVal"); };
    assert_eq!(pkv.get("planetID"), Some(&PyRep::Integer(JITA_PLANET_ID)));
    assert_eq!(pkv.get("hasColony"), Some(&PyRep::Bool(false)));
    assert!(pkv.get("planetTypeID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(pkv.get("radius").and_then(|v| v.as_f64()).unwrap_or(0.0) > 1_000_000.0);

    let res_info = rpc(&ctx, &mut sess, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(JITA_PLANET_ID)]).await.expect("GetPlanetResourceInfo");
    let PyRep::KeyVal(res_kv) = res_info else { panic!("Expected KeyVal"); };
    let Some(PyRep::List(res_list)) = res_kv.get("resources") else { panic!("Expected resources List"); };
    assert!(!res_list.is_empty(), "Planet must have P0 resources");
    let first_p0_tid = match &res_list[0] {
        PyRep::KeyVal(kv) => kv.get("resourceTypeID").and_then(|v| v.as_i64()).unwrap(),
        _ => panic!("Expected KeyVal"),
    };

    // Survey fails without Remote Sensing
    let s_err = rpc(&ctx, &mut sess, "planetMgr", "GetResourceData", &[PyRep::keyval([("planetID", PyRep::Integer(JITA_PLANET_ID)), ("resourceTypeID", PyRep::Integer(first_p0_tid))])]).await.unwrap_err();
    assert_eq!(s_err.code, "PISkillRequiredToSurveyPlanetResources");

    // Survey succeeds with Remote Sensing
    ctx.db.with_mut_snapshot(|snap| grant_skills(snap, char_id, &[(REMOTE_SENSING_SKILL, 4), (PLANETOLOGY_SKILL, 3), (ADV_PLANETOLOGY_SKILL, 2)]));
    let res_data = rpc(&ctx, &mut sess, "planetMgr", "GetResourceData", &[PyRep::keyval([
        ("planetID", PyRep::Integer(JITA_PLANET_ID)), ("resourceTypeID", PyRep::Integer(first_p0_tid)),
        ("planetology", PyRep::Integer(3)), ("advancedPlanetology", PyRep::Integer(2)),
        ("remoteSensing", PyRep::Integer(4)), ("proximity", PyRep::Integer(5)), ("newBand", PyRep::Integer(9)),
    ])]).await.expect("GetResourceData with Remote Sensing");
    let PyRep::KeyVal(rd_kv) = res_data else { panic!("Expected KeyVal"); };
    assert_eq!(rd_kv.get("numBands"), Some(&PyRep::Integer(9)));
    assert!(matches!(rd_kv.get("data"), Some(PyRep::Bytes(b)) if !b.is_empty()));
    assert_eq!(rd_kv.get("proximity"), Some(&PyRep::Integer(5)));

    // 2. feat:pi.command-center: Deployment rules, errors, upgrade mechanics
    sess.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&char_id) { ch.station_id = Some(60_003_760); } });
    let docked_err = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![cmd_pin_tup(0, 1, GAS_COMMAND_CENTER_TYPE, 0.20, -0.80)])]).await.unwrap_err();
    assert_eq!(docked_err.code, "CannotPlaceCommandCenterWhileDocked");

    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&char_id) { ch.station_id = None; } });
    let no_item_err = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![cmd_pin_tup(0, 1, GAS_COMMAND_CENTER_TYPE, 0.20, -0.80)])]).await.unwrap_err();
    assert_eq!(no_item_err.code, "CommandCenterItemRequired");

    ctx.db.with_mut_snapshot(|snap| { seed_cargo_item(snap, BARREN_COMMAND_CENTER_TYPE, char_id, ship_id, 1); });
    let mismatch_err = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![cmd_pin_tup(0, 1, BARREN_COMMAND_CENTER_TYPE, 0.20, -0.80)])]).await.unwrap_err();
    assert_eq!(mismatch_err.code, "IncompatibleCommandCenterPlanetType");

    let cc_item_id = ctx.db.with_mut_snapshot(|snap| seed_cargo_item(snap, GAS_COMMAND_CENTER_TYPE, char_id, ship_id, 2));
    let place_cc = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![cmd_pin_tup(0, 1, GAS_COMMAND_CENTER_TYPE, 0.20, -0.80)])]).await.expect("Deploy CC");
    let PyRep::KeyVal(col_kv) = place_cc else { panic!("Expected KeyVal"); };
    assert_eq!(col_kv.get("currentLevel"), Some(&PyRep::Integer(0)));
    assert_eq!(col_kv.get("cpuCapacity"), Some(&PyRep::Integer(1675)));
    assert_eq!(col_kv.get("powerCapacity"), Some(&PyRep::Integer(6000)));
    assert_eq!(col_kv.get("pinsCount"), Some(&PyRep::Integer(1)));
    assert_eq!(ctx.db.get_item(cc_item_id).unwrap().quantity, 1);

    let Some(PyRep::List(cc_pins)) = col_kv.get("pins") else { panic!("Expected pins"); };
    let cc_pin_id = cc_pins[0].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();

    let dup_cc_err = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![cmd_pin_tup(0, 2, GAS_COMMAND_CENTER_TYPE, 0.25, -0.75)])]).await.unwrap_err();
    assert_eq!(dup_cc_err.code, "ColonyAlreadyExists");

    let up_err = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![cmd_up_cc(cc_pin_id, 1)])]).await.unwrap_err();
    assert_eq!(up_err.code, "MissingSkillForCommandCenterUpgrade");

    ctx.db.with_mut_snapshot(|snap| grant_skills(snap, char_id, &[(COMMAND_CENTER_UPGRADES_SKILL, 3)]));
    let bal_before = ctx.db.get_character(char_id).unwrap().balance;
    let up1 = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![cmd_up_cc(cc_pin_id, 1)])]).await.expect("Upgrade CC to 1");
    let PyRep::KeyVal(up1_kv) = up1 else { panic!("Expected KeyVal"); };
    assert_eq!(up1_kv.get("currentLevel"), Some(&PyRep::Integer(1)));
    assert_eq!(up1_kv.get("cpuCapacity"), Some(&PyRep::Integer(7057)));
    assert_eq!(up1_kv.get("powerCapacity"), Some(&PyRep::Integer(9000)));
    let bal_after1 = ctx.db.get_character(char_id).unwrap().balance;
    assert_eq!(bal_before.0 - bal_after1.0, 58_000_000);

    let up3 = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![cmd_up_cc(cc_pin_id, 3)])]).await.expect("Upgrade CC to 3");
    let PyRep::KeyVal(up3_kv) = up3 else { panic!("Expected KeyVal"); };
    assert_eq!(up3_kv.get("currentLevel"), Some(&PyRep::Integer(3)));
    assert_eq!(up3_kv.get("cpuCapacity"), Some(&PyRep::Integer(17215)));
    assert_eq!(up3_kv.get("powerCapacity"), Some(&PyRep::Integer(15000)));
    assert_eq!(rpc(&ctx, &mut sess, "planetMgr", "GetCommandCenterLevel", &[PyRep::Integer(JITA_PLANET_ID)]).await.expect("CCLevel"), PyRep::Integer(3));

    // 3. feat:pi.colony-limit: Interplanetary Consolidation limits colonies (1 + level)
    ctx.db.with_mut_snapshot(|snap| { seed_cargo_item(snap, GAS_COMMAND_CENTER_TYPE, char_id, ship_id, 2); });
    let col_lim_err = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(SECOND_PLANET_ID), PyRep::List(vec![cmd_pin_tup(0, 1, GAS_COMMAND_CENTER_TYPE, 0.10, 0.10)])]).await.unwrap_err();
    assert_eq!(col_lim_err.code, "MaxColoniesReached");

    ctx.db.with_mut_snapshot(|snap| grant_skills(snap, char_id, &[(INTERPLANETARY_CONSOLIDATION_SKILL, 1)]));
    let second_col = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(SECOND_PLANET_ID), PyRep::List(vec![cmd_pin_tup(0, 1, GAS_COMMAND_CENTER_TYPE, 0.10, 0.10)])]).await.expect("Deploy 2nd colony");
    let PyRep::KeyVal(sc_kv) = second_col else { panic!("Expected KeyVal"); };
    assert_eq!(sc_kv.get("pinsCount"), Some(&PyRep::Integer(1)));

    let third_err = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(THIRD_PLANET_ID), PyRep::List(vec![cmd_pin_tup(0, 1, GAS_COMMAND_CENTER_TYPE, 0.15, 0.15)])]).await.unwrap_err();
    assert_eq!(third_err.code, "MaxColoniesReached");
}

#[tokio::test]
async fn test_pi_pins_links_routes_and_extractor_program() {
    // parity: feat:pi.place-pins
    // parity: feat:pi.links
    // parity: feat:pi.routes
    // parity: feat:pi.extractor-program
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 7002, char_id).await;
    let ship_id = sess.ship_id.expect("Active ship ID");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.balance = IskCents::from_isk(500_000_000); ch.station_id = None; }
        grant_skills(snap, char_id, &[
            (COMMAND_CENTER_UPGRADES_SKILL, 5), (INTERPLANETARY_CONSOLIDATION_SKILL, 4),
            (REMOTE_SENSING_SKILL, 5), (PLANETOLOGY_SKILL, 5), (ADV_PLANETOLOGY_SKILL, 5),
        ]);
        seed_cargo_item(snap, GAS_COMMAND_CENTER_TYPE, char_id, ship_id, 1);
    });

    let col = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![
        cmd_pin_tup(0, 1, GAS_COMMAND_CENTER_TYPE, 0.35, -1.10),
        cmd_up_cc_tup(0, 1, 4),
    ])]).await.expect("Deploy and upgrade CC to level 4");
    let PyRep::KeyVal(col_kv) = col else { panic!("Expected KeyVal"); };
    let Some(PyRep::List(pins)) = col_kv.get("pins") else { panic!("Expected pins"); };
    let cc_pin_id = pins[0].dict_get("pinID").and_then(|v| v.as_i64()).expect("cc pinID");

    // 1. feat:pi.place-pins & 2. feat:pi.links: Place 6 pin categories + links
    let bal_before = ctx.db.get_character(char_id).unwrap().balance;
    let net_res = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![
        cmd_pin_tup(1, 2, EXTRACTOR_CONTROL_UNIT_TYPE, 0.36, -1.10),
        cmd_pin_tup(1, 3, BASIC_FACILITY_TYPE, 0.37, -1.09),
        cmd_pin_tup(1, 4, ADVANCED_FACILITY_TYPE, 0.38, -1.08),
        cmd_pin_tup(1, 5, HIGHTECH_FACILITY_TYPE, 0.39, -1.07),
        cmd_pin_tup(1, 6, STORAGE_FACILITY_TYPE, 0.40, -1.06),
        cmd_pin_tup(1, 7, LAUNCHPAD_TYPE, 0.41, -1.05),
        cmd_link(cc_pin_id, 2),
        cmd_link(2, 3),
        cmd_up_link(cc_pin_id, 2, 2),
    ])]).await.expect("Place 6 pin categories and links");

    let PyRep::KeyVal(net_kv) = net_res else { panic!("Expected KeyVal"); };
    assert_eq!(net_kv.get("pinsCount"), Some(&PyRep::Integer(7)));
    assert_eq!(net_kv.get("linksCount"), Some(&PyRep::Integer(2)));
    let bal_after = ctx.db.get_character(char_id).unwrap().balance;
    let isk_cost = net_kv.get("totalCostISK").and_then(|v| v.as_f64()).unwrap();
    assert!(isk_cost > 100_000.0, "Placing pins must cost ISK");
    assert_eq!(bal_before.0 - bal_after.0, IskCents::from_f64_isk(isk_cost).0);

    let Some(PyRep::List(pins_list)) = net_kv.get("pins") else { panic!("Expected pins list"); };
    for req_type in [EXTRACTOR_CONTROL_UNIT_TYPE as i64, BASIC_FACILITY_TYPE as i64, ADVANCED_FACILITY_TYPE as i64, HIGHTECH_FACILITY_TYPE as i64, STORAGE_FACILITY_TYPE as i64, LAUNCHPAD_TYPE as i64] {
        assert!(pins_list.iter().any(|p| p.dict_get("typeID").and_then(|v| v.as_i64()) == Some(req_type)), "Missing pin type {req_type}");
    }

    let Some(PyRep::List(links_list)) = net_kv.get("links") else { panic!("Expected links list"); };
    let upgraded_link = links_list.iter().find(|l| l.dict_get("level").and_then(|v| v.as_i64()) == Some(2)).expect("Level 2 link");
    let len_km = upgraded_link.dict_get("lengthKm").and_then(|v| v.as_f64()).unwrap();
    assert!(len_km > 0.0);
    assert_eq!(upgraded_link.dict_get("bandwidth").and_then(|v| v.as_f64()).unwrap(), 5000.0);
    let (exp_pg, exp_cpu) = compute_pi_link_cost(len_km, 2);
    let l_cpu = upgraded_link.dict_get("cpuUsage").and_then(|v| v.as_f64()).unwrap();
    let l_pg = upgraded_link.dict_get("powergridUsage").and_then(|v| v.as_f64()).unwrap();
    assert!((l_cpu - exp_cpu).abs() < 1e-6);
    assert!((l_pg - exp_pg).abs() < 1e-6);

    // CPU/PG budget overflow check
    let budget_err = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(
        (10..=15).map(|idx| cmd_pin_tup(1, idx, EXTRACTOR_CONTROL_UNIT_TYPE, 0.40 + (idx as f64) * 0.005, -1.05)).collect(),
    )]).await.unwrap_err();
    assert_eq!(budget_err.code, "InsufficientColonyCpuOrPowergrid");

    let ecu_pin_id = pins_list
        .iter()
        .find(|p| p.dict_get("typeID").and_then(|v| v.as_i64()) == Some(EXTRACTOR_CONTROL_UNIT_TYPE as i64))
        .and_then(|p| p.dict_get("pinID").and_then(|v| v.as_i64()))
        .expect("ecu pinID");
    let basic_pin_id = pins_list
        .iter()
        .find(|p| p.dict_get("typeID").and_then(|v| v.as_i64()) == Some(BASIC_FACILITY_TYPE as i64))
        .and_then(|p| p.dict_get("pinID").and_then(|v| v.as_i64()))
        .expect("basic pinID");

    // 3. feat:pi.routes: Route commodity along link between Pin 2 and Pin 3
    let _ = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![
        cmd_route(501, ecu_pin_id, basic_pin_id, AQUEOUS_LIQUIDS_P0_TYPE, 1_000),
    ])]).await.expect("Add route");

    let full_net = rpc(&ctx, &mut sess, "planetMgr", "GetFullNetworkForOwner", &[
        PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(char_id),
    ]).await.expect("GetFullNetworkForOwner");
    let PyRep::KeyVal(fn_kv) = full_net else { panic!("Expected KeyVal"); };
    let Some(PyRep::List(routes)) = fn_kv.get("routes") else { panic!("Expected routes"); };
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].dict_get("commodityTypeID").and_then(|v| v.as_i64()), Some(AQUEOUS_LIQUIDS_P0_TYPE as i64));
    assert_eq!(routes[0].dict_get("quantity").and_then(|v| v.as_i64()), Some(1_000));

    // 4. feat:pi.extractor-program: Install ECU program, inspect yield curve, CancelInstallProgram
    let prog_install = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![
        cmd_prog(ecu_pin_id, AQUEOUS_LIQUIDS_P0_TYPE, 0.02, 3),
    ])]).await.expect("Install ECU program");
    let PyRep::KeyVal(pi_kv) = prog_install else { panic!("Expected KeyVal"); };
    assert_eq!(pi_kv.get("pinsCount"), Some(&PyRep::Integer(7)));

    let prog_info = rpc(&ctx, &mut sess, "planetMgr", "GetProgramResultInfo", &[
        PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ecu_pin_id), PyRep::Integer(AQUEOUS_LIQUIDS_P0_TYPE as i64),
        PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(3)]), PyRep::Real(0.02),
    ]).await.expect("GetProgramResultInfo");
    let PyRep::Tuple(prog_tup) = prog_info else { panic!("Expected Tuple"); };
    assert_eq!(prog_tup.len(), 3);
    assert!(prog_tup[0].as_i64().unwrap_or(0) > 0);
    assert!(prog_tup[1].as_i64().unwrap_or(0) >= 300 * 10_000_000);
    assert_eq!(prog_tup[2].as_i64(), Some(10));

    let pin_before = rpc(&ctx, &mut sess, "planetMgr", "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ecu_pin_id)]).await.expect("GetPin 2");
    assert_eq!(pin_before.dict_get("headCount").and_then(|v| v.as_i64()), Some(3));

    let cancel_res = rpc(&ctx, &mut sess, "planetMgr", "CancelInstallProgram", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ecu_pin_id)]).await.expect("CancelInstallProgram");
    assert_eq!(cancel_res, PyRep::Bool(true));

    let pin_after = rpc(&ctx, &mut sess, "planetMgr", "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ecu_pin_id)]).await.expect("GetPin 2 after cancel");
    assert_eq!(pin_after.dict_get("headCount").and_then(|v| v.as_i64()), Some(0));
}

#[tokio::test]
async fn test_pi_depletion_factory_cycles_and_storage_overflow() {
    // parity: feat:pi.pi-depletion
    // parity: feat:pi.factory-cycles
    // parity: feat:pi.storage-overflow
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 7003, char_id).await;
    let ship_id = sess.ship_id.expect("Active ship ID");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.balance = IskCents::from_isk(500_000_000); ch.station_id = None; }
        grant_skills(snap, char_id, &[
            (COMMAND_CENTER_UPGRADES_SKILL, 5), (INTERPLANETARY_CONSOLIDATION_SKILL, 4),
            (REMOTE_SENSING_SKILL, 5), (PLANETOLOGY_SKILL, 5), (ADV_PLANETOLOGY_SKILL, 5),
        ]);
        seed_cargo_item(snap, GAS_COMMAND_CENTER_TYPE, char_id, ship_id, 1);
    });

    let _ = rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![
        cmd_pin_tup(0, 1, GAS_COMMAND_CENTER_TYPE, 0.10, 0.10),
        cmd_up_cc_tup(0, 1, 3),
        cmd_pin_tup(1, 102, EXTRACTOR_CONTROL_UNIT_TYPE, 0.11, 0.11),
        cmd_link(1, 102),
        cmd_prog(102, AQUEOUS_LIQUIDS_P0_TYPE, 0.02, 3),
    ])]).await.expect("Deploy network");

    // 1. feat:pi.pi-depletion: Depletion calculation & Flush depletion effect
    let (base_rate, curve) = ctx.db.with_mut_snapshot(|snap| {
        let net = PiColonyNetworkState {
            pins: vec![PiPinState {
                pin_id: 102, type_id: EXTRACTOR_CONTROL_UNIT_TYPE, extractor_head_count: 3,
                extractor_resource_type_id: Some(AQUEOUS_LIQUIDS_P0_TYPE), extractor_head_radius: 0.02, ..Default::default()
            }],
            ..Default::default()
        };
        simulate_planet_depletion(snap, JITA_PLANET_ID, char_id, &net, 24)
    });
    assert!(base_rate > 0.0, "Base depletion rate must be positive");
    assert_eq!(curve.len(), 24);
    assert!(curve[0] > curve[23], "Yield factor must deplete over time");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, JITA_PLANET_ID)) {
            col.last_extracted_unix = 1;
        }
    });

    let flush_res = rpc(&ctx, &mut sess, "planetMgr", "Flush", &[PyRep::Integer(JITA_PLANET_ID)]).await.expect("Flush extraction");
    assert!(flush_res.dict_get("producedQuantity").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    let res_info = rpc(&ctx, &mut sess, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(JITA_PLANET_ID)]).await.expect("GetPlanetResourceInfo after Flush");
    let Some(PyRep::List(res_list)) = res_info.dict_get("resources") else { panic!("Expected resources"); };
    let depleted_qual = res_list.iter().find_map(|r| {
        if r.dict_get("resourceTypeID").and_then(|v| v.as_i64()) == Some(AQUEOUS_LIQUIDS_P0_TYPE as i64) {
            r.dict_get("quality").and_then(|v| v.as_f64())
        } else {
            None
        }
    }).unwrap();
    assert!(depleted_qual < 0.85, "Harvested resource quality must be depleted");

    // 2. feat:pi.factory-cycles: Factory consumes inputs and produces output per schematic
    let water_schem = ctx.sde.planet_schematics.iter().find(|(_, s)| s.types.iter().any(|t| !t.is_input && t.type_id == WATER_P1_TYPE)).map(|(&id, _)| id).expect("Water P1 schematic");

    let mut fac_net = PiColonyNetworkState {
        pins: vec![
            PiPinState { pin_id: 201, type_id: STORAGE_FACILITY_TYPE, contents: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 3_000)]), ..Default::default() },
            PiPinState { pin_id: 202, type_id: BASIC_FACILITY_TYPE, schematic_id: Some(water_schem), ..Default::default() },
            PiPinState { pin_id: 203, type_id: STORAGE_FACILITY_TYPE, contents: HashMap::new(), ..Default::default() },
        ],
        links: vec![
            PiLinkState { source_pin_id: 201, dest_pin_id: 202, level: 1, ..Default::default() },
            PiLinkState { source_pin_id: 202, dest_pin_id: 203, level: 1, ..Default::default() },
        ],
        routes: vec![
            PiRouteState { route_id: 1, path: vec![201, 202], commodity_type_id: AQUEOUS_LIQUIDS_P0_TYPE, quantity: 3_000 },
            PiRouteState { route_id: 2, path: vec![202, 203], commodity_type_id: WATER_P1_TYPE, quantity: 20 },
        ],
        ..Default::default()
    };
    let mut fac_col = eve_db::PlanetaryColonyRecord {
        planet_id: JITA_PLANET_ID, owner_id: char_id, solar_system_id: JITA_SOLAR_SYSTEM_ID, command_center_level: 3, pins_count: 3, last_extracted_unix: 0,
        extracted_commodities: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 3_000)]),
    };

    let (_, proc_qty) = advance_colony_simulation(&ctx, &mut fac_col, &mut fac_net, 1800, 1800);
    assert_eq!(proc_qty, 20, "1 cycle of factory produces 20 units of Water P1");
    assert_eq!(fac_net.pins[0].contents.get(&AQUEOUS_LIQUIDS_P0_TYPE).copied().unwrap_or(0), 0, "Source inputs consumed");
    assert_eq!(fac_net.pins[2].contents.get(&WATER_P1_TYPE).copied(), Some(20), "Produced output deposited in destination pin");

    let (_, proc_empty) = advance_colony_simulation(&ctx, &mut fac_col, &mut fac_net, 1800, 3600);
    assert_eq!(proc_empty, 0, "No production when inputs are absent");

    // 3. feat:pi.storage-overflow: Capacity clamp and overflow protection per V24
    let mut cap_net = PiColonyNetworkState {
        pins: vec![
            PiPinState {
                pin_id: 301, type_id: EXTRACTOR_CONTROL_UNIT_TYPE, extractor_resource_type_id: Some(AQUEOUS_LIQUIDS_P0_TYPE),
                extractor_head_count: 2, extractor_head_radius: 0.02, extractor_cycle_seconds: 900, ..Default::default()
            },
            PiPinState { pin_id: 302, type_id: STORAGE_FACILITY_TYPE, contents: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 1_199_999)]), ..Default::default() },
        ],
        links: vec![PiLinkState { source_pin_id: 301, dest_pin_id: 302, level: 1, ..Default::default() }],
        routes: vec![PiRouteState { route_id: 1, path: vec![301, 302], commodity_type_id: AQUEOUS_LIQUIDS_P0_TYPE, quantity: 1_000 }],
        ..Default::default()
    };
    let mut cap_col = eve_db::PlanetaryColonyRecord {
        planet_id: JITA_PLANET_ID, owner_id: char_id, solar_system_id: JITA_SOLAR_SYSTEM_ID, command_center_level: 3, pins_count: 2, last_extracted_unix: 0,
        extracted_commodities: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 1_199_999)]),
    };

    let (ext_added, _) = advance_colony_simulation(&ctx, &mut cap_col, &mut cap_net, 900, 1800);
    assert_eq!(ext_added, 1, "Deposition clamped to remaining capacity (1 unit)");
    let stored_units = cap_net.pins[1].contents.get(&AQUEOUS_LIQUIDS_P0_TYPE).copied().unwrap_or(0);
    assert_eq!(stored_units, 1_200_000);
    let stored_vol = (stored_units as f64) * 0.01;
    assert!(stored_vol <= 12_000.0 + 1e-4, "Stored volume never exceeds 12,000 m3");

    let mut full_fac_net = PiColonyNetworkState {
        pins: vec![
            PiPinState { pin_id: 401, type_id: STORAGE_FACILITY_TYPE, contents: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 3_000)]), ..Default::default() },
            PiPinState { pin_id: 402, type_id: BASIC_FACILITY_TYPE, schematic_id: Some(water_schem), ..Default::default() },
            PiPinState { pin_id: 403, type_id: STORAGE_FACILITY_TYPE, contents: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 1_200_000)]), ..Default::default() },
        ],
        links: vec![
            PiLinkState { source_pin_id: 401, dest_pin_id: 402, level: 1, ..Default::default() },
            PiLinkState { source_pin_id: 402, dest_pin_id: 403, level: 1, ..Default::default() },
        ],
        routes: vec![
            PiRouteState { route_id: 1, path: vec![401, 402], commodity_type_id: AQUEOUS_LIQUIDS_P0_TYPE, quantity: 3_000 },
            PiRouteState { route_id: 2, path: vec![402, 403], commodity_type_id: WATER_P1_TYPE, quantity: 20 },
        ],
        ..Default::default()
    };
    let mut full_fac_col = eve_db::PlanetaryColonyRecord {
        planet_id: JITA_PLANET_ID, owner_id: char_id, solar_system_id: JITA_SOLAR_SYSTEM_ID, command_center_level: 3, pins_count: 3, last_extracted_unix: 0,
        extracted_commodities: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 1_203_000)]),
    };

    let (_, full_proc) = advance_colony_simulation(&ctx, &mut full_fac_col, &mut full_fac_net, 1800, 3600);
    assert_eq!(full_proc, 0, "Factory halts when destination is completely full");
    assert_eq!(full_fac_net.pins[0].contents.get(&AQUEOUS_LIQUIDS_P0_TYPE).copied(), Some(3_000), "Inputs preserved when destination storage is full");
}

#[allow(dead_code, unused_imports)]
pub(crate) mod planetary_customs_and_evermarks {
    use super::*;

//  Remediation integration tests for PI SDE schematic recipes & facility tier validation
//  (`feat:pi.factory-cycles`, `feat:pi.schematic-input-output-routing`).


use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId};
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const JITA_PLANET_ID: i64 = 40_009_082;
const PARAGON_CORP_ID: i64 = 1_000_419;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
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
    sess
}

fn seed_item(
    snap: &mut eve_db::WorldStateSnapshot,
    type_id: TypeId,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
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

#[tokio::test]
async fn test_pi_sde_schematics_facility_tier_validation_and_routing() {
    // parity: feat:pi.factory-cycles, feat:pi.schematic-input-output-routing
    eve_testkit::attest(&[
        "feat:pi.factory-cycles",
        "feat:pi.schematic-input-output-routing",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    assert!(
        ctx.sde.planet_schematics.len() > 60,
        "SDE must load full planet_schematics table"
    );

    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 731, "demo", "demo-secret", char_id).await;
    sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
    sess.station_id = None;
    sess.structure_id = None;
    let ship_id = sess.ship_id.expect("Active ship");

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
        ch.balance = IskCents::from_isk(100_000_000);
        seed_item(snap, 2534, char_id, ship_id, InventoryFlag::Cargo, 1);
        snap.skills.entry(char_id).or_default().insert(
            2505,
            TrainedSkillRecord {
                type_id: 2505,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    let find_schem = |out_tid: TypeId| -> i64 {
        ctx.sde
            .planet_schematics
            .iter()
            .find(|(_, s)| s.types.iter().any(|t| !t.is_input && t.type_id == out_tid))
            .map(|(&id, _)| i64::from(id))
            .unwrap_or_else(|| panic!("missing SDE schematic for output {out_tid}"))
    };
    let water_schem = find_schem(3645);
    let coolant_schem = find_schem(9832); // 9832 = Coolant (Water + Electrolytes)
    let nanofactory_schem = find_schem(2875);

    // Deploy Command Center + Basic (2492) + Advanced (2494) + High-Tech (2482) + Storage (2541) + Launchpad (2543)
    let setup = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(101), PyRep::Integer(2534), PyRep::Real(0.10), PyRep::Real(0.10)]),
                    PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(101), PyRep::Integer(5)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(2492), PyRep::Real(0.11), PyRep::Real(0.11)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(2494), PyRep::Real(0.12), PyRep::Real(0.12)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(104), PyRep::Integer(2482), PyRep::Real(0.13), PyRep::Real(0.13)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(105), PyRep::Integer(2541), PyRep::Real(0.14), PyRep::Real(0.14)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(106), PyRep::Integer(2543), PyRep::Real(0.15), PyRep::Real(0.15)]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("Deploy PI facilities");
    let PyRep::List(pins) = setup.dict_get("pins").unwrap() else {
        panic!("expected pins list");
    };
    let basic_id = pins[1].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
    let adv_id = pins[2].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
    let htech_id = pins[3].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
    let store_id = pins[4].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
    let pad_id = pins[5].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();

    // Negative facility-tier & unknown schematic checks against SDE planetSchematicsPinMap
    for (pin_id, bad_schem, label) in [
        (basic_id, 999_999, "unknown schematic ID"),
        (basic_id, coolant_schem, "P2 schematic on Basic facility"),
        (adv_id, water_schem, "P1 schematic on Advanced facility"),
        (adv_id, nanofactory_schem, "P4 schematic on Advanced facility"),
        (htech_id, water_schem, "P1 schematic on High-Tech facility"),
    ] {
        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "UserUpdateNetwork",
                &[
                    PyRep::Integer(JITA_PLANET_ID),
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(7),
                        PyRep::Integer(pin_id),
                        PyRep::Integer(bad_schem),
                    ])]),
                ],
                &kw,
                None,
            )
            .await
            .expect_err(label);
        assert_eq!(err.code, "InvalidSchematic", "{label}");
    }

    // Assign valid SDE schematics for each facility tier:
    // - Water (3000 Aqueous Liquids -> 20 Water, 1800s) on Basic (2492)
    // - Biocells (40 Biofuels + 40 Precious Metals -> 5 Biocells, 3600s) on Advanced (2494)
    // - Nano-Factory (6 Industrial Fibers + 6 Reactive Metals + 6 Ukomi Super Conductors -> 1 Nano-Factory, 3600s) on High-Tech (2482)
    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(basic_id), PyRep::Integer(water_schem)]),
                PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(adv_id), PyRep::Integer(coolant_schem)]),
                PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(htech_id), PyRep::Integer(nanofactory_schem)]),
            ]),
        ],
        &kw,
        None,
    )
    .await
    .expect("Install matching SDE schematics");

    // Verify GetPin reports exact SDE cycleTimeSeconds for each facility
    for (pin_id, expected_cycle) in [(basic_id, 1800), (adv_id, 3600), (htech_id, 3600)] {
        let pin_info = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "GetPin",
                &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(pin_id)],
                &kw,
                None,
            )
            .await
            .expect("GetPin");
        assert_eq!(
            pin_info.dict_get("cycleTimeSeconds"),
            Some(&PyRep::Integer(expected_cycle))
        );
    }

    let nano_inputs: Vec<(TypeId, u64)> = ctx
        .sde
        .planet_schematics
        .get(&(nanofactory_schem as u32))
        .expect("Nano-Factory schematic in SDE")
        .types
        .iter()
        .filter(|t| t.is_input)
        .map(|t| (t.type_id, t.quantity))
        .collect();
    assert!(!nano_inputs.is_empty());

    // Seed inputs into Storage without routes -> 0 factory cycles must execute
    ctx.db.with_mut_snapshot(|snap| {
        let mut net =
            eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 5);
        if let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == store_id) {
            pin.contents.insert(2268, 6_000);
            pin.contents.insert(3645, 80);
            pin.contents.insert(2390, 80);
            for &(in_tid, in_qty) in &nano_inputs {
                pin.contents.insert(in_tid, in_qty * 2);
            }
        }
        eve_server::services::industry_pi::save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
        if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, JITA_PLANET_ID)) {
            col.last_extracted_unix = ctx.now_unix();
        }
    });
    ctx.advance_virtual_clock(3_600);
    let col_unrouted = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "GetColony",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetColony unrouted");
    assert_eq!(
        col_unrouted.dict_get("simulatedProcessedQuantity"),
        Some(&PyRep::Integer(0))
    );

    // Wire links & input/output routes -> 2 Basic cycles (40 Water) + 1 Adv cycle (5 Biocells) + 1 High-Tech cycle (1 Nano-Factory) = 46 units
    let mut route_cmds = vec![
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(store_id), PyRep::Integer(basic_id), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(basic_id), PyRep::Integer(pad_id), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(store_id), PyRep::Integer(adv_id), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(adv_id), PyRep::Integer(pad_id), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(store_id), PyRep::Integer(htech_id), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(htech_id), PyRep::Integer(pad_id), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(basic_id)]), PyRep::Integer(2268), PyRep::Integer(3_000)]),
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(2), PyRep::List(vec![PyRep::Integer(basic_id), PyRep::Integer(pad_id)]), PyRep::Integer(3645), PyRep::Integer(20)]),
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(3), PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(adv_id)]), PyRep::Integer(3645), PyRep::Integer(40)]),
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(4), PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(adv_id)]), PyRep::Integer(2390), PyRep::Integer(40)]),
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(5), PyRep::List(vec![PyRep::Integer(adv_id), PyRep::Integer(pad_id)]), PyRep::Integer(9832), PyRep::Integer(5)]),
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(9), PyRep::List(vec![PyRep::Integer(htech_id), PyRep::Integer(pad_id)]), PyRep::Integer(2875), PyRep::Integer(1)]),
    ];
    for (idx, &(in_tid, in_qty)) in nano_inputs.iter().enumerate() {
        route_cmds.push(PyRep::Tuple(vec![
            PyRep::Integer(6),
            PyRep::Integer(10 + idx as i64),
            PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(htech_id)]),
            PyRep::Integer(i64::from(in_tid)),
            PyRep::Integer(in_qty as i64),
        ]));
    }
    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(route_cmds),
        ],
        &kw,
        None,
    )
    .await
    .expect("Create PI factory routes");

    ctx.advance_virtual_clock(3_600);
    let col_routed = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "GetColony",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetColony routed");
    assert_eq!(
        col_routed.dict_get("simulatedProcessedQuantity"),
        Some(&PyRep::Integer(46))
    );
}

#[tokio::test]
async fn test_wallet_evermarks_sde_paragon_catalog_and_redemption() {

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 732, "demo", "demo-secret", char_id).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_015_169);
            ch.balance = IskCents::from_isk(1_000_000);
            let sid = ch.active_ship_id;
            if let Some(ship) = snap.items.get_mut(&sid) {
                ship.location_id = 60_015_169;
            }
        }
    });
    sess.station_id = Some(60_015_169);

    // 1. Out-of-the-box Paragon EverMarks catalog loaded from V24 shipcosmeticlicenses.fsdbinary
    let offers_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "LPStoreMgr",
            None,
            "GetAvailableOffersFromCorp",
            &[PyRep::Integer(PARAGON_CORP_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetAvailableOffersFromCorp Paragon");
    let PyRep::List(offers) = offers_rep else {
        panic!("expected List of Paragon offers");
    };
    assert!(
        offers.len() > 800,
        "Expected >800 Paragon ship emblem offers (Tranquility ESI + supplementary V24 SDE), got {}",
        offers.len()
    );
    let rifter_offer_id = 17_208;
    let slasher_alliance_offer_id = 17_221;
    let rifter_offer = offers
        .iter()
        .find(|o| {
            o.dict_get("offerID") == Some(&PyRep::Integer(rifter_offer_id))
                && o.dict_get("typeID") == Some(&PyRep::Integer(72964))
        })
        .expect("Tranquility Rifter emblem offer 17208 present");
    let Some(PyRep::Integer(rifter_lp_cost)) = rifter_offer.dict_get("lpCost") else {
        panic!("expected integer lpCost on Rifter emblem offer");
    };
    assert_eq!(*rifter_lp_cost, 5_000);
    assert_eq!(rifter_offer.dict_get("iskCost"), Some(&PyRep::Real(0.0)));

    let slasher_offer = offers
        .iter()
        .find(|o| {
            o.dict_get("offerID") == Some(&PyRep::Integer(slasher_alliance_offer_id))
                && o.dict_get("typeID") == Some(&PyRep::Integer(73394))
        })
        .expect("Tranquility Slasher alliance emblem offer 17221 present");
    let Some(PyRep::Integer(slasher_lp_cost)) = slasher_offer.dict_get("lpCost") else {
        panic!("expected integer lpCost on Slasher alliance emblem offer");
    };
    assert_eq!(*slasher_lp_cost, 5_000);
    assert_eq!(slasher_offer.dict_get("iskCost"), Some(&PyRep::Real(0.0)));

    // Verify ESI GET /v1/loyalty/stores/1000419/offers exposes the same V24 SDE catalog
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let esi_req = EsiHttpRequest::new("GET", "/v1/loyalty/stores/1000419/offers/");
    let esi_resp = router.handle_request(&esi_req);
    assert_eq!(esi_resp.status_code, 200);
    spec.validate_response(
        "GET",
        "/v1/loyalty/stores/1000419/offers/",
        200,
        &esi_resp.headers,
        &esi_resp.body,
    )
    .expect("OpenAPI validation for Paragon LP store offers");
    let esi_json = esi_resp.json().expect("JSON");
    assert_eq!(esi_json.as_array().unwrap().len(), offers.len());

    // 2. Negative redemption check when character has 0 EverMarks
    let insuf_err = ctx
        .dispatch_rpc(
            &mut sess,
            "LPStoreMgr",
            None,
            "TakeOfferForCharacter",
            &[
                PyRep::Integer(PARAGON_CORP_ID),
                PyRep::Integer(rifter_offer_id),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Redeeming without EverMarks must fail");
    assert_eq!(insuf_err.code, "InsufficientLoyaltyPoints");

    // 3. Accrue EverMarks via validated SKINR sequencing jobs (empty payload awards 0 EM)
    let mk_req = |corr: &[u8], type_name: &str, payload: Vec<u8>| {
        eve_proto::quasar::envelopes::EvePublicRequest {
            issued: Some(eve_proto::quasar::types::GoogleTimestamp {
                seconds: ctx.now_unix() as i64,
                nanos: 0,
            }),
            correlation_uuid: corr.to_vec(),
            payload: Some(eve_proto::quasar::types::GoogleAny {
                type_url: format!("{}{type_name}", eve_proto::quasar::TYPE_GOOGLEAPIS_PREFIX),
                value: payload,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"em-remediation-01".to_vec(),
            authoritative_context: None,
            trace_context: None,
        }
    };
    let empty_rsp = ctx.quasar_gateway.handle_public_request(
        &sess,
        &mk_req(
            b"corr-em-empty",
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            Vec::new(),
        ),
        None,
        false,
        ctx.now_unix(),
    );
    assert_eq!(empty_rsp.status_code, 400);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points.get(&PARAGON_CORP_ID).copied().unwrap_or(0)),
        0
    );

    let skinr_cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
    let coating_comp = skinr_cat
        .components
        .values()
        .find(|c| !c.is_pattern() && !c.associated_types.is_empty() && !c.disallowed_slots.contains(&1))
        .expect("SDE SKINR coating");
    let (coating_item_type_id, _) = coating_comp.associated_types[0];
    let coating_item_id = ctx.db.with_mut_snapshot(|snap| {
        for sid in [81366u32, 81375] {
            snap.skills.entry(char_id).or_default().insert(
                sid,
                TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 },
            );
        }
        seed_item(snap, coating_item_type_id, char_id, 60_015_169, InventoryFlag::Hangar, 11)
    });

    let mut seq_item = Vec::new();
    encode_varint_field(1, coating_item_id as u64, &mut seq_item);
    let mut item_outer = Vec::new();
    encode_bytes_field(1, &seq_item, &mut item_outer);
    let mut consume_req = Vec::new();
    encode_bytes_field(1, &item_outer, &mut consume_req);
    encode_varint_field(2, 11, &mut consume_req);
    assert_eq!(
        ctx.quasar_gateway
            .handle_public_request(
                &sess,
                &mk_req(
                    b"corr-em-consume",
                    "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
                    consume_req,
                ),
                None,
                false,
                ctx.now_unix(),
            )
            .status_code,
        200
    );

    let seq_job = |idx: u32, name: &str, qty: u64| {
        let mut seq_comp = Vec::new();
        encode_varint_field(1, coating_comp.component_id as u64, &mut seq_comp);
        let mut coat_cfg = Vec::new();
        encode_bytes_field(1, &seq_comp, &mut coat_cfg);
        let mut cfg_outer = Vec::new();
        encode_bytes_field(1, &coat_cfg, &mut cfg_outer);
        let mut seq_slot = Vec::new();
        encode_varint_field(1, 1, &mut seq_slot);
        let mut slot_msg = Vec::new();
        encode_bytes_field(1, &seq_slot, &mut slot_msg);
        encode_bytes_field(2, &cfg_outer, &mut slot_msg);
        let mut seq_hull = Vec::new();
        encode_varint_field(1, 587, &mut seq_hull);
        let mut layout = Vec::new();
        encode_bytes_field(1, &slot_msg, &mut layout);
        encode_bytes_field(2, &seq_hull, &mut layout);
        encode_varint_field(3, 1, &mut layout);
        let mut init_req = Vec::new();
        encode_string_field(1, name, &mut init_req);
        encode_string_field(2, "Paragon Custom", &mut init_req);
        encode_bytes_field(3, &layout, &mut init_req);
        encode_varint_field(5, qty, &mut init_req);
        ctx.quasar_gateway.handle_public_request(
            &sess,
            &mk_req(
                format!("corr-em-seq-{idx}").as_bytes(),
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
                init_req,
            ),
            None,
            false,
            ctx.now_unix(),
        )
    };

    for (idx, name) in [(1, "Rifter Gold"), (2, "Rifter Crimson")] {
        assert_eq!(seq_job(idx, name, 1).status_code, 200);
    }

    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points.get(&PARAGON_CORP_ID).copied().unwrap_or(0)),
        0
    );

    // 4. Sequence a 9-license batch and redeem Rifter Corporation Emblem (72964, offer 17208, 5,000 LP)
    assert_eq!(seq_job(3, "Rifter Fleet Batch", 9).status_code, 200);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points.get(&PARAGON_CORP_ID).copied().unwrap_or(0)),
        0
    );
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().loyalty_points.insert(PARAGON_CORP_ID, 5_500);
    });
    let take_res = ctx
        .dispatch_rpc(
            &mut sess,
            "LPStoreMgr",
            None,
            "TakeOfferForCharacter",
            &[
                PyRep::Integer(PARAGON_CORP_ID),
                PyRep::Integer(rifter_offer_id),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("Redeem Rifter Corporation Emblem");
    assert_eq!(take_res.dict_get("deliveredTypeID"), Some(&PyRep::Integer(72964)));
    assert_eq!(
        take_res.dict_get("remainingLP"),
        Some(&PyRep::Integer(5_500 - *rifter_lp_cost))
    );
}

}
