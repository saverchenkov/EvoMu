//! Integration tests for Planet Manager (`planetMgr`) open RPCs and PI storage/simulation gap behavioral verification:
//! - rpc:planetMgr.GetPin
//! - rpc:planetMgr.GetExtractorsForPlanet
//! - rpc:planetMgr.GetCommandPinsForPlanet
//! - rpc:planetMgr.RevertChanges
//! - behavioral verification of BF-PI-IMPORT-PIN-TYPE-HARDCODE-AND-DOUBLE-CREDIT-01
//! - behavioral verification of BF-PI-SIMULATION-SPLIT-INPUT-MINT-02

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::industry_pi::*;
use std::collections::HashMap;

const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const JITA_PLANET_ID: i64 = 40_009_082;
const BARREN_PLANET_ID: i64 = 40_009_077;

const GAS_CC_TYPE: TypeId = 2534;
const GAS_EXT_TYPE: TypeId = 2848;
const GAS_PROC_TYPE: TypeId = 2492;
const GAS_LP_TYPE: TypeId = 2543;
const GAS_STORAGE_TYPE: TypeId = 2536;

const BARREN_CC_TYPE: TypeId = 2524;
const BARREN_LP_TYPE: TypeId = 2544;
const BARREN_STORAGE_TYPE: TypeId = 2541;

const WATER_P1_TYPE: TypeId = 3645;
const AQUEOUS_LIQUIDS_P0_TYPE: TypeId = 2268;

async fn setup_pilot(ctx: &ServerContext, session_id: u64, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
    sess.station_id = None;
    sess.structure_id = None;
    sess
}

fn grant_pi_skills(snap: &mut eve_db::WorldStateSnapshot, char_id: i64) {
    if let Some(ch) = snap.characters.get_mut(&char_id) {
        ch.balance = IskCents::from_isk(100_000_000);
        ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
        ch.station_id = None;
        ch.structure_id = None;
    }
    let sk = snap.skills.entry(char_id).or_default();
    for (stid, lvl) in [
        (2505, 5),  // CommandCenterUpgrades
        (2495, 5),  // InterplanetaryConsolidation
        (13279, 5), // RemoteSensing
        (2406, 5),  // Planetology
        (2403, 5),  // AdvancedPlanetology
        (33467, 5), // CustomsCodeExpertise
    ] {
        sk.insert(stid, TrainedSkillRecord { type_id: stid, level: lvl, skill_points: 256_000.0 });
    }
}

fn seed_cargo(snap: &mut eve_db::WorldStateSnapshot, type_id: TypeId, owner_id: i64, location_id: i64, qty: i32) -> i64 {
    snap.next_entity_id += 1;
    let iid = snap.next_entity_id;
    snap.items.insert(
        iid,
        InventoryItemRecord {
            item_id: iid,
            type_id,
            owner_id,
            location_id,
            flag: InventoryFlag::Cargo,
            quantity: qty,
            singleton: false,
            version: 1,
            ..Default::default()
        },
    );
    iid
}

fn get_i64(rep: &PyRep, key: &str) -> Option<i64> {
    rep.dict_get(key).and_then(|v| v.as_i64())
}

#[tokio::test]
async fn test_pi_planet_mgr_rpc() {
    // parity: rpc:planetMgr.GetPin, rpc:planetMgr.GetExtractorsForPlanet, rpc:planetMgr.GetCommandPinsForPlanet, rpc:planetMgr.RevertChanges
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 2101, char_id).await;
    let ship_id = sess.ship_id.expect("active ship");

    ctx.db.with_mut_snapshot(|snap| {
        grant_pi_skills(snap, char_id);
        seed_cargo(snap, GAS_CC_TYPE, char_id, ship_id, 1);
    });

    let water_schem = ctx
        .sde
        .planet_schematics
        .iter()
        .find(|(_, s)| s.types.iter().any(|t| !t.is_input && t.type_id == WATER_P1_TYPE))
        .map(|(&id, _)| i64::from(id))
        .expect("Water P1 schematic in SDE");

    // 1. Setup full colony network on JITA_PLANET_ID
    let update_cmds = vec![
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(101), PyRep::Integer(GAS_CC_TYPE as i64), PyRep::Real(0.10), PyRep::Real(0.10)]),
        PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(101), PyRep::Integer(3)]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(GAS_EXT_TYPE as i64), PyRep::Real(0.11), PyRep::Real(0.11)]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(GAS_PROC_TYPE as i64), PyRep::Real(0.12), PyRep::Real(0.12)]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(104), PyRep::Integer(GAS_LP_TYPE as i64), PyRep::Real(0.13), PyRep::Real(0.13)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(101), PyRep::Integer(102), PyRep::Integer(0)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(102), PyRep::Integer(103), PyRep::Integer(0)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(103), PyRep::Integer(104), PyRep::Integer(0)]),
        PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(103), PyRep::Integer(water_schem)]),
        PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(102), PyRep::Integer(AQUEOUS_LIQUIDS_P0_TYPE as i64), PyRep::Real(0.02), PyRep::Integer(3)]),
    ];
    let net_rep = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(update_cmds)], &kw, None)
        .await
        .expect("UserUpdateNetwork colony setup");
    assert_eq!(get_i64(&net_rep, "commandCenterLevel"), Some(3));
    assert_eq!(get_i64(&net_rep, "pinsCount"), Some(4));

    let (cc_pin_id, ext_pin_id, proc_pin_id, lp_pin_id) = ctx.db.with_snapshot(|snap| {
        let net = load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
        let cc = net.pins.iter().find(|p| p.type_id == GAS_CC_TYPE).unwrap().pin_id;
        let ext = net.pins.iter().find(|p| p.type_id == GAS_EXT_TYPE).unwrap().pin_id;
        let proc = net.pins.iter().find(|p| p.type_id == GAS_PROC_TYPE).unwrap().pin_id;
        let lp = net.pins.iter().find(|p| p.type_id == GAS_LP_TYPE).unwrap().pin_id;
        (cc, ext, proc, lp)
    });

    // 2. rpc:planetMgr.GetCommandPinsForPlanet
    let cmd_pins_rep = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetCommandPinsForPlanet", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("GetCommandPinsForPlanet");
    let PyRep::List(cmd_pins) = cmd_pins_rep else { panic!("expected list"); };
    assert_eq!(cmd_pins.len(), 1, "Only Command Center pin must be returned");
    assert_eq!(get_i64(&cmd_pins[0], "pinID"), Some(cc_pin_id));
    assert_eq!(get_i64(&cmd_pins[0], "typeID"), Some(GAS_CC_TYPE as i64));
    assert_eq!(get_i64(&cmd_pins[0], "level"), Some(3));
    assert!(cmd_pins[0].dict_get("latitude").is_some());
    assert!(cmd_pins[0].dict_get("longitude").is_some());

    // Error path: missing planet ID
    let cmd_err = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetCommandPinsForPlanet", &[], &kw, None)
        .await
        .expect_err("Missing planetID must fail");
    assert_eq!(cmd_err.code, "InvalidPlanetID");

    // 3. rpc:planetMgr.GetExtractorsForPlanet
    let ext_pins_rep = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetExtractorsForPlanet", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("GetExtractorsForPlanet");
    let PyRep::List(ext_pins) = ext_pins_rep else { panic!("expected list"); };
    assert_eq!(ext_pins.len(), 1, "Only Extractor pin must be returned");
    assert_eq!(get_i64(&ext_pins[0], "pinID"), Some(ext_pin_id));
    assert_eq!(get_i64(&ext_pins[0], "typeID"), Some(GAS_EXT_TYPE as i64));
    assert_eq!(get_i64(&ext_pins[0], "headCount"), Some(3));
    assert!(get_i64(&ext_pins[0], "cycleTimeSeconds").unwrap_or(0) >= 300);

    // Error path: missing planet ID
    let ext_err = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetExtractorsForPlanet", &[], &kw, None)
        .await
        .expect_err("Missing planetID must fail");
    assert_eq!(ext_err.code, "InvalidPlanetID");

    // 4. rpc:planetMgr.GetPin: queries across pin types
    let pin_cc = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(cc_pin_id)], &kw, None)
        .await
        .expect("GetPin CC");
    assert_eq!(get_i64(&pin_cc, "pinID"), Some(cc_pin_id));
    assert_eq!(get_i64(&pin_cc, "typeID"), Some(GAS_CC_TYPE as i64));

    let pin_ext = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ext_pin_id)], &kw, None)
        .await
        .expect("GetPin Extractor");
    assert_eq!(get_i64(&pin_ext, "pinID"), Some(ext_pin_id));
    assert_eq!(get_i64(&pin_ext, "headCount"), Some(3));

    let pin_proc = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(proc_pin_id)], &kw, None)
        .await
        .expect("GetPin Processor");
    assert_eq!(get_i64(&pin_proc, "pinID"), Some(proc_pin_id));
    assert_eq!(get_i64(&pin_proc, "schematicID"), Some(water_schem));
    assert_eq!(get_i64(&pin_proc, "cycleTimeSeconds"), Some(1800));

    let pin_lp = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(lp_pin_id)], &kw, None)
        .await
        .expect("GetPin Launchpad");
    assert_eq!(get_i64(&pin_lp, "pinID"), Some(lp_pin_id));
    assert_eq!(get_i64(&pin_lp, "typeID"), Some(GAS_LP_TYPE as i64));

    // Error path: PinNotFound
    let pin_not_found = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(999_999)], &kw, None)
        .await
        .expect_err("Unknown pin must fail");
    assert_eq!(pin_not_found.code, "PinNotFound");

    // Error path: ColonyNotFound
    let no_colony_err = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetPin", &[PyRep::Integer(49_999_999), PyRep::Integer(101)], &kw, None)
        .await
        .expect_err("Missing colony must fail");
    assert_eq!(no_colony_err.code, "ColonyNotFound");

    // 5. rpc:planetMgr.RevertChanges
    let revert_ok = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "RevertChanges", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("RevertChanges on existing colony");
    assert_eq!(revert_ok.as_bool(), Some(true));

    // Error path: RevertChanges on non-existent colony
    let revert_err = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "RevertChanges", &[PyRep::Integer(49_999_999)], &kw, None)
        .await
        .expect_err("RevertChanges on missing colony must fail");
    assert_eq!(revert_err.code, "ColonyNotFound");
}

