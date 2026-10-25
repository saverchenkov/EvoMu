//! Integration tests for Planet Manager (`planetMgr`) orbitals, launches, GM tools, and simulation:
//! Flush, UserTransferCommodities, UserLaunchCommodities, GetMyLaunchesDetails, DeleteLaunch,
//! CancelInstallProgram, RevertChanges, StartTicking, colonies, celestialIndex,
//! GMAddCommodity, GMConvertCommandCenter, GMForceInstallProgram, GMVerifySimulation,
//! GMGetLocalDistributionReport, GMGetSynchedServerState, GMRunDepletionSim, append, remoteHandler.

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

fn get_str<'a>(rep: &'a PyRep, key: &str) -> Option<&'a str> {
    rep.dict_get(key).and_then(|v| v.as_str())
}

fn cmd_pin(id: i64, tid: TypeId, lat: f64, lon: f64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(id), PyRep::Integer(tid as i64), PyRep::Real(lat), PyRep::Real(lon)])
}

fn cmd_link(a: i64, b: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(a), PyRep::Integer(b), PyRep::Integer(0)])
}

fn cmd_upgrade_cc(id: i64, lvl: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(id), PyRep::Integer(lvl)])
}

fn cmd_extractor_program(id: i64, tid: TypeId, radius: f64, heads: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(id), PyRep::Integer(tid as i64), PyRep::Real(radius), PyRep::Integer(heads)])
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
    let ext_pin = col_pins.iter().find(|p| get_i64(p, "typeID") == Some(GAS_EXTRACTOR_TYPE_ID as i64)).expect("ext pin");
    let ext_pin_id = get_i64(ext_pin, "pinID").expect("ext pin id");
    let fac_pin = col_pins.iter().find(|p| get_i64(p, "typeID") == Some(GAS_BASIC_FACILITY_TYPE_ID as i64)).expect("fac pin");
    let fac_pin_id = get_i64(fac_pin, "pinID").expect("fac pin id");

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

    let produced_p0_tid = get_i64(&flush_rep, "producedTypeID").expect("produced type id") as TypeId;
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

    // 2. UserTransferCommodities: V24 client expedited transfer via path and commodities dict
    let comms = vec![(PyRep::Integer(produced_p0_tid as i64), PyRep::Integer(10))];
    let xfer_rep = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserTransferCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![
                PyRep::Integer(ext_pin_id),
                PyRep::Integer(fac_pin_id),
                PyRep::Integer(launch_pin_id),
            ]),
            PyRep::Dict(comms),
        ],
    )
    .await
    .expect("UserTransferCommodities V24");
    assert_eq!(get_i64(&xfer_rep, "planetID"), Some(JITA_PLANET_ID));
    assert_eq!(get_i64(&xfer_rep, "sourcePinID"), Some(ext_pin_id));
    assert_eq!(get_i64(&xfer_rep, "destPinID"), Some(launch_pin_id));
    assert_eq!(get_i64(&xfer_rep, "commodityTypeID"), Some(produced_p0_tid as i64));
    assert_eq!(get_i64(&xfer_rep, "quantity"), Some(10));

    // Pin-to-pin transfer: Launchpad to Command Center
    let comms2 = vec![(PyRep::Integer(produced_p0_tid as i64), PyRep::Integer(5))];
    let pin_xfer = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserTransferCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![
                PyRep::Integer(launch_pin_id),
                PyRep::Integer(fac_pin_id),
                PyRep::Integer(ext_pin_id),
                PyRep::Integer(cc_pin_id),
            ]),
            PyRep::Dict(comms2),
        ],
    )
    .await
    .expect("Pin-to-pin transfer");
    assert_eq!(get_i64(&pin_xfer, "sourcePinID"), Some(launch_pin_id));
    assert_eq!(get_i64(&pin_xfer, "destPinID"), Some(cc_pin_id));
    assert_eq!(get_i64(&pin_xfer, "quantity"), Some(5));

    ctx.db.with_snapshot(|snap| {
        let net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
        let lp = net.pins.iter().find(|p| p.pin_id == launch_pin_id).expect("lp");
        let cp = net.pins.iter().find(|p| p.pin_id == cc_pin_id).expect("cp");
        assert_eq!(lp.contents.get(&produced_p0_tid).copied(), Some(5));
        assert_eq!(cp.contents.get(&produced_p0_tid).copied(), Some(5));
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
        let mut net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
        if let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == cc_pin_id) {
            *pin.contents.entry(WATER_P1_TYPE_ID).or_insert(0) += 500;
        }
        eve_server::services::industry_pi::save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
    });

    // 3. UserLaunchCommodities: orbital launch from Command Center
    let launch_rep = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(cc_pin_id),
            PyRep::Integer(WATER_P1_TYPE_ID as i64),
            PyRep::Integer(300),
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
    let cancel_rep = rpc(&ctx, &mut sess, "planetMgr", "CancelInstallProgram", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ext_pin_id)])
        .await
        .expect("CancelInstallProgram");
    assert_eq!(cancel_rep.as_bool(), Some(true));

    let prog_cleared = ctx.db.with_snapshot(|snap| {
        let net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
        net.pins.iter().find(|p| p.pin_id == ext_pin_id).map(|p| p.extractor_resource_type_id.is_none()).unwrap_or(false)
    });
    assert!(prog_cleared);

    let revert_rep = rpc(&ctx, &mut sess, "planetMgr", "RevertChanges", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("RevertChanges");
    assert_eq!(revert_rep.as_bool(), Some(true));

    let tick_rep = rpc(&ctx, &mut sess, "planetMgr", "StartTicking", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("StartTicking");
    assert_eq!(tick_rep.as_bool(), Some(true));

    // 7. colonies and celestialIndex accessors with cross-character isolation
    let char_other = 90_000_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (char_other, 40_009_083),
            eve_db::PlanetaryColonyRecord {
                planet_id: 40_009_083,
                owner_id: char_other,
                solar_system_id: 30000142,
                command_center_level: 1,
                pins_count: 1,
                last_extracted_unix: 0,
                extracted_commodities: std::collections::HashMap::new(),
            },
        );
    });

    let colonies_rep = rpc(&ctx, &mut sess, "planetMgr", "colonies", &[]).await.expect("colonies");
    let PyRep::Dict(col_entries) = colonies_rep else { panic!("expected colonies dict"); };
    assert_eq!(col_entries.len(), 1);
    assert_eq!(col_entries[0].0.as_i64(), Some(char_id));
    let PyRep::List(char_cols) = &col_entries[0].1 else { panic!("expected cols list"); };
    assert_eq!(char_cols.len(), 1);
    assert_eq!(get_i64(&char_cols[0], "planetID"), Some(JITA_PLANET_ID));

    let cel_rep = rpc(&ctx, &mut sess, "planetMgr", "celestialIndex", &[PyRep::Integer(JITA_PLANET_ID)]).await.expect("celestialIndex");
    assert_eq!(cel_rep.as_i64(), Some(4));
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

    // 1. Role gate: Regular player session rejected with AdminAccessDenied on all 7 GM methods
    for method in &[
        "GMAddCommodity",
        "GMConvertCommandCenter",
        "GMForceInstallProgram",
        "GMVerifySimulation",
        "GMGetLocalDistributionReport",
        "GMGetSynchedServerState",
        "GMRunDepletionSim",
    ] {
        let err = rpc(&ctx, &mut sess, "planetMgr", method, &[PyRep::Integer(JITA_PLANET_ID)])
            .await
            .expect_err(method);
        // rpc-lint: allow GM authorization rejection error code assertion
        assert_eq!(err.code, "AdminAccessDenied");
    }

    // 2. Deploy colony before GM modifications
    let gm_col_dep = rpc(
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

    let PyRep::List(gm_pins) = gm_col_dep.dict_get("pins").expect("pins list") else { panic!("pins"); };
    let gm_ext_pin = gm_pins.iter().find(|p| get_i64(p, "typeID") == Some(GAS_EXTRACTOR_TYPE_ID as i64)).expect("ext pin");
    let gm_ext_pin_id = get_i64(gm_ext_pin, "pinID").expect("ext pin id");

    // 3. Elevate session to GM role
    sess.role |= service_roles::ROLE_ADMIN;
    assert!(sess.has_gm_role());

    // 4. GM state mutations
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

    let has_water = ctx.db.with_snapshot(|snap| {
        snap.planetary_colonies.get(&(char_id, JITA_PLANET_ID))
            .and_then(|c| c.extracted_commodities.get(&(WATER_P1_TYPE_ID as u32)))
            .copied()
            .unwrap_or(0) == 100
    });
    assert!(has_water);

    let gm_conv = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "GMConvertCommandCenter",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(GAS_COMMAND_CENTER_TYPE_ID as i64), PyRep::Integer(4)],
    )
    .await
    .expect("GMConvertCommandCenter");
    assert_eq!(gm_conv.as_bool(), Some(true));

    let cc_lvl = ctx.db.with_snapshot(|snap| {
        snap.planetary_colonies.get(&(char_id, JITA_PLANET_ID)).map(|c| c.command_center_level)
    });
    assert_eq!(cc_lvl, Some(4));

    let gm_force = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "GMForceInstallProgram",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(gm_ext_pin_id), PyRep::Integer(WATER_P1_TYPE_ID as i64), PyRep::Integer(5)],
    )
    .await
    .expect("GMForceInstallProgram");
    assert_eq!(gm_force.as_bool(), Some(true));

    let prog_installed = ctx.db.with_snapshot(|snap| {
        let net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 4);
        net.pins.iter().find(|p| p.pin_id == gm_ext_pin_id).map(|p| p.extractor_resource_type_id == Some(WATER_P1_TYPE_ID as u32)).unwrap_or(false)
    });
    assert!(prog_installed);

    let gm_sim = rpc(&ctx, &mut sess, "planetMgr", "GMVerifySimulation", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GMVerifySimulation");
    assert_eq!(gm_sim.dict_get("valid").and_then(|v| v.as_bool()), Some(true));

    // 5. GM diagnostic reports & depletion simulations
    let gm_dist = rpc(&ctx, &mut sess, "planetMgr", "GMGetLocalDistributionReport", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GMGetLocalDistributionReport");
    assert_eq!(get_i64(&gm_dist, "planetID"), Some(JITA_PLANET_ID));
    assert_eq!(get_i64(&gm_dist, "characterID"), Some(char_id));
    assert_eq!(get_i64(&gm_dist, "pinsCount"), Some(3));
    let PyRep::Dict(rep_entries) = gm_dist.dict_get("report").expect("report") else { panic!("report"); };
    assert!(!rep_entries.is_empty());

    let gm_sync = rpc(&ctx, &mut sess, "planetMgr", "GMGetSynchedServerState", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GMGetSynchedServerState");
    assert_eq!(get_i64(&gm_sync, "colonyCount"), Some(1));
    let PyRep::List(pins) = gm_sync.dict_get("pins").expect("pins") else { panic!("pins"); };
    assert_eq!(pins.len(), 3);

    let gm_dep = rpc(&ctx, &mut sess, "planetMgr", "GMRunDepletionSim", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(48)])
        .await
        .expect("GMRunDepletionSim");
    assert_eq!(get_i64(&gm_dep, "colonyCount"), Some(1));
    assert_eq!(get_i64(&gm_dep, "durationHours"), Some(48));
    let PyRep::List(curve) = gm_dep.dict_get("depletionCurve").expect("curve") else { panic!("curve"); };
    assert_eq!(curve.len(), 48);

    // 6. Proxy properties: append and remoteHandler
    let append_rep = rpc(&ctx, &mut sess, "planetMgr", "append", &[]).await.expect("append");
    assert_eq!(append_rep, PyRep::None);

    let remote_rep = rpc(&ctx, &mut sess, "planetMgr", "remoteHandler", &[]).await.expect("remoteHandler");
    assert_eq!(remote_rep, PyRep::None);
}
