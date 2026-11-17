//! Complete tests for Planetary Interaction expedited commodity transfer (`feat:pi.expedited-transfer`, `rpc:planetMgr.UserTransferCommodities`).
//!
//! Validates pin lookup, storage quantity availability, link path reachability,
//! destination capacity bounds, atomic storage quantity updates, and transfer cooldowns.

use crate::common::{self, SessionBorrowWireTransport};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_testkit::client_harness::{ClientErrorResponse, ClientHarness, ContractValidationMode};
use std::collections::HashMap;

const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const JITA_PLANET_ID: i64 = 40_009_082;

const COMMAND_CENTER_UPGRADES_SKILL: TypeId = 2505;
const INTERPLANETARY_CONSOLIDATION_SKILL: TypeId = 2495;

const CC_PIN_TYPE_ID: TypeId = 2534; // Gas Command Center (capacity 500 m3)
const EXTRACTOR_PIN_TYPE_ID: TypeId = 2848;
const STORAGE_PIN_TYPE_ID: TypeId = 2536; // Gas Storage Facility (capacity 12,000 m3)
const LAUNCHPAD_PIN_TYPE_ID: TypeId = 2543; // Gas Launchpad (capacity 10,000 m3)
const WATER_P1_TYPE_ID: TypeId = 2389; // unit volume 0.19 m3

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, ClientErrorResponse> {
    let kw = HashMap::new();
    common::borrow_wire_harness(ctx, sess)
        .call(service, None, method, args, &kw)
        .await
}