#[tokio::test]
async fn test_planet_mgr_colony_and_extractor_validation() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 2102, char_id).await;
    let ship_id = sess.ship_id.expect("active ship");

    ctx.db.with_mut_snapshot(|snap| {
        grant_pi_skills(snap, char_id);
        seed_cargo(snap, BARREN_CC_TYPE, char_id, ship_id, 1);
        seed_cargo(snap, WATER_P1_TYPE, char_id, ship_id, 1_000);
    });

    // 1. Setup Barren colony (non-Gas planet type) with Barren CC, Launchpad, and Storage Facility
    let update_cmds = vec![
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(201), PyRep::Integer(BARREN_CC_TYPE as i64), PyRep::Real(0.20), PyRep::Real(0.20)]),
        PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(201), PyRep::Integer(3)]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(202), PyRep::Integer(BARREN_LP_TYPE as i64), PyRep::Real(0.21), PyRep::Real(0.21)]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(203), PyRep::Integer(BARREN_STORAGE_TYPE as i64), PyRep::Real(0.22), PyRep::Real(0.22)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(201), PyRep::Integer(202), PyRep::Integer(0)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(202), PyRep::Integer(203), PyRep::Integer(0)]),
    ];
    ctx.dispatch_rpc(&mut sess, "planetMgr", None, "UserUpdateNetwork", &[PyRep::Integer(BARREN_PLANET_ID), PyRep::List(update_cmds)], &kw, None)
        .await
        .expect("UserUpdateNetwork Barren colony setup");

    // 2. Capacity validation BEFORE consumption or tax
    // Attempting to import 60,000 units of Water (11,400 m3 > 10,000 m3 Spaceport Launchpad capacity)
    let cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserLaunchCommodities",
            &[PyRep::Integer(BARREN_PLANET_ID), PyRep::Integer(WATER_P1_TYPE as i64), PyRep::Integer(60_000), PyRep::String("ImportToColony".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Import exceeding Launchpad capacity must fail");
    assert_eq!(cap_err.code, "LaunchpadCapacityExceeded");

    // Verify inventory cargo was NOT debited and wallet was NOT charged
    ctx.db.with_snapshot(|snap| {
        let cargo_qty: i32 = snap.items.values()
            .filter(|i| i.owner_id == char_id && i.type_id == WATER_P1_TYPE && i.flag == InventoryFlag::Cargo)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(cargo_qty, 1_000, "Items must not be consumed when capacity check fails");
    });

    // 3. Valid ImportToColony on Barren planet: resolves Barren Launchpad 2544 via SDE group ID 1030
    let import_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserLaunchCommodities",
            &[PyRep::Integer(BARREN_PLANET_ID), PyRep::Integer(WATER_P1_TYPE as i64), PyRep::Integer(200), PyRep::String("ImportToColony".into())],
            &kw,
            None,
        )
        .await
        .expect("Valid ImportToColony on Barren planet");
    assert_eq!(get_i64(&import_rep, "routedQuantity"), Some(200));

    // Verify authoritative pin storage and NO double-credit in col.extracted_commodities
    ctx.db.with_snapshot(|snap| {
        let col = snap.planetary_colonies.get(&(char_id, BARREN_PLANET_ID)).expect("colony");
        let net = load_pi_network(snap, char_id, BARREN_PLANET_ID, 3);
        let lp_pin = net.pins.iter().find(|p| p.type_id == BARREN_LP_TYPE).expect("Barren launchpad");
        assert_eq!(lp_pin.contents.get(&WATER_P1_TYPE).copied(), Some(200));
        assert_eq!(col.extracted_commodities.get(&WATER_P1_TYPE).copied(), Some(200), "No double credit");
    });

    // 4. Flush: verify save_pi_network derives extracted_commodities authoritatively without double-adding
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, BARREN_PLANET_ID)) {
            col.last_extracted_unix = 1;
        }
    });
    let flush_rep = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "Flush", &[PyRep::Integer(BARREN_PLANET_ID)], &kw, None)
        .await
        .expect("Flush on Barren colony");
    let produced_qty = get_i64(&flush_rep, "producedQuantity").unwrap_or(0) as u64;
    let produced_tid = get_i64(&flush_rep, "producedTypeID").unwrap_or(0) as TypeId;
    assert!(produced_qty > 0);

    ctx.db.with_snapshot(|snap| {
        let col = snap.planetary_colonies.get(&(char_id, BARREN_PLANET_ID)).expect("colony");
        let net = load_pi_network(snap, char_id, BARREN_PLANET_ID, 3);
        let pin_sum: u64 = net.pins.iter().map(|p| p.contents.get(&produced_tid).copied().unwrap_or(0)).sum();
        let col_amt = col.extracted_commodities.get(&produced_tid).copied().unwrap_or(0);
        assert_eq!(pin_sum, produced_qty);
        assert_eq!(col_amt, pin_sum, "Flush must derive extracted_commodities authoritatively without double-adding");
    });

    // 5. Remediate BF-PI-SIMULATION-SPLIT-INPUT-MINT-02:
    // Multi-pin split inputs: Water schematic requires 3,000 units of Aqueous Liquids P0.
    // We split 3,000 across two storage pins: 1,200 in Pin 301, 1,800 in Pin 302.
    // Both pins have routes to Processor Pin 303.
    let water_schem = ctx
        .sde
        .planet_schematics
        .iter()
        .find(|(_, s)| s.types.iter().any(|t| !t.is_input && t.type_id == WATER_P1_TYPE))
        .map(|(&id, _)| id)
        .expect("Water P1 schematic in SDE");
    let mut net = PiColonyNetworkState {
        pins: vec![
            PiPinState { pin_id: 301, type_id: GAS_STORAGE_TYPE, contents: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 1_200)]), ..Default::default() },
            PiPinState { pin_id: 302, type_id: GAS_STORAGE_TYPE, contents: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 1_800)]), ..Default::default() },
            PiPinState { pin_id: 303, type_id: GAS_PROC_TYPE, schematic_id: Some(water_schem), ..Default::default() },
            PiPinState { pin_id: 304, type_id: GAS_LP_TYPE, contents: HashMap::new(), ..Default::default() },
        ],
        links: vec![
            PiLinkState { source_pin_id: 301, dest_pin_id: 303, level: 1, ..Default::default() },
            PiLinkState { source_pin_id: 302, dest_pin_id: 303, level: 1, ..Default::default() },
            PiLinkState { source_pin_id: 303, dest_pin_id: 304, level: 1, ..Default::default() },
        ],
        routes: vec![
            PiRouteState { route_id: 1, path: vec![301, 303], commodity_type_id: AQUEOUS_LIQUIDS_P0_TYPE, quantity: 1_200 },
            PiRouteState { route_id: 2, path: vec![302, 303], commodity_type_id: AQUEOUS_LIQUIDS_P0_TYPE, quantity: 1_800 },
            PiRouteState { route_id: 3, path: vec![303, 304], commodity_type_id: WATER_P1_TYPE, quantity: 20 },
        ],
        ..Default::default()
    };
    let mut col = eve_db::PlanetaryColonyRecord {
        planet_id: JITA_PLANET_ID,
        owner_id: char_id,
        solar_system_id: JITA_SOLAR_SYSTEM_ID,
        command_center_level: 3,
        pins_count: 4,
        last_extracted_unix: 0,
        extracted_commodities: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 3_000)]),
    };

    // Run simulation: 1,800 seconds (1 cycle of Water schematic)
    let (ext, proc) = advance_colony_simulation(&ctx, &mut col, &mut net, 1800, 1800);
    assert_eq!(ext, 0);
    assert_eq!(proc, 20, "1 cycle executed producing 20 Water P1");
    // Verify inputs were deducted from BOTH connected pins (split input fulfillment)
    assert_eq!(net.pins[0].contents.get(&AQUEOUS_LIQUIDS_P0_TYPE).copied().unwrap_or(0), 0);
    assert_eq!(net.pins[1].contents.get(&AQUEOUS_LIQUIDS_P0_TYPE).copied().unwrap_or(0), 0);
    assert_eq!(net.pins[3].contents.get(&WATER_P1_TYPE).copied().unwrap_or(0), 20);

    // 6. Negative test: Disconnected pin holding commodities does NOT allow minting
    let mut net_isolated = PiColonyNetworkState {
        pins: vec![
            // Connected pin has 0
            PiPinState { pin_id: 401, type_id: GAS_STORAGE_TYPE, contents: HashMap::new(), ..Default::default() },
            // Isolated unconnected pin has 50,000 commodities
            PiPinState { pin_id: 402, type_id: GAS_STORAGE_TYPE, contents: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 50_000)]), ..Default::default() },
            PiPinState { pin_id: 403, type_id: GAS_PROC_TYPE, schematic_id: Some(water_schem), ..Default::default() },
            PiPinState { pin_id: 404, type_id: GAS_LP_TYPE, contents: HashMap::new(), ..Default::default() },
        ],
        links: vec![
            PiLinkState { source_pin_id: 401, dest_pin_id: 403, level: 1, ..Default::default() },
            PiLinkState { source_pin_id: 403, dest_pin_id: 404, level: 1, ..Default::default() },
        ],
        routes: vec![
            PiRouteState { route_id: 1, path: vec![401, 403], commodity_type_id: AQUEOUS_LIQUIDS_P0_TYPE, quantity: 3_000 },
            PiRouteState { route_id: 2, path: vec![403, 404], commodity_type_id: WATER_P1_TYPE, quantity: 20 },
        ],
        ..Default::default()
    };
    let mut col_isolated = eve_db::PlanetaryColonyRecord {
        planet_id: JITA_PLANET_ID,
        owner_id: char_id,
        solar_system_id: JITA_SOLAR_SYSTEM_ID,
        command_center_level: 3,
        pins_count: 4,
        last_extracted_unix: 0,
        // Colony total has 50,000 in extracted_commodities from the isolated pin
        extracted_commodities: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 50_000)]),
    };

    let (ext2, proc2) = advance_colony_simulation(&ctx, &mut col_isolated, &mut net_isolated, 1800, 3600);
    assert_eq!(ext2, 0);
    assert_eq!(proc2, 0, "No output minted when connected pin cannot supply input");
    assert_eq!(net_isolated.pins[3].contents.get(&WATER_P1_TYPE).copied().unwrap_or(0), 0);
    assert_eq!(net_isolated.pins[1].contents.get(&AQUEOUS_LIQUIDS_P0_TYPE).copied(), Some(50_000));
}

