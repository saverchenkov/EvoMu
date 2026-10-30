//! Integration tests for Planet Manager (`planetMgr`) open RPCs and PI storage/simulation gap remediations:
//! - rpc:planetMgr.GetPin
//! - rpc:planetMgr.GetExtractorsForPlanet
//! - rpc:planetMgr.GetCommandPinsForPlanet
//! - rpc:planetMgr.RevertChanges
//! - Remediation of BF-PI-IMPORT-PIN-TYPE-HARDCODE-AND-DOUBLE-CREDIT-01
//! - Remediation of BF-PI-SIMULATION-SPLIT-INPUT-MINT-02

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
async fn test_parity_gap_remediations() {
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