async fn setup_pilot(ctx: &ServerContext, session_id: u64, username: &str, password: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

fn grant_pi_skills(snap: &mut eve_db::WorldStateSnapshot, char_id: i64) {
    if let Some(ch) = snap.characters.get_mut(&char_id) {
        ch.balance = IskCents::from_isk(100_000_000);
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
    }
    let sk = snap.skills.entry(char_id).or_default();
    sk.insert(COMMAND_CENTER_UPGRADES_SKILL, TrainedSkillRecord { type_id: COMMAND_CENTER_UPGRADES_SKILL, level: 5, skill_points: 256_000.0 });
    sk.insert(INTERPLANETARY_CONSOLIDATION_SKILL, TrainedSkillRecord { type_id: INTERPLANETARY_CONSOLIDATION_SKILL, level: 4, skill_points: 256_000.0 });
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

fn get_i64(rep: &PyRep, key: &str) -> Option<i64> {
    rep.dict_get(key).and_then(|v| v.as_i64())
}

#[tokio::test]
async fn test_planetary_interaction_expedited_transfer() {
    // parity: feat:pi.expedited-transfer, rpc:planetMgr.UserTransferCommodities
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 1104, "demo", "demo-secret", char_id).await;
    let ship_id = sess.ship_id.expect("Active ship");
    ctx.db.with_mut_snapshot(|snap| {
        grant_pi_skills(snap, char_id);
        seed_cargo_item(snap, CC_PIN_TYPE_ID, char_id, ship_id, 1);
    });

    // Deploy colony
    let col_dep = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1001), PyRep::Integer(CC_PIN_TYPE_ID as i64), PyRep::Real(0.10), PyRep::Real(0.10)]),
                PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(1001), PyRep::Integer(3)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1002), PyRep::Integer(LAUNCHPAD_PIN_TYPE_ID as i64), PyRep::Real(0.11), PyRep::Real(0.11)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1003), PyRep::Integer(STORAGE_PIN_TYPE_ID as i64), PyRep::Real(0.12), PyRep::Real(0.12)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1004), PyRep::Integer(EXTRACTOR_PIN_TYPE_ID as i64), PyRep::Real(0.13), PyRep::Real(0.13)]),
                // Link 1001 <-> 1002, and 1002 <-> 1003. Pin 1004 is left isolated (no link).
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(1001), PyRep::Integer(1002), PyRep::Integer(0)]),
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(1002), PyRep::Integer(1003), PyRep::Integer(0)]),
            ]),
        ],
    )
    .await
    .expect("Deploy colony network");

    let PyRep::List(pins) = col_dep.dict_get("pins").expect("pins list") else { panic!("expected pins list"); };
    let cc_pin_id = pins.iter().find(|p| get_i64(p, "typeID") == Some(CC_PIN_TYPE_ID as i64)).and_then(|p| get_i64(p, "pinID")).expect("cc pin id");
    let launch_pin_id = pins.iter().find(|p| get_i64(p, "typeID") == Some(LAUNCHPAD_PIN_TYPE_ID as i64)).and_then(|p| get_i64(p, "pinID")).expect("launch pin id");
    let storage_pin_id = pins.iter().find(|p| get_i64(p, "typeID") == Some(STORAGE_PIN_TYPE_ID as i64)).and_then(|p| get_i64(p, "pinID")).expect("storage pin id");
    let isolated_pin_id = pins.iter().find(|p| get_i64(p, "typeID") == Some(EXTRACTOR_PIN_TYPE_ID as i64)).and_then(|p| get_i64(p, "pinID")).expect("isolated pin id");

    // Negative check: ImportToColony fails when character does not hold the items
    let imp_missing = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(WATER_P1_TYPE_ID as i64),
            PyRep::Integer(500),
            PyRep::String("ImportToColony".into()),
        ],
    ).await.expect_err("Import without commodity must fail");
    assert_eq!(imp_missing.code, "ItemNotFound");

    ctx.db.with_mut_snapshot(|snap| {
        seed_cargo_item(snap, WATER_P1_TYPE_ID, char_id, ship_id, 200);
    });
    let imp_insuf = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(WATER_P1_TYPE_ID as i64),
            PyRep::Integer(500),
            PyRep::String("ImportToColony".into()),
        ],
    ).await.expect_err("Import with insufficient quantity must fail");
    assert_eq!(imp_insuf.code, "InsufficientCommodityQuantity");

    // Seed remaining items for 500 initial + 3000 later import
    ctx.db.with_mut_snapshot(|snap| {
        seed_cargo_item(snap, WATER_P1_TYPE_ID, char_id, ship_id, 3300);
    });

    // Import 500 units of Water P1 into Launchpad pin via UserLaunchCommodities (ImportToColony)
    rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(WATER_P1_TYPE_ID as i64),
            PyRep::Integer(500),
            PyRep::String("ImportToColony".into()),
        ],
    )
    .await
    .expect("ImportToColony");

    let comms_map = |tid: TypeId, qty: i64| -> PyRep {
        PyRep::Dict(vec![(PyRep::Integer(tid as i64), PyRep::Integer(qty))])
    };

    // 1. Error: Source pin not found
    let err_src_missing = rpc(
        &ctx, &mut sess, "planetMgr", "UserTransferCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![PyRep::Integer(999_999), PyRep::Integer(storage_pin_id)]), comms_map(WATER_P1_TYPE_ID, 50)],
    ).await.expect_err("Source pin not found must fail");
    assert_eq!(err_src_missing.code, "PinNotFound");

    // 2. Error: Destination pin not found
    let err_dst_missing = rpc(
        &ctx, &mut sess, "planetMgr", "UserTransferCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![PyRep::Integer(launch_pin_id), PyRep::Integer(999_999)]), comms_map(WATER_P1_TYPE_ID, 50)],
    ).await.expect_err("Dest pin not found must fail");
    assert_eq!(err_dst_missing.code, "PinNotFound");

    // 3. Error: No link between pins
    let err_no_link = rpc(
        &ctx, &mut sess, "planetMgr", "UserTransferCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![PyRep::Integer(launch_pin_id), PyRep::Integer(isolated_pin_id)]), comms_map(WATER_P1_TYPE_ID, 50)],
    ).await.expect_err("No link path between pins must fail");
    assert_eq!(err_no_link.code, "NoLinkBetweenPins");

    // 4. Error: Insufficient commodity in source pin (source has 500, request 1000)
    let err_insuf = rpc(
        &ctx, &mut sess, "planetMgr", "UserTransferCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![PyRep::Integer(launch_pin_id), PyRep::Integer(storage_pin_id)]), comms_map(WATER_P1_TYPE_ID, 1_000)],
    ).await.expect_err("Insufficient commodity must fail");
    assert_eq!(err_insuf.code, "InsufficientCommodityStorage");

    // 5. Error: Destination pin storage capacity full
    // Command Center has 500 m3 capacity. Transferring 3,000 units of Water (3,000 * 0.19 m3 = 570 m3) exceeds 500 m3.
    rpc(
        &ctx, &mut sess, "planetMgr", "UserLaunchCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(WATER_P1_TYPE_ID as i64), PyRep::Integer(3000), PyRep::String("ImportToColony".into())],
    ).await.expect("ImportToColony 3000");

    let err_cap = rpc(
        &ctx, &mut sess, "planetMgr", "UserTransferCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![PyRep::Integer(launch_pin_id), PyRep::Integer(cc_pin_id)]), comms_map(WATER_P1_TYPE_ID, 3_000)],
    ).await.expect_err("Destination full must fail");
    assert_eq!(err_cap.code, "PinStorageFull");

    // 6. Successful expedited transfer via V24 bound moniker signature (path, commodities) -> (simRet, lastRunTime)
    let expected_ft = ctx.now_blueos_filetime_ticks();
    let bound_planet_id = ctx
        .bind_object(&sess, "planetMgr", JITA_PLANET_ID)
        .expect("bind planetMgr");
    let xfer_ok = ClientHarness::new(SessionBorrowWireTransport {
        ctx: &ctx,
        session: &mut sess,
        preserve_char_notifications: false,
    })
    .with_mode(ContractValidationMode::Strict)
    .call_ok(
        "planetMgr",
        Some(bound_planet_id),
        "UserTransferCommodities",
        &[
            PyRep::List(vec![PyRep::Integer(launch_pin_id), PyRep::Integer(storage_pin_id)]),
            comms_map(WATER_P1_TYPE_ID, 150),
        ],
        &HashMap::new(),
    )
    .await;

    let PyRep::Tuple(ref xfer_tuple) = xfer_ok else {
        panic!("UserTransferCommodities must return 2-tuple (simRet, lastRunTime), got {xfer_ok:?}");
    };
    assert_eq!(xfer_tuple.len(), 2);
    assert_eq!(xfer_tuple[0].as_i64(), Some(expected_ft));
    assert_eq!(xfer_tuple[1].as_i64(), Some(expected_ft));

    // Verify storage amounts updated in pin contents and layout
    ctx.db.with_snapshot(|snap| {
        let net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
        let lp = net.pins.iter().find(|p| p.pin_id == launch_pin_id).expect("lp");
        let sp = net.pins.iter().find(|p| p.pin_id == storage_pin_id).expect("sp");
        assert_eq!(lp.contents.get(&WATER_P1_TYPE_ID).copied(), Some(3350));
        assert_eq!(sp.contents.get(&WATER_P1_TYPE_ID).copied(), Some(150));

        let layout = snap.planetary_colony_layouts.get(&(char_id, JITA_PLANET_ID)).expect("layout");
        let l_lp = layout.pins.iter().find(|p| p.pin_id == launch_pin_id).expect("layout lp");
        let l_sp = layout.pins.iter().find(|p| p.pin_id == storage_pin_id).expect("layout sp");
        let lp_amt: i64 = l_lp.contents.iter().filter(|c| c.type_id == WATER_P1_TYPE_ID).map(|c| c.amount).sum();
        let sp_amt: i64 = l_sp.contents.iter().filter(|c| c.type_id == WATER_P1_TYPE_ID).map(|c| c.amount).sum();
        assert_eq!(lp_amt, 3350);
        assert_eq!(sp_amt, 150);
    });

    // 7. Error: Expedited transfer on cooldown
    let err_cd = rpc(
        &ctx, &mut sess, "planetMgr", "UserTransferCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![PyRep::Integer(launch_pin_id), PyRep::Integer(storage_pin_id)]), comms_map(WATER_P1_TYPE_ID, 50)],
    ).await.expect_err("Immediate repeat transfer must fail with cooldown");
    assert_eq!(err_cd.code, "ExpeditedTransferOnCooldown");

    // 8. After cooldown elapses, unbound V24 transfer (planetID, path, commodities) returns updated timestamps
    ctx.advance_secs(301);
    let expected_ft_2 = ctx.now_blueos_filetime_ticks();
    let xfer_unbound = ClientHarness::new(SessionBorrowWireTransport {
        ctx: &ctx,
        session: &mut sess,
        preserve_char_notifications: false,
    })
    .with_mode(ContractValidationMode::Strict)
    .call_ok(
        "planetMgr",
        None,
        "UserTransferCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![PyRep::Integer(storage_pin_id), PyRep::Integer(launch_pin_id)]),
            comms_map(WATER_P1_TYPE_ID, 50),
        ],
        &HashMap::new(),
    )
    .await;
    let PyRep::Tuple(ref xfer_tuple_2) = xfer_unbound else {
        panic!("Expected 2-tuple from unbound UserTransferCommodities, got {xfer_unbound:?}");
    };
    assert_eq!(xfer_tuple_2.len(), 2);
    assert_eq!(xfer_tuple_2[0].as_i64(), Some(expected_ft_2));
    assert_eq!(xfer_tuple_2[1].as_i64(), Some(expected_ft_2));
    ctx.db.with_snapshot(|snap| {
        let net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
        let lp = net.pins.iter().find(|p| p.pin_id == launch_pin_id).expect("lp");
        let sp = net.pins.iter().find(|p| p.pin_id == storage_pin_id).expect("sp");
        assert_eq!(lp.contents.get(&WATER_P1_TYPE_ID).copied(), Some(3400));
        assert_eq!(sp.contents.get(&WATER_P1_TYPE_ID).copied(), Some(100));
    });
}