pub(crate) mod pi_planet_mgr_bound_unbound {
    // Integration tests for Planet Manager (`planetMgr`) open RPCs and P1 gap behavioral verification:
    // - rpc:planetMgr.CancelInstallProgram
    // - rpc:planetMgr.DeleteLaunch
    // - rpc:planetMgr.GetMyLaunchesDetails
    // - rpc:planetMgr.GetProgramResultInfo
    // - rpc:planetMgr.GetResourceData
    // - rpc:planetMgr.UserAbandonPlanet
    // - behavioral verification of BF-PI-SIMULATION-UNLINKED-ROUTE-AND-CAPACITY-OVERFLOW-01
    // - behavioral verification of BF-PI-LAUNCH-DOUBLE-DEDUCTION-AND-PIN-FALLBACK-02

    use crate::common;
    use eve_db::{IskCents, TrainedSkillRecord, TypeId};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::industry_pi::*;
    use std::collections::HashMap;

    const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
    const JITA_PLANET_ID: i64 = 40_009_082;
    const BARREN_PLANET_ID: i64 = 40_009_077;

    const GAS_COMMAND_CENTER_TYPE_ID: TypeId = 2534;
    const GAS_EXTRACTOR_TYPE_ID: TypeId = 2848;
    const GAS_BASIC_FACILITY_TYPE_ID: TypeId = 2492;
    const GAS_LAUNCHPAD_TYPE_ID: TypeId = 2543;
    const GAS_STORAGE_TYPE_ID: TypeId = 2536;

    const WATER_P1_TYPE: TypeId = 3645;
    const AQUEOUS_LIQUIDS_P0_TYPE: TypeId = 2268;

    async fn setup_pilot(ctx: &ServerContext, session_id: u64, char_id: i64) -> SessionState {
        let mut sess = SessionState::new(session_id);
        common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
        ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
            .await
            .expect("SelectCharacterID");
        sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
        sess.station_id = None;
        sess.structure_id = None;
        sess
    }

