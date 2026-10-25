//! Complete tests for Planetary Interaction expedited commodity transfer (`feat:pi.expedited-transfer`, `rpc:planetMgr.UserTransferCommodities`).
//!
//! Validates pin lookup, storage quantity availability, link path reachability,
//! destination capacity bounds, atomic storage quantity updates, and transfer cooldowns.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
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

async fn setup_pilot(ctx: &ServerContext, session_id: u64, username: &str, password: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
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
    let kw = HashMap::new();
    let col_dep = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
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
            &kw,
            None,
        )
        .await
        .expect("Deploy colony network");

    let PyRep::List(pins) = col_dep.dict_get("pins").expect("pins list") else { panic!("expected pins list"); };
    let cc_pin_id = pins.iter().find(|p| get_i64(p, "typeID") == Some(CC_PIN_TYPE_ID as i64)).and_then(|p| get_i64(p, "pinID")).expect("cc pin id");
    let launch_pin_id = pins.iter().find(|p| get_i64(p, "typeID") == Some(LAUNCHPAD_PIN_TYPE_ID as i64)).and_then(|p| get_i64(p, "pinID")).expect("launch pin id");
    let storage_pin_id = pins.iter().find(|p| get_i64(p, "typeID") == Some(STORAGE_PIN_TYPE_ID as i64)).and_then(|p| get_i64(p, "pinID")).expect("storage pin id");
    let isolated_pin_id = pins.iter().find(|p| get_i64(p, "typeID") == Some(EXTRACTOR_PIN_TYPE_ID as i64)).and_then(|p| get_i64(p, "pinID")).expect("isolated pin id");

    // Import 500 units of Water P1 into Launchpad pin via UserLaunchCommodities (ImportToColony)
    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserLaunchCommodities",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(WATER_P1_TYPE_ID as i64),
            PyRep::Integer(500),
            PyRep::String("ImportToColony".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("ImportToColony");

    let comms_map = |tid: TypeId, qty: i64| -> PyRep {
        PyRep::Dict(vec![(PyRep::Integer(tid as i64), PyRep::Integer(qty))])
    };

    // 1. Error: Source pin not found
    let err_src_missing = ctx.dispatch_rpc(
        &mut sess, "planetMgr", None, "UserTransferCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![PyRep::Integer(999_999), PyRep::Integer(storage_pin_id)]), comms_map(WATER_P1_TYPE_ID, 50)],
        &kw, None,
    ).await.expect_err("Source pin not found must fail");
    assert_eq!(err_src_missing.code, "PinNotFound");

    // 2. Error: Destination pin not found
    let err_dst_missing = ctx.dispatch_rpc(
        &mut sess, "planetMgr", None, "UserTransferCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![PyRep::Integer(launch_pin_id), PyRep::Integer(999_999)]), comms_map(WATER_P1_TYPE_ID, 50)],
        &kw, None,
    ).await.expect_err("Dest pin not found must fail");
    assert_eq!(err_dst_missing.code, "PinNotFound");

    // 3. Error: No link between pins
    let err_no_link = ctx.dispatch_rpc(
        &mut sess, "planetMgr", None, "UserTransferCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![PyRep::Integer(launch_pin_id), PyRep::Integer(isolated_pin_id)]), comms_map(WATER_P1_TYPE_ID, 50)],
        &kw, None,
    ).await.expect_err("No link path between pins must fail");
    assert_eq!(err_no_link.code, "NoLinkBetweenPins");

    // 4. Error: Insufficient commodity in source pin (source has 500, request 1000)
    let err_insuf = ctx.dispatch_rpc(
        &mut sess, "planetMgr", None, "UserTransferCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![PyRep::Integer(launch_pin_id), PyRep::Integer(storage_pin_id)]), comms_map(WATER_P1_TYPE_ID, 1_000)],
        &kw, None,
    ).await.expect_err("Insufficient commodity must fail");
    assert_eq!(err_insuf.code, "InsufficientCommodityStorage");

    // 5. Error: Destination pin storage capacity full
    // Command Center has 500 m3 capacity. Transferring 3,000 units of Water (3,000 * 0.19 m3 = 570 m3) exceeds 500 m3.
    ctx.dispatch_rpc(
        &mut sess, "planetMgr", None, "UserLaunchCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(WATER_P1_TYPE_ID as i64), PyRep::Integer(3000), PyRep::String("ImportToColony".into())],
        &kw, None,
    ).await.expect("ImportToColony 3000");

    let err_cap = ctx.dispatch_rpc(
        &mut sess, "planetMgr", None, "UserTransferCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![PyRep::Integer(launch_pin_id), PyRep::Integer(cc_pin_id)]), comms_map(WATER_P1_TYPE_ID, 3_000)],
        &kw, None,
    ).await.expect_err("Destination full must fail");
    assert_eq!(err_cap.code, "PinStorageFull");

    // 6. Successful expedited transfer via V24 client signature: Launchpad -> Storage, transfer 150 units (28.5 m3)
    let xfer_ok = ctx.dispatch_rpc(
        &mut sess, "planetMgr", None, "UserTransferCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![PyRep::Integer(launch_pin_id), PyRep::Integer(storage_pin_id)]), comms_map(WATER_P1_TYPE_ID, 150)],
        &kw, None,
    ).await.expect("Expedited transfer should succeed");

    assert_eq!(get_i64(&xfer_ok, "planetID"), Some(JITA_PLANET_ID));
    assert_eq!(get_i64(&xfer_ok, "sourcePinID"), Some(launch_pin_id));
    assert_eq!(get_i64(&xfer_ok, "destPinID"), Some(storage_pin_id));
    assert_eq!(get_i64(&xfer_ok, "commodityTypeID"), Some(WATER_P1_TYPE_ID as i64));
    assert_eq!(get_i64(&xfer_ok, "quantity"), Some(150));
    assert!(get_i64(&xfer_ok, "cooldownSeconds").unwrap_or(0) >= 300);

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
    let err_cd = ctx.dispatch_rpc(
        &mut sess, "planetMgr", None, "UserTransferCommodities",
        &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![PyRep::Integer(launch_pin_id), PyRep::Integer(storage_pin_id)]), comms_map(WATER_P1_TYPE_ID, 50)],
        &kw, None,
    ).await.expect_err("Immediate repeat transfer must fail with cooldown");
    assert_eq!(err_cd.code, "ExpeditedTransferOnCooldown");
}