#[tokio::test]
async fn test_planetary_interaction_launch_commodities_v24() {
    // parity: feat:pi.launch, rpc:planetMgr.UserLaunchCommodities
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 1105, "demo", "demo-secret", char_id).await;
    let ship_id = sess.ship_id.expect("Active ship");
    ctx.db.with_mut_snapshot(|snap| {
        grant_pi_skills(snap, char_id);
        seed_cargo_item(snap, CC_PIN_TYPE_ID, char_id, ship_id, 1);
    });

    let col_dep = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2001), PyRep::Integer(CC_PIN_TYPE_ID as i64), PyRep::Real(0.10), PyRep::Real(0.10)]),
                PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(2001), PyRep::Integer(3)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2002), PyRep::Integer(LAUNCHPAD_PIN_TYPE_ID as i64), PyRep::Real(0.11), PyRep::Real(0.11)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2004), PyRep::Integer(EXTRACTOR_PIN_TYPE_ID as i64), PyRep::Real(0.12), PyRep::Real(0.12)]),
            ]),
        ],
    )
    .await
    .expect("Deploy PI colony for V24 launch");

    let PyRep::List(pins) = col_dep.dict_get("pins").expect("pins list") else {
        panic!("expected pins list");
    };
    let cc_pin_id = pins
        .iter()
        .find(|p| get_i64(p, "typeID") == Some(CC_PIN_TYPE_ID as i64))
        .and_then(|p| get_i64(p, "pinID"))
        .expect("cc pin id");
    let launch_pin_id = pins
        .iter()
        .find(|p| get_i64(p, "typeID") == Some(LAUNCHPAD_PIN_TYPE_ID as i64))
        .and_then(|p| get_i64(p, "pinID"))
        .expect("launch pin id");
    let ext_pin_id = pins
        .iter()
        .find(|p| get_i64(p, "typeID") == Some(EXTRACTOR_PIN_TYPE_ID as i64))
        .and_then(|p| get_i64(p, "pinID"))
        .expect("ext pin id");

    ctx.db.with_mut_snapshot(|snap| {
        let mut net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
        if let Some(cc) = net.pins.iter_mut().find(|p| p.pin_id == cc_pin_id) {
            cc.contents.insert(WATER_P1_TYPE_ID, 2_800);
        }
        if let Some(lp) = net.pins.iter_mut().find(|p| p.pin_id == launch_pin_id) {
            lp.contents.insert(WATER_P1_TYPE_ID, 500);
        }
        if let Some(ext) = net.pins.iter_mut().find(|p| p.pin_id == ext_pin_id) {
            ext.contents.insert(WATER_P1_TYPE_ID, 50);
        }
        eve_server::services::industry_pi::save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
    });

    let bound_planet_id = ctx
        .bind_object(&sess, "planetMgr", JITA_PLANET_ID)
        .expect("bind planetMgr");

    // 1. Validation gates with V24 (commandPinID, commoditiesToLaunch) signature
    let err_pin_missing = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(999_999),
            PyRep::Dict(vec![(PyRep::Integer(WATER_P1_TYPE_ID as i64), PyRep::Integer(10))]),
        ],
    )
    .await
    .expect_err("Missing pin must fail");
    assert_eq!(err_pin_missing.code, "PinNotFound");

    let err_pin_type = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(ext_pin_id),
            PyRep::Dict(vec![(PyRep::Integer(WATER_P1_TYPE_ID as i64), PyRep::Integer(10))]),
        ],
    )
    .await
    .expect_err("Extractor pin cannot launch");
    assert_eq!(err_pin_type.code, "InvalidPinTypeForLaunch");

    // Command Center launch capacity cap is 500 m3 (3,000 * 0.19 m3 = 570 m3 > 500 m3)
    let err_cc_cap = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(cc_pin_id),
            PyRep::Dict(vec![(PyRep::Integer(WATER_P1_TYPE_ID as i64), PyRep::Integer(3_000))]),
        ],
    )
    .await
    .expect_err("Exceeding Command Center 500 m3 cap must fail");
    assert_eq!(err_cc_cap.code, "CommandCenterLaunchCapacityExceeded");

    // Empty commodities dict rejected
    let err_empty = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserLaunchCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(cc_pin_id), PyRep::Dict(Vec::new())],
    )
    .await
    .expect_err("Empty dict must fail");
    assert_eq!(err_empty.code, "InvalidCommodity");

    // Insufficient stock in pin (Launchpad only has 500, ask for 600)
    let err_insuf = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(launch_pin_id),
            PyRep::Dict(vec![(PyRep::Integer(WATER_P1_TYPE_ID as i64), PyRep::Integer(600))]),
        ],
    )
    .await
    .expect_err("Insufficient commodities in pin must fail");
    assert_eq!(err_insuf.code, "InsufficientCommoditiesForRouting");

    // 2. Successful V24 bound launch under ContractValidationMode::Strict -> scalar Blue FileTime timestamp
    let expected_ft = ctx.now_blueos_filetime_ticks();
    let launch_rep = ClientHarness::new(SessionBorrowWireTransport {
        ctx: &ctx,
        session: &mut sess,
        preserve_char_notifications: false,
    })
    .with_mode(ContractValidationMode::Strict)
    .call_ok(
        "planetMgr",
        Some(bound_planet_id),
        "UserLaunchCommodities",
        &[
            PyRep::Integer(cc_pin_id),
            PyRep::Dict(vec![(PyRep::Integer(WATER_P1_TYPE_ID as i64), PyRep::Integer(200))]),
        ],
        &HashMap::new(),
    )
    .await;
    assert_eq!(
        launch_rep.as_i64(),
        Some(expected_ft),
        "V24 UserLaunchCommodities must return scalar Blue FileTime lastLaunchTime"
    );

    // Immediate retry from same pin must fail with 60s LaunchOnCooldown
    let err_cd = rpc(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(cc_pin_id),
            PyRep::Dict(vec![(PyRep::Integer(WATER_P1_TYPE_ID as i64), PyRep::Integer(50))]),
        ],
    )
    .await
    .expect_err("Immediate repeat launch from same pin must hit 60s cooldown");
    assert_eq!(err_cd.code, "LaunchOnCooldown");

    // Inspect created launch canister & verify pin deduction
    let launches = rpc(&ctx, &mut sess, "planetMgr", "GetMyLaunchesDetails", &[PyRep::Integer(JITA_PLANET_ID)])
        .await
        .expect("GetMyLaunchesDetails");
    let PyRep::List(launch_rows) = launches else {
        panic!("expected launches list");
    };
    assert_eq!(launch_rows.len(), 1);
    let launch_id = launch_rows[0]
        .dict_get("launchID")
        .and_then(|v| v.as_i64())
        .expect("launchID");
    assert_eq!(launch_rows[0].dict_get("quantity").and_then(|v| v.as_i64()), Some(200));

    ctx.db.with_snapshot(|snap| {
        let net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
        let cc = net.pins.iter().find(|p| p.pin_id == cc_pin_id).expect("cc");
        assert_eq!(cc.contents.get(&WATER_P1_TYPE_ID).copied(), Some(2_600));
        let container = snap.items.get(&launch_id).expect("launch container item");
        assert_eq!(container.location_id, i64::from(JITA_SOLAR_SYSTEM_ID));
    });

    // Advance 61s and launch again from the same pin
    ctx.advance_secs(61);
    let expected_ft_2 = ctx.now_blueos_filetime_ticks();
    let launch_rep_2 = ClientHarness::new(SessionBorrowWireTransport {
        ctx: &ctx,
        session: &mut sess,
        preserve_char_notifications: false,
    })
    .with_mode(ContractValidationMode::Strict)
    .call_ok(
        "planetMgr",
        Some(bound_planet_id),
        "UserLaunchCommodities",
        &[
            PyRep::Integer(cc_pin_id),
            PyRep::Dict(vec![(PyRep::Integer(WATER_P1_TYPE_ID as i64), PyRep::Integer(100))]),
        ],
        &HashMap::new(),
    )
    .await;
    assert_eq!(launch_rep_2.as_i64(), Some(expected_ft_2));
}