    fn grant_pi_skills(snap: &mut eve_db::WorldStateSnapshot, char_id: i64) {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(100_000_000);
            ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
            ch.station_id = None;
            ch.structure_id = None;
        }
        let sk = snap.skills.entry(char_id).or_default();
        for (stid, lvl) in [(2505, 5), (2495, 4), (13279, 4), (2406, 3), (2403, 2)] {
            sk.insert(stid, TrainedSkillRecord { type_id: stid, level: lvl, skill_points: 256_000.0 });
        }
    }

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
    }

    async fn rpc_bound(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        planet_id: i64,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        let bound_id = ctx.bind_object(sess, service, planet_id)?;
        ctx.dispatch_rpc(sess, service, Some(bound_id), method, args, &HashMap::new(), None).await
    }

    fn get_kv_i64(rep: &PyRep, key: &str) -> Option<i64> {
        match rep {
            PyRep::KeyVal(kv) => kv.get(key).and_then(|v| v.as_i64()),
            PyRep::Dict(entries) => entries
                .iter()
                .find(|(k, _)| k.as_str() == Some(key))
                .and_then(|(_, v)| v.as_i64()),
            _ => None,
        }
    }

    /// Integration test verifying the 6 open planetMgr RPCs with bound/unbound shapes:
    /// CancelInstallProgram, DeleteLaunch, GetMyLaunchesDetails, GetProgramResultInfo,
    /// GetResourceData, and UserAbandonPlanet.
    #[tokio::test]
    async fn test_pi_planet_mgr_open_rpcs_bound_and_unbound() {
        // parity: rpc:planetMgr.CancelInstallProgram, rpc:planetMgr.DeleteLaunch, rpc:planetMgr.GetMyLaunchesDetails, rpc:planetMgr.GetProgramResultInfo, rpc:planetMgr.GetResourceData, rpc:planetMgr.UserAbandonPlanet
        let ctx = ServerContext::new();
        let char_id = 90_000_001;
        let session_id = 701;
        let mut sess = setup_pilot(&ctx, session_id, char_id).await;

        ctx.db.with_mut_snapshot(|snap| {
            grant_pi_skills(snap, char_id);
            // Seed colony with CC, Extractor, Launchpad, and Storage pins
            snap.planetary_colonies.insert(
                (char_id, JITA_PLANET_ID),
                eve_db::PlanetaryColonyRecord {
                    planet_id: JITA_PLANET_ID,
                    owner_id: char_id,
                    solar_system_id: JITA_SOLAR_SYSTEM_ID,
                    command_center_level: 3,
                    pins_count: 4,
                    last_extracted_unix: 0,
                    extracted_commodities: HashMap::new(),
                },
            );
            let net = PiColonyNetworkState {
                pins: vec![
                    PiPinState { pin_id: 101, type_id: GAS_COMMAND_CENTER_TYPE_ID, latitude: 0.1, longitude: 0.1, ..Default::default() },
                    PiPinState { pin_id: 102, type_id: GAS_EXTRACTOR_TYPE_ID, latitude: 0.2, longitude: 0.2, extractor_resource_type_id: Some(AQUEOUS_LIQUIDS_P0_TYPE), extractor_head_count: 4, extractor_head_radius: 0.03, extractor_cycle_seconds: 900, ..Default::default() },
                    PiPinState { pin_id: 103, type_id: GAS_LAUNCHPAD_TYPE_ID, latitude: 0.3, longitude: 0.3, contents: HashMap::from([(WATER_P1_TYPE, 500)]), ..Default::default() },
                    PiPinState { pin_id: 104, type_id: GAS_STORAGE_TYPE_ID, latitude: 0.4, longitude: 0.4, ..Default::default() },
                ],
                links: vec![
                    PiLinkState { source_pin_id: 101, dest_pin_id: 102, level: 1, ..Default::default() },
                    PiLinkState { source_pin_id: 102, dest_pin_id: 103, level: 1, ..Default::default() },
                    PiLinkState { source_pin_id: 103, dest_pin_id: 104, level: 1, ..Default::default() },
                ],
                routes: vec![],
                ..Default::default()
            };
            save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
        });

        // 1. [rpc:planetMgr.GetResourceData]
        // Unbound call: GetResourceData(planetID, resourceTypeID)
        let res_unbound = rpc(
            &ctx,
            &mut sess,
            "planetMgr",
            "GetResourceData",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(AQUEOUS_LIQUIDS_P0_TYPE as i64)],
        )
        .await
        .expect("GetResourceData unbound");
        // V24 clientPlanet.pyj reads exactly shData.data / .numBands / .proximity from this KeyVal.
        let PyRep::KeyVal(kv) = &res_unbound else { panic!("expected KeyVal shData, got {res_unbound:?}") };
        let mut keys: Vec<&str> = kv.keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["data", "numBands", "proximity"]);
        assert_eq!(kv.get("numBands").and_then(|v| v.as_i64()), Some(9), "skill cap 2 + 3 + 2*2");
        assert!(matches!(kv.get("data"), Some(PyRep::Bytes(b)) if b.len() == 9 * 9 * 4));
        assert_eq!(kv.get("proximity"), Some(&PyRep::None));

        // Bound moniker call: GetResourceData(resourceTypeID)
        let res_bound = rpc_bound(
            &ctx,
            &mut sess,
            "planetMgr",
            JITA_PLANET_ID,
            "GetResourceData",
            // Client-shaped `info` KeyVal from ClientPlanet.CreateResourceInfo
            &[PyRep::keyval([
                ("resourceTypeID", PyRep::Integer(AQUEOUS_LIQUIDS_P0_TYPE as i64)),
                ("updateTime", PyRep::Integer(0)),
                ("oldBand", PyRep::Integer(0)),
                ("planetology", PyRep::Integer(3)),
                ("advancedPlanetology", PyRep::Integer(2)),
                ("remoteSensing", PyRep::Integer(4)),
                ("proximity", PyRep::Integer(4)),
                ("newBand", PyRep::Integer(7)),
            ])],
        )
        .await
        .expect("GetResourceData bound");
        assert_eq!(res_bound.dict_get("numBands").and_then(|v| v.as_i64()), Some(7), "requested newBand honoured under cap");
        assert_eq!(res_bound.dict_get("proximity").and_then(|v| v.as_i64()), Some(4));
        assert!(matches!(res_bound.dict_get("data"), Some(PyRep::Bytes(b)) if b.len() == 7 * 7 * 4));

        // 2. [rpc:planetMgr.GetProgramResultInfo]
        // Unbound call: GetProgramResultInfo(planetID, pinID, typeID, heads, headRadius)
        let prog_info = rpc(
            &ctx,
            &mut sess,
            "planetMgr",
            "GetProgramResultInfo",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(102),
                PyRep::Integer(AQUEOUS_LIQUIDS_P0_TYPE as i64),
                PyRep::List(vec![]),
                PyRep::Real(0.03),
            ],
        )
        .await
        .expect("GetProgramResultInfo unbound");
        let PyRep::Tuple(prog_tuple) = &prog_info else {
            panic!("expected (qtyToDistribute, cycleTime, numCycles) 3-tuple from GetProgramResultInfo");
        };
        assert_eq!(prog_tuple.len(), 3);
        assert!(prog_tuple[0].as_i64().unwrap_or(0) > 0);
        assert!(prog_tuple[1].as_i64().unwrap_or(0) >= 300 * 10_000_000);
        assert_eq!(prog_tuple[2].as_i64(), Some(10));

        // Bound moniker call: GetProgramResultInfo(pinID, typeID, heads, headRadius)
        let prog_bound = rpc_bound(
            &ctx,
            &mut sess,
            "planetMgr",
            JITA_PLANET_ID,
            "GetProgramResultInfo",
            &[
                PyRep::Integer(102),
                PyRep::Integer(AQUEOUS_LIQUIDS_P0_TYPE as i64),
                PyRep::List(vec![]),
                PyRep::Real(0.03),
            ],
        )
        .await
        .expect("GetProgramResultInfo bound");
        assert_eq!(prog_bound, prog_info);

        // 3. [rpc:planetMgr.CancelInstallProgram]
        // Unbound call: CancelInstallProgram(planetID, pinID)
        let cancel_res = rpc(
            &ctx,
            &mut sess,
            "planetMgr",
            "CancelInstallProgram",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(102)],
        )
        .await
        .expect("CancelInstallProgram unbound");
        assert_eq!(cancel_res.as_bool(), Some(true));

        // Verify program was cleared on extractor pin 102
        ctx.db.with_snapshot(|snap| {
            let net = load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
            let pin = net.pins.iter().find(|p| p.pin_id == 102).expect("pin 102");
            assert!(pin.extractor_resource_type_id.is_none(), "extractor program cleared");
        });

        // Re-install program and verify bound call: CancelInstallProgram(pinID)
        ctx.db.with_mut_snapshot(|snap| {
            let mut net = load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
            if let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == 102) {
                pin.extractor_resource_type_id = Some(AQUEOUS_LIQUIDS_P0_TYPE);
            }
            save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
        });
        let cancel_bound = rpc_bound(
            &ctx,
            &mut sess,
            "planetMgr",
            JITA_PLANET_ID,
            "CancelInstallProgram",
            &[PyRep::Integer(102)],
        )
        .await
        .expect("CancelInstallProgram bound");
        assert_eq!(cancel_bound.as_bool(), Some(true));

        // 4. [rpc:planetMgr.GetMyLaunchesDetails] and [rpc:planetMgr.DeleteLaunch]
        // Perform orbital launch to generate launch container
        let launch_rep = rpc(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserLaunchCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(103),
                PyRep::Integer(WATER_P1_TYPE as i64),
                PyRep::Integer(200),
            ],
        )
        .await
        .expect("UserLaunchCommodities");
        assert_eq!(get_kv_i64(&launch_rep, "routedQuantity"), Some(200));

        // Query launches details
        let launches_rep = rpc(&ctx, &mut sess, "planetMgr", "GetMyLaunchesDetails", &[])
            .await
            .expect("GetMyLaunchesDetails");
        let PyRep::List(launches) = launches_rep else {
            panic!("expected launches list");
        };
        assert!(!launches.is_empty(), "expected at least 1 launch");
        let launch_obj = &launches[0];
        let launch_id = get_kv_i64(launch_obj, "launchID").expect("launchID present");
        assert_eq!(get_kv_i64(launch_obj, "planetID"), Some(JITA_PLANET_ID));
        assert_eq!(get_kv_i64(launch_obj, "solarSystemID"), Some(JITA_SOLAR_SYSTEM_ID as i64));
        assert_eq!(get_kv_i64(launch_obj, "typeID"), Some(WATER_P1_TYPE as i64));
        assert_eq!(get_kv_i64(launch_obj, "quantity"), Some(200));

        // Verify DeleteLaunch rejects invalid launches: non-existent, foreign, and non-planetary
        let err_not_found = rpc(&ctx, &mut sess, "planetMgr", "DeleteLaunch", &[PyRep::Integer(999_999)])
            .await
            .expect_err("non-existent launch ID must fail");
        assert_eq!(err_not_found.code, "LaunchNotFound");

        let foreign_launch_id = 888_001;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(foreign_launch_id, eve_db::InventoryItemRecord {
                item_id: foreign_launch_id, type_id: WATER_P1_TYPE, owner_id: 90_000_002, location_id: JITA_PLANET_ID,
                flag: eve_db::InventoryFlag::None, quantity: 100, singleton: false, version: 1, ..Default::default()
            });
        });
        let err_foreign = rpc(&ctx, &mut sess, "planetMgr", "DeleteLaunch", &[PyRep::Integer(foreign_launch_id)])
            .await
            .expect_err("foreign launch ID must fail");
        assert_eq!(err_foreign.code, "LaunchAccessDenied");

        let non_launch_id = 888_002;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(non_launch_id, eve_db::InventoryItemRecord {
                item_id: non_launch_id, type_id: WATER_P1_TYPE, owner_id: char_id, location_id: 60_003_760,
                flag: eve_db::InventoryFlag::None, quantity: 100, singleton: false, version: 1, ..Default::default()
            });
        });
        let err_non_launch = rpc(&ctx, &mut sess, "planetMgr", "DeleteLaunch", &[PyRep::Integer(non_launch_id)])
            .await
            .expect_err("non-launch item must fail");
        assert_eq!(err_non_launch.code, "InvalidLaunchID");

        // Delete launch container
        let del_rep = rpc(
            &ctx,
            &mut sess,
            "planetMgr",
            "DeleteLaunch",
            &[PyRep::Integer(launch_id)],
        )
        .await
        .expect("DeleteLaunch");
        assert_eq!(del_rep.as_bool(), Some(true));

        // Verify deleted from items
        let launch_exists = ctx.db.with_snapshot(|snap| snap.items.contains_key(&launch_id));
        assert!(!launch_exists, "launch container was removed");

        // 5. [rpc:planetMgr.UserAbandonPlanet]
        // Unbound call: UserAbandonPlanet(planetID)
        let abandon_rep = rpc(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserAbandonPlanet",
            &[PyRep::Integer(JITA_PLANET_ID)],
        )
        .await
        .expect("UserAbandonPlanet unbound");
        assert_eq!(abandon_rep.as_bool(), Some(true));

        // Verify colony was removed
        let col_exists = ctx.db.with_snapshot(|snap| {
            snap.planetary_colonies.contains_key(&(char_id, JITA_PLANET_ID))
        });
        assert!(!col_exists, "colony on JITA_PLANET_ID was abandoned");

        // Seed another colony on BARREN_PLANET_ID and test bound/0-arg call
        ctx.db.with_mut_snapshot(|snap| {
            snap.planetary_colonies.insert(
                (char_id, BARREN_PLANET_ID),
                eve_db::PlanetaryColonyRecord {
                    planet_id: BARREN_PLANET_ID,
                    owner_id: char_id,
                    solar_system_id: JITA_SOLAR_SYSTEM_ID,
                    command_center_level: 1,
                    pins_count: 1,
                    last_extracted_unix: 0,
                    extracted_commodities: HashMap::new(),
                },
            );
        });
        let abandon_bound = rpc_bound(
            &ctx,
            &mut sess,
            "planetMgr",
            BARREN_PLANET_ID,
            "UserAbandonPlanet",
            &[],
        )
        .await
        .expect("UserAbandonPlanet bound");
        assert_eq!(abandon_bound.as_bool(), Some(true));
        let barren_exists = ctx.db.with_snapshot(|snap| {
            snap.planetary_colonies.contains_key(&(char_id, BARREN_PLANET_ID))
        });
        assert!(!barren_exists, "colony on BARREN_PLANET_ID was abandoned");
    }

    /// Integration test for BF-PI-SIMULATION-UNLINKED-ROUTE-AND-CAPACITY-OVERFLOW-01 behavioral verification:
    /// 1. UserUpdateNetwork link deletion prunes dependent routes.
    /// 2. advance_colony_simulation rejects unlinked routes (missing hops).
    /// 3. advance_colony_simulation clamps/rejects deposition when destination storage is at capacity.
    #[tokio::test]
    async fn test_p1_simulation_unlinked_route_and_capacity_overflow() {
        let ctx = ServerContext::new();
        let char_id = 90_000_001;
        let session_id = 702;
        let mut sess = setup_pilot(&ctx, session_id, char_id).await;

        // Part 1: UserUpdateNetwork link removal prunes dependent routes
        ctx.db.with_mut_snapshot(|snap| {
            grant_pi_skills(snap, char_id);
            snap.planetary_colonies.insert(
                (char_id, JITA_PLANET_ID),
                eve_db::PlanetaryColonyRecord {
                    planet_id: JITA_PLANET_ID,
                    owner_id: char_id,
                    solar_system_id: JITA_SOLAR_SYSTEM_ID,
                    command_center_level: 3,
                    pins_count: 4,
                    last_extracted_unix: 0,
                    extracted_commodities: HashMap::new(),
                },
            );
            let net = PiColonyNetworkState {
                pins: vec![
                    PiPinState { pin_id: 201, type_id: GAS_COMMAND_CENTER_TYPE_ID, ..Default::default() },
                    PiPinState { pin_id: 202, type_id: GAS_STORAGE_TYPE_ID, ..Default::default() },
                    PiPinState { pin_id: 203, type_id: GAS_LAUNCHPAD_TYPE_ID, ..Default::default() },
                    PiPinState { pin_id: 204, type_id: GAS_BASIC_FACILITY_TYPE_ID, ..Default::default() },
                ],
                links: vec![
                    PiLinkState { source_pin_id: 201, dest_pin_id: 202, level: 1, ..Default::default() },
                    PiLinkState { source_pin_id: 202, dest_pin_id: 203, level: 1, ..Default::default() },
                ],
                routes: vec![
                    // Route 1: uses link 201-202 only
                    PiRouteState { route_id: 1, path: vec![201, 202], commodity_type_id: WATER_P1_TYPE, quantity: 10 },
                    // Route 2: uses link 202-203 only
                    PiRouteState { route_id: 2, path: vec![202, 203], commodity_type_id: WATER_P1_TYPE, quantity: 20 },
                    // Route 3: uses both 201-202 and 202-203
                    PiRouteState { route_id: 3, path: vec![201, 202, 203], commodity_type_id: WATER_P1_TYPE, quantity: 30 },
                ],
                ..Default::default()
            };
            save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
        });

        // Execute UserUpdateNetwork command 4 to remove link between 202 and 203
        let cmd_remove_link = PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(202), PyRep::Integer(203)]);
        let _net_rep = rpc(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![cmd_remove_link])],
        )
        .await
        .expect("UserUpdateNetwork remove link");

        // Route 1 must remain, while Route 2 and Route 3 must be pruned
        ctx.db.with_snapshot(|snap| {
            let net = load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
            assert_eq!(net.links.len(), 1, "link between 202 and 203 was removed");
            assert_eq!(net.routes.len(), 1, "only unaffected route remains");
            assert_eq!(net.routes[0].route_id, 1, "Route 1 was retained");
        });

        // Part 2: advance_colony_simulation rejects unlinked routes
        let mut unlinked_net = PiColonyNetworkState {
            pins: vec![
                PiPinState { pin_id: 301, type_id: GAS_EXTRACTOR_TYPE_ID, extractor_resource_type_id: Some(AQUEOUS_LIQUIDS_P0_TYPE), extractor_head_count: 2, extractor_head_radius: 0.02, extractor_cycle_seconds: 900, ..Default::default() },
                PiPinState { pin_id: 302, type_id: GAS_STORAGE_TYPE_ID, contents: HashMap::new(), ..Default::default() },
            ],
            links: vec![],
            routes: vec![
                PiRouteState { route_id: 1, path: vec![301, 302], commodity_type_id: AQUEOUS_LIQUIDS_P0_TYPE, quantity: 100 },
            ],
            ..Default::default()
        };
        let mut unlinked_col = eve_db::PlanetaryColonyRecord {
            planet_id: JITA_PLANET_ID,
            owner_id: char_id,
            solar_system_id: JITA_SOLAR_SYSTEM_ID,
            command_center_level: 3,
            pins_count: 2,
            last_extracted_unix: 0,
            extracted_commodities: HashMap::new(),
        };

        let (_ext, _proc) = advance_colony_simulation(&ctx, &mut unlinked_col, &mut unlinked_net, 900, 900);
        // Because the route between 301 and 302 has no valid link path, target pin 302 remains unpopulated
        let pin_302_contents = unlinked_net.pins[1].contents.get(&AQUEOUS_LIQUIDS_P0_TYPE).copied().unwrap_or(0);
        assert_eq!(pin_302_contents, 0, "destination pin without valid link path does not receive commodities");

        // Part 3: advance_colony_simulation respects storage capacity and clamps overflow
        // GAS_STORAGE_TYPE has 12,000 m3 capacity. Aqueous Liquids P0 has volume 0.01 m3 / unit.
        // 1,199,999 units * 0.01 = 11,999.99 m3 -> only ~0.01 m3 remaining space (room for 1 unit).
        let mut cap_net = PiColonyNetworkState {
            pins: vec![
                PiPinState { pin_id: 401, type_id: GAS_EXTRACTOR_TYPE_ID, extractor_resource_type_id: Some(AQUEOUS_LIQUIDS_P0_TYPE), extractor_head_count: 1, extractor_head_radius: 0.02, extractor_cycle_seconds: 900, ..Default::default() },
                PiPinState { pin_id: 402, type_id: GAS_STORAGE_TYPE_ID, contents: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 1_199_999)]), ..Default::default() },
            ],
            links: vec![
                PiLinkState { source_pin_id: 401, dest_pin_id: 402, level: 1, ..Default::default() },
            ],
            routes: vec![
                PiRouteState { route_id: 1, path: vec![401, 402], commodity_type_id: AQUEOUS_LIQUIDS_P0_TYPE, quantity: 1_000 },
            ],
            ..Default::default()
        };
        let mut cap_col = eve_db::PlanetaryColonyRecord {
            planet_id: JITA_PLANET_ID,
            owner_id: char_id,
            solar_system_id: JITA_SOLAR_SYSTEM_ID,
            command_center_level: 3,
            pins_count: 2,
            last_extracted_unix: 0,
            extracted_commodities: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 1_199_999)]),
        };

        let (ext_added, _) = advance_colony_simulation(&ctx, &mut cap_col, &mut cap_net, 900, 1800);
        // The extraction would ordinarily produce ~1,000 units, but available space fits at most 1 unit!
        assert_eq!(ext_added, 1, "deposition was clamped to remaining volume capacity: got {ext_added}");
        let total_stored = cap_net.pins[1].contents.get(&AQUEOUS_LIQUIDS_P0_TYPE).copied().unwrap_or(0);
        let vol_stored = (total_stored as f64) * 0.01;
        assert!(vol_stored <= 12_000.0 + 1e-3, "stored volume does not exceed 12,000 m3");

        // Part 4: factory simulation halts and conserves inputs when destination storage is full
        let water_schem = ctx
            .sde
            .planet_schematics
            .iter()
            .find(|(_, s)| s.types.iter().any(|t| !t.is_input && t.type_id == WATER_P1_TYPE))
            .map(|(&id, _)| id)
            .expect("Water P1 schematic in SDE");
        let mut fac_net = PiColonyNetworkState {
            pins: vec![
                PiPinState { pin_id: 501, type_id: GAS_STORAGE_TYPE_ID, contents: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 3_000)]), ..Default::default() },
                PiPinState { pin_id: 502, type_id: GAS_BASIC_FACILITY_TYPE_ID, schematic_id: Some(water_schem), ..Default::default() },
                PiPinState { pin_id: 503, type_id: GAS_STORAGE_TYPE_ID, contents: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 1_199_999)]), ..Default::default() },
            ],
            links: vec![
                PiLinkState { source_pin_id: 501, dest_pin_id: 502, level: 1, ..Default::default() },
                PiLinkState { source_pin_id: 502, dest_pin_id: 503, level: 1, ..Default::default() },
            ],
            routes: vec![
                PiRouteState { route_id: 1, path: vec![501, 502], commodity_type_id: AQUEOUS_LIQUIDS_P0_TYPE, quantity: 3_000 },
                PiRouteState { route_id: 2, path: vec![502, 503], commodity_type_id: WATER_P1_TYPE, quantity: 20 },
            ],
            ..Default::default()
        };
        let mut fac_col = eve_db::PlanetaryColonyRecord {
            planet_id: JITA_PLANET_ID,
            owner_id: char_id,
            solar_system_id: JITA_SOLAR_SYSTEM_ID,
            command_center_level: 3,
            pins_count: 3,
            last_extracted_unix: 0,
            extracted_commodities: HashMap::from([(AQUEOUS_LIQUIDS_P0_TYPE, 1_202_999)]),
        };

        // Destination storage full: factory cycle halts; input commodities are not deducted
        let (_, proc_1) = advance_colony_simulation(&ctx, &mut fac_col, &mut fac_net, 0, 1800);
        assert_eq!(proc_1, 0, "no factory output produced when destination pin is full");
        assert_eq!(fac_net.pins[0].contents.get(&AQUEOUS_LIQUIDS_P0_TYPE).copied(), Some(3_000), "inputs preserved");
        assert_eq!(fac_col.extracted_commodities.get(&AQUEOUS_LIQUIDS_P0_TYPE).copied(), Some(1_202_999), "colony totals preserved");

        // Clear destination storage space: factory runs and consumes inputs
        fac_net.pins[2].contents.clear();
        let (_, proc_2) = advance_colony_simulation(&ctx, &mut fac_col, &mut fac_net, 1800, 3600);
        assert_eq!(proc_2, 20, "factory output produced after freeing destination space");
        assert_eq!(fac_net.pins[0].contents.get(&AQUEOUS_LIQUIDS_P0_TYPE).copied().unwrap_or(0), 0, "inputs deducted");
        assert_eq!(fac_net.pins[2].contents.get(&WATER_P1_TYPE).copied(), Some(20), "output deposited to destination pin");
        assert_eq!(fac_col.extracted_commodities.get(&AQUEOUS_LIQUIDS_P0_TYPE).copied(), Some(1_199_999), "colony totals updated");
    }

    /// Integration test for BF-PI-LAUNCH-DOUBLE-DEDUCTION-AND-PIN-FALLBACK-02 behavioral verification:
    /// 1. UserLaunchCommodities rejects launching from non-storage pin types.
    /// 2. UserLaunchCommodities does not double-deduct from col.extracted_commodities.
    #[tokio::test]
    async fn test_p1_launch_non_storage_pin_and_no_double_deduction() {
        let ctx = ServerContext::new();
        let char_id = 90_000_001;
        let session_id = 703;
        let mut sess = setup_pilot(&ctx, session_id, char_id).await;

        ctx.db.with_mut_snapshot(|snap| {
            grant_pi_skills(snap, char_id);
            snap.planetary_colonies.insert(
                (char_id, JITA_PLANET_ID),
                eve_db::PlanetaryColonyRecord {
                    planet_id: JITA_PLANET_ID,
                    owner_id: char_id,
                    solar_system_id: JITA_SOLAR_SYSTEM_ID,
                    command_center_level: 3,
                    pins_count: 3,
                    last_extracted_unix: 0,
                    extracted_commodities: HashMap::from([(WATER_P1_TYPE, 600)]),
                },
            );
            let net = PiColonyNetworkState {
                pins: vec![
                    PiPinState { pin_id: 501, type_id: GAS_EXTRACTOR_TYPE_ID, contents: HashMap::from([(WATER_P1_TYPE, 100)]), ..Default::default() },
                    PiPinState { pin_id: 502, type_id: GAS_BASIC_FACILITY_TYPE_ID, contents: HashMap::from([(WATER_P1_TYPE, 100)]), ..Default::default() },
                    PiPinState { pin_id: 503, type_id: GAS_LAUNCHPAD_TYPE_ID, contents: HashMap::from([(WATER_P1_TYPE, 400)]), ..Default::default() },
                ],
                links: vec![
                    PiLinkState { source_pin_id: 501, dest_pin_id: 503, level: 1, ..Default::default() },
                    PiLinkState { source_pin_id: 502, dest_pin_id: 503, level: 1, ..Default::default() },
                ],
                routes: vec![],
                ..Default::default()
            };
            save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
        });

        // 1. Explicitly launch from Extractor pin 501 -> Expect InvalidPinTypeForLaunch
        let ext_launch_err = rpc(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserLaunchCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(501),
                PyRep::Integer(WATER_P1_TYPE as i64),
                PyRep::Integer(50),
            ],
        )
        .await
        .expect_err("launch from extractor must be rejected");
        assert_eq!(ext_launch_err.code, "InvalidPinTypeForLaunch");

        // 2. Explicitly launch from Processor pin 502 -> Expect InvalidPinTypeForLaunch
        let proc_launch_err = rpc(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserLaunchCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(502),
                PyRep::Integer(WATER_P1_TYPE as i64),
                PyRep::Integer(50),
            ],
        )
        .await
        .expect_err("launch from processor must be rejected");
        assert_eq!(proc_launch_err.code, "InvalidPinTypeForLaunch");

        // 3. Launch from valid Launchpad pin 503 -> verify no double deduction
        let launch_rep = rpc(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserLaunchCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(503),
                PyRep::Integer(WATER_P1_TYPE as i64),
                PyRep::Integer(150),
            ],
        )
        .await
        .expect("launch from launchpad succeeded");
        assert_eq!(get_kv_i64(&launch_rep, "routedQuantity"), Some(150));

        // Verify pin contents was reduced by 150 (400 - 150 = 250)
        ctx.db.with_snapshot(|snap| {
            let net = load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
            let lp = net.pins.iter().find(|p| p.pin_id == 503).expect("lp pin 503");
            assert_eq!(lp.contents.get(&WATER_P1_TYPE).copied(), Some(250));

            // Verify col.extracted_commodities was NOT double-subtracted
            let col = snap.planetary_colonies.get(&(char_id, JITA_PLANET_ID)).expect("colony");
            let remaining_extracted = col.extracted_commodities.get(&WATER_P1_TYPE).copied().unwrap_or(0);
            // Previously, the bug subtracted 150 twice or reduced col.extracted_commodities redundantly.
            assert_eq!(remaining_extracted, 450, "col.extracted_commodities was preserved without double deduction");
        });
    }
}

