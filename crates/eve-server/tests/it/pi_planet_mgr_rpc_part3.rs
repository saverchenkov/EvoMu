//! Integration tests for multi-colony bound moniker isolation and Planet Manager RPCs:
//! - Bound moniker planet routing (verifying Planet B never mutates or affects Planet A)
//! - rpc:planetMgr.UserAbandonPlanet (unbound 0-arg rejection and bound 0-arg isolation)
//! - rpc:planetMgr.CancelInstallProgram (bound routing and PinNotFound error without mutating net.pins[0])
//! - rpc:planetMgr.GetProgramResultInfo (bound moniker routing, preview heads overlap, and non-mutation)
//! - rpc:planetMgr.GetResourceData (bound moniker routing without arbitrary Jita IV fallback)

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