pub(crate) mod pi_multi_colony_bound_moniker {
    // Integration tests for multi-colony bound moniker isolation and Planet Manager RPCs:
    // - Bound moniker planet routing (verifying Planet B never mutates or affects Planet A)
    // - rpc:planetMgr.UserAbandonPlanet (unbound 0-arg rejection and bound 0-arg isolation)
    // - rpc:planetMgr.CancelInstallProgram (bound routing and PinNotFound error without mutating net.pins[0])
    // - rpc:planetMgr.GetProgramResultInfo (bound moniker routing, preview heads overlap, and non-mutation)
    // - rpc:planetMgr.GetResourceData (bound moniker routing without arbitrary Jita IV fallback)

    use crate::common;
    use eve_db::{IskCents, TrainedSkillRecord, TypeId};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::industry_pi::*;
    use std::collections::HashMap;

    const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
    const PLANET_A_ID: i64 = 40_009_082;
    const PLANET_B_ID: i64 = 40_009_077;

    const GAS_COMMAND_CENTER_TYPE_ID: TypeId = 2534;
    const GAS_EXTRACTOR_TYPE_ID: TypeId = 2848;
    const AQUEOUS_LIQUIDS_P0_TYPE: TypeId = 2268;

    async fn setup_pilot(ctx: &ServerContext, session_id: u64, char_id: i64) -> SessionState {
        let mut sess = SessionState::new(session_id);
        common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
        ctx.dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SelectCharacterID");
        sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
        sess.station_id = None;
        sess.structure_id = None;
        sess
    }

    fn grant_pi_skills(snap: &mut eve_db::WorldStateSnapshot, char_id: i64) {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(100_000_000);
            ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
            ch.station_id = None;
            ch.structure_id = None;
        }
        let sk = snap.skills.entry(char_id).or_default();
        for (stid, lvl) in [(2505, 5), (2495, 4), (13279, 4), (2406, 3), (2403, 2)] {
            sk.insert(stid, TrainedSkillRecord { type_id: stid, level: lvl, skill_points: 256_000.0 });
        }
    }

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
    }

    /// Multi-colony integration test verifying bound moniker routing isolation:
    /// Operations executed on Planet B moniker must NEVER touch or affect Planet A.
    #[tokio::test]
    async fn test_pi_multi_colony_bound_moniker_isolation() {
        // parity: rpc:planetMgr.UserAbandonPlanet, rpc:planetMgr.CancelInstallProgram, rpc:planetMgr.GetProgramResultInfo, rpc:planetMgr.GetResourceData
        let ctx = ServerContext::new();
        let char_id = 90_000_001;
        let session_id = 705;
        let mut sess = setup_pilot(&ctx, session_id, char_id).await;

        // Seed pilot skills and two independent colonies on Planet A and Planet B
        ctx.db.with_mut_snapshot(|snap| {
            grant_pi_skills(snap, char_id);

            // Planet A: CommandCenter level 3, Extractor pin 102 installed
            snap.planetary_colonies.insert(
                (char_id, PLANET_A_ID),
                eve_db::PlanetaryColonyRecord {
                    planet_id: PLANET_A_ID,
                    owner_id: char_id,
                    solar_system_id: JITA_SOLAR_SYSTEM_ID,
                    command_center_level: 3,
                    pins_count: 2,
                    last_extracted_unix: 0,
                    extracted_commodities: HashMap::new(),
                },
            );
            let net_a = PiColonyNetworkState {
                pins: vec![
                    PiPinState { pin_id: 101, type_id: GAS_COMMAND_CENTER_TYPE_ID, latitude: 0.1, longitude: 0.1, ..Default::default() },
                    PiPinState { pin_id: 102, type_id: GAS_EXTRACTOR_TYPE_ID, latitude: 0.2, longitude: 0.2, extractor_resource_type_id: Some(AQUEOUS_LIQUIDS_P0_TYPE), extractor_head_count: 2, extractor_head_radius: 0.02, extractor_cycle_seconds: 900, ..Default::default() },
                ],
                links: vec![
                    PiLinkState { source_pin_id: 101, dest_pin_id: 102, level: 1, ..Default::default() },
                ],
                routes: vec![],
                ..Default::default()
            };
            save_pi_network(snap, char_id, PLANET_A_ID, &net_a);

            // Planet B: CommandCenter level 2, Extractor pin 202 installed
            snap.planetary_colonies.insert(
                (char_id, PLANET_B_ID),
                eve_db::PlanetaryColonyRecord {
                    planet_id: PLANET_B_ID,
                    owner_id: char_id,
                    solar_system_id: JITA_SOLAR_SYSTEM_ID,
                    command_center_level: 2,
                    pins_count: 2,
                    last_extracted_unix: 0,
                    extracted_commodities: HashMap::new(),
                },
            );
            let net_b = PiColonyNetworkState {
                pins: vec![
                    PiPinState { pin_id: 201, type_id: GAS_COMMAND_CENTER_TYPE_ID, latitude: -0.1, longitude: -0.1, ..Default::default() },
                    PiPinState { pin_id: 202, type_id: GAS_EXTRACTOR_TYPE_ID, latitude: -0.2, longitude: -0.2, extractor_resource_type_id: Some(AQUEOUS_LIQUIDS_P0_TYPE), extractor_head_count: 2, extractor_head_radius: 0.02, extractor_cycle_seconds: 900, ..Default::default() },
                ],
                links: vec![
                    PiLinkState { source_pin_id: 201, dest_pin_id: 202, level: 1, ..Default::default() },
                ],
                routes: vec![],
                ..Default::default()
            };
            save_pi_network(snap, char_id, PLANET_B_ID, &net_b);
        });

        // 1. Unbound UserAbandonPlanet with 0 arguments must fail with InvalidPlanetID without deleting either colony
        let unbound_abandon_err = rpc(&ctx, &mut sess, "planetMgr", "UserAbandonPlanet", &[])
            .await
            .expect_err("unbound UserAbandonPlanet with 0 args must fail");
        assert_eq!(unbound_abandon_err.code, "InvalidPlanetID");
        ctx.db.with_snapshot(|snap| {
            assert!(snap.planetary_colonies.contains_key(&(char_id, PLANET_A_ID)), "Planet A colony preserved");
            assert!(snap.planetary_colonies.contains_key(&(char_id, PLANET_B_ID)), "Planet B colony preserved");
        });

        // 2. Bind planet moniker specifically for Planet B
        let bound_b = ctx.bind_object(&mut sess, "planetMgr", PLANET_B_ID).expect("bind Planet B");

        // 3. CancelInstallProgram on Planet B moniker with non-existent pin ID 999
        // Must return PinNotFound and must NOT mutate net.pins[0] or any pin on Planet B or Planet A
        let cancel_err = ctx.dispatch_rpc(
            &mut sess,
            "planetMgr",
            Some(bound_b),
            "CancelInstallProgram",
            &[PyRep::Integer(999)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("CancelInstallProgram with invalid pinID must fail");
        assert_eq!(cancel_err.code, "PinNotFound");

        ctx.db.with_snapshot(|snap| {
            let net_b = load_pi_network(snap, char_id, PLANET_B_ID, 2);
            assert_eq!(net_b.pins.len(), 2);
            assert_eq!(net_b.pins[0].pin_id, 201, "Planet B CC pin 0 untouched");
            assert_eq!(net_b.pins[1].extractor_resource_type_id, Some(AQUEOUS_LIQUIDS_P0_TYPE), "Planet B pin 202 program untouched");

            let net_a = load_pi_network(snap, char_id, PLANET_A_ID, 3);
            assert_eq!(net_a.pins.len(), 2);
            assert_eq!(net_a.pins[0].pin_id, 101, "Planet A CC pin 0 untouched");
            assert_eq!(net_a.pins[1].extractor_resource_type_id, Some(AQUEOUS_LIQUIDS_P0_TYPE), "Planet A pin 102 program untouched");
        });

        // 4. CancelInstallProgram on Planet B moniker for pin 202 clears program on Planet B, Planet A remains untouched
        let cancel_ok = ctx.dispatch_rpc(
            &mut sess,
            "planetMgr",
            Some(bound_b),
            "CancelInstallProgram",
            &[PyRep::Integer(202)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CancelInstallProgram on pin 202");
        assert_eq!(cancel_ok.as_bool(), Some(true));

        ctx.db.with_snapshot(|snap| {
            let net_b = load_pi_network(snap, char_id, PLANET_B_ID, 2);
            assert!(net_b.pins[1].extractor_resource_type_id.is_none(), "Planet B pin 202 program cleared");

            let net_a = load_pi_network(snap, char_id, PLANET_A_ID, 3);
            assert_eq!(net_a.pins[1].extractor_resource_type_id, Some(AQUEOUS_LIQUIDS_P0_TYPE), "Planet A pin 102 program STILL intact");
        });

        // 5. GetProgramResultInfo on Planet B moniker: verifies bound planet routing and preview heads calculation
        // Non-overlapping preview heads: 2 heads separated by 1.0 radian
        let non_overlap_heads = PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.0), PyRep::Real(0.0)]),
            PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Real(1.0), PyRep::Real(1.0)]),
        ]);
        let prog_info_1 = ctx.dispatch_rpc(
            &mut sess,
            "planetMgr",
            Some(bound_b),
            "GetProgramResultInfo",
            &[
                PyRep::Integer(202),
                PyRep::Integer(AQUEOUS_LIQUIDS_P0_TYPE as i64),
                non_overlap_heads,
                PyRep::Real(0.02),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetProgramResultInfo non-overlapping");
        let PyRep::Tuple(tuple_1) = &prog_info_1 else {
            panic!("expected (qtyToDistribute, cycleTime, numCycles) 3-tuple from GetProgramResultInfo");
        };
        assert_eq!(tuple_1.len(), 3);
        assert_eq!(tuple_1[0].as_i64(), Some(2000), "2 non-overlapping heads yield 2,000 units initial cycle");
        assert!(tuple_1[1].as_i64().unwrap_or(0) >= 300 * 10_000_000);
        assert_eq!(tuple_1[2].as_i64(), Some(10));

        // Overlapping preview heads: 2 heads placed at identical coordinates
        let overlap_heads = PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.0), PyRep::Real(0.0)]),
            PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ]);
        let prog_info_2 = ctx.dispatch_rpc(
            &mut sess,
            "planetMgr",
            Some(bound_b),
            "GetProgramResultInfo",
            &[
                PyRep::Integer(202),
                PyRep::Integer(AQUEOUS_LIQUIDS_P0_TYPE as i64),
                overlap_heads,
                PyRep::Real(0.02),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetProgramResultInfo overlapping");
        let PyRep::Tuple(tuple_2) = &prog_info_2 else {
            panic!("expected (qtyToDistribute, cycleTime, numCycles) 3-tuple from GetProgramResultInfo");
        };
        assert_eq!(tuple_2.len(), 3);
        assert!(tuple_2[0].as_i64().unwrap_or(0) < 2000, "overlapping preview heads reduce extraction output");

        // Verify preview calculation did NOT mutate pin state on either planet
        ctx.db.with_snapshot(|snap| {
            let net_b = load_pi_network(snap, char_id, PLANET_B_ID, 2);
            assert_eq!(net_b.pins[1].extractor_head_count, 0, "Planet B head count was cleared by CancelInstallProgram and unmutated by preview");
            let net_a = load_pi_network(snap, char_id, PLANET_A_ID, 3);
            assert_eq!(net_a.pins[1].extractor_head_count, 2, "Planet A head count unmutated and intact at 2");
        });

        // 6. GetResourceData on Planet B bound moniker (omitting planetID argument)
        let res_data = ctx.dispatch_rpc(
            &mut sess,
            "planetMgr",
            Some(bound_b),
            "GetResourceData",
            &[PyRep::keyval([
                ("resourceTypeID", PyRep::Integer(AQUEOUS_LIQUIDS_P0_TYPE as i64)),
                ("proximity", PyRep::Integer(0)),
                ("newBand", PyRep::Integer(15)),
            ])],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetResourceData on bound Planet B moniker");
        // newBand 15 exceeds the pilot's skill cap (2 + 3 + 2*2 = 9) and is clamped server-side
        assert_eq!(res_data.dict_get("numBands").and_then(|v| v.as_i64()), Some(9));
        assert_eq!(res_data.dict_get("proximity").and_then(|v| v.as_i64()), Some(0));
        assert!(matches!(res_data.dict_get("data"), Some(PyRep::Bytes(b)) if b.len() == 9 * 9 * 4));

        // 7. UserAbandonPlanet on Planet B moniker with 0 arguments:
        // Deletes Planet B colony, but leaves Planet A colony completely intact!
        let abandon_b = ctx.dispatch_rpc(
            &mut sess,
            "planetMgr",
            Some(bound_b),
            "UserAbandonPlanet",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("UserAbandonPlanet on bound Planet B");
        assert_eq!(abandon_b.as_bool(), Some(true));

        ctx.db.with_snapshot(|snap| {
            assert!(!snap.planetary_colonies.contains_key(&(char_id, PLANET_B_ID)), "Planet B colony deleted");
            let col_a = snap.planetary_colonies.get(&(char_id, PLANET_A_ID)).expect("Planet A colony STILL exists");
            assert_eq!(col_a.command_center_level, 3, "Planet A command center level preserved");
            let net_a = load_pi_network(snap, char_id, PLANET_A_ID, 3);
            assert_eq!(net_a.pins.len(), 2, "Planet A network pins fully preserved");
            assert_eq!(net_a.pins[1].extractor_resource_type_id, Some(AQUEOUS_LIQUIDS_P0_TYPE), "Planet A extractor program preserved");
        });
    }
}
