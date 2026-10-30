//! Extended Planetary Interaction, Customs Offices, and Wallet/Item Trader parity tests (Part 2).

use crate::common;
use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    TrainedSkillRecord, TypeId,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::station_structure::structure_upwell_ops::StructureExtendedState;
use std::collections::{HashMap, HashSet};

const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const JITA_PLANET_ID: i64 = 40_009_082; // Jita IV (Gas)
const LOWSEC_SOLAR_SYSTEM_ID: u32 = 30_000_012;
const LOWSEC_STATION_ID: i64 = 60_012_292;
const LOWSEC_PLANET_ID: i64 = 40_000_663;

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
    singleton: bool,
    is_blueprint_copy: bool,
    runs: Option<i32>,
) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    let mut dynamic_attributes = HashMap::new();
    if is_blueprint_copy {
        dynamic_attributes.insert(1115, 1.0);
    }
    if let Some(r) = runs {
        dynamic_attributes.insert(1124, r as f64);
    }
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton,
            version: 1,
            dynamic_attributes,
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy,
            blueprint_runs_remaining: runs,
        },
    );
    item_id
}

#[tokio::test]
async fn test_pi_command_center_budget_launchpads_and_planet_resources() {
    // parity: feat:pi.command-center-cpu-power-budget, feat:pi.command-center-import-export-pad, feat:pi.planet-type-resource-restrictions
    eve_testkit::attest(&[
        "feat:pi.command-center-cpu-power-budget",
        "feat:pi.command-center-import-export-pad",
        "feat:pi.planet-type-resource-restrictions",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 705, "demo", "demo-secret", char_id).await;
    sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
    sess.station_id = None;
    let ship_id = sess.ship_id.expect("Active ship");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.balance = IskCents::from_isk(100_000_000);
        }
        seed_item(snap, 2534, char_id, ship_id, InventoryFlag::Cargo, 1, false, false, None);
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(2505, TrainedSkillRecord { type_id: 2505, level: 2, skill_points: 8_000.0 });
        sk.insert(2495, TrainedSkillRecord { type_id: 2495, level: 5, skill_points: 256_000.0 });
    });

    // 1. Planet type resource restrictions
    let gas_res = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetPlanetResourceInfo", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("Gas planet resources");
    let PyRep::List(gas_list) = gas_res.dict_get("resources").unwrap() else { panic!() };
    let gas_tids: Vec<TypeId> = gas_list
        .iter()
        .filter_map(|r| r.dict_get("resourceTypeID").and_then(|v| v.as_i64()))
        .map(|v| v as TypeId)
        .collect();
    assert!(gas_tids.contains(&2310));
    assert!(!gas_tids.contains(&2307));
    assert!(!gas_tids.contains(&2286));

    // 2. Command Center CPU/PG budget gated by Command Center Upgrades skill
    let deploy = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(101),
                    PyRep::Integer(2534),
                    PyRep::Real(0.10),
                    PyRep::Real(0.10),
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .expect("Deploy Gas Command Center");
    assert_eq!(deploy.dict_get("cpuCapacity"), Some(&PyRep::Integer(1675)));
    assert_eq!(deploy.dict_get("powerCapacity"), Some(&PyRep::Integer(6000)));

    let skill_err = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "UpgradeCommandCenter", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(3)], &kw, None)
        .await
        .expect_err("Upgrade above skill level must fail");
    assert_eq!(skill_err.code, "MissingSkillForCommandCenterUpgrade");

    let pg_err = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(3060), PyRep::Real(0.11), PyRep::Real(0.11)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(3060), PyRep::Real(0.12), PyRep::Real(0.12)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(104), PyRep::Integer(3060), PyRep::Real(0.13), PyRep::Real(0.13)]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Exceeding Command Center PG budget must fail");
    assert_eq!(pg_err.code, "InsufficientColonyCpuOrPowergrid");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            2505,
            TrainedSkillRecord { type_id: 2505, level: 5, skill_points: 256_000.0 },
        );
    });
    let up5 = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "UpgradeCommandCenter", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(5)], &kw, None)
        .await
        .expect("Upgrade Command Center to level 5");
    assert_eq!(up5.dict_get("cpuMax"), Some(&PyRep::Real(25_415.0)));
    assert_eq!(up5.dict_get("powergridMax"), Some(&PyRep::Real(19_000.0)));

    let non_native_err = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(3060), PyRep::Real(0.11), PyRep::Real(0.11)]),
                    PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(102), PyRep::Integer(2307), PyRep::Integer(300), PyRep::Integer(1)]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Extracting non-native P0 resource must fail");
    assert_eq!(non_native_err.code, "ClientSchematicOverrideForbidden");

    // 3. Command Center 500 m3 one-way orbital launch (at 1.5x higher tax) vs Spaceport Launchpad 10,000 m3 bidirectional transfer
    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, 3645, char_id, ship_id, InventoryFlag::Cargo, 100, false, false, None);
        let mut net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 0);
        if let Some(pin) = net.pins.first_mut() {
            pin.contents.insert(3645, 1_000);
        }
        eve_server::services::industry_pi::save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
    });
    let cc_cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserLaunchCommodities",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(3645), PyRep::Integer(3_000), PyRep::String("CommandCenter".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Command Center launch > 500 m3 must fail");
    assert_eq!(cc_cap_err.code, "CommandCenterLaunchCapacityExceeded");

    let cc_launch = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserLaunchCommodities",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(3645), PyRep::Integer(100), PyRep::String("CommandCenter".into())],
            &kw,
            None,
        )
        .await
        .expect("Valid Command Center launch");
    let cc_tax = cc_launch.dict_get("exportTaxISK").and_then(|v| v.as_f64()).unwrap();

    let lp_cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserLaunchCommodities",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(3645), PyRep::Integer(60_000), PyRep::String("CustomsOffice".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Launchpad transfer > 10,000 m3 must fail");
    assert_eq!(lp_cap_err.code, "LaunchpadCapacityExceeded");

    let lp_export = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserLaunchCommodities",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(3645), PyRep::Integer(100), PyRep::String("CustomsOffice".into())],
            &kw,
            None,
        )
        .await
        .expect("Valid Launchpad export");
    let lp_export_tax = lp_export.dict_get("exportTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((cc_tax - lp_export_tax * 1.5).abs() < 1e-4);

    let lp_import = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserLaunchCommodities",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(3645), PyRep::Integer(100), PyRep::String("ImportToColony".into())],
            &kw,
            None,
        )
        .await
        .expect("Valid Launchpad import");
    let lp_import_tax = lp_import.dict_get("importTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((lp_import_tax - lp_export_tax * 0.5).abs() < 1e-4);
}

#[tokio::test]
async fn test_pi_extractor_heads_decay_and_schematic_routing() {
    // parity: feat:pi.extractor-head-placement-and-decay, feat:pi.schematic-input-output-routing
    eve_testkit::attest(&[
        "feat:pi.extractor-head-placement-and-decay",
        "feat:pi.schematic-input-output-routing",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 706, "demo", "demo-secret", char_id).await;
    sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
    sess.station_id = None;
    let ship_id = sess.ship_id.expect("Active ship");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.balance = IskCents::from_isk(100_000_000);
        }
        seed_item(snap, 2534, char_id, ship_id, InventoryFlag::Cargo, 1, false, false, None);
        snap.skills.entry(char_id).or_default().insert(
            2505,
            TrainedSkillRecord { type_id: 2505, level: 5, skill_points: 256_000.0 },
        );
    });

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
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(3060), PyRep::Real(0.11), PyRep::Real(0.11)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(2492), PyRep::Real(0.12), PyRep::Real(0.12)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(104), PyRep::Integer(2494), PyRep::Real(0.13), PyRep::Real(0.13)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(105), PyRep::Integer(2482), PyRep::Real(0.14), PyRep::Real(0.14)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(106), PyRep::Integer(2541), PyRep::Real(0.15), PyRep::Real(0.15)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(107), PyRep::Integer(2543), PyRep::Real(0.16), PyRep::Real(0.16)]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("Deploy full PI facility network");
    let PyRep::List(pins) = setup.dict_get("pins").unwrap() else { panic!() };
    let ext_id = pins[1].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
    let basic_id = pins[2].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
    let adv_id = pins[3].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
    let htech_id = pins[4].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
    let store_id = pins[5].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
    let pad_id = pins[6].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();

    // 1. Extractor head placement: max 10 heads, radius check, overlap interference, and per-cycle decay
    let over_10_err = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(10),
                    PyRep::Integer(ext_id),
                    PyRep::Integer(2268),
                    PyRep::Integer(300),
                    PyRep::Integer(11),
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("> 10 extractor heads must fail");
    assert_eq!(over_10_err.code, "InvalidExtractorHeadCount");

    let out_of_range_err = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(14),
                    PyRep::Integer(ext_id),
                    PyRep::Integer(1),
                    PyRep::Real(1.20),
                    PyRep::Real(1.20),
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Head outside ECU radius must fail");
    assert_eq!(out_of_range_err.code, "ExtractorHeadOutOfRange");

    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(10),
                PyRep::Integer(ext_id),
                PyRep::Integer(2268),
                PyRep::Real(0.03),
                PyRep::Integer(2),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.11), PyRep::Real(0.11)]),
                    PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Real(0.20), PyRep::Real(0.20)]),
                ]),
            ])]),
        ],
        &kw,
        None,
    )
    .await
    .expect("Install separated 2-head program");
    let prog_sep = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetProgramResultInfo", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ext_id)], &kw, None)
        .await
        .expect("GetProgramResultInfo separated");
    let PyRep::Tuple(sep_tuple) = &prog_sep else { panic!() };
    let sep_c0 = sep_tuple[0].as_i64().unwrap();
    assert!(sep_c0 > 0);
    assert_eq!(sep_tuple[2].as_i64(), Some(10));

    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(15),
                PyRep::Integer(ext_id),
                PyRep::Integer(2),
                PyRep::Real(0.111),
                PyRep::Real(0.111),
            ])]),
        ],
        &kw,
        None,
    )
    .await
    .expect("Move head 2 to overlap head 1");
    let prog_ovl = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetProgramResultInfo", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ext_id)], &kw, None)
        .await
        .expect("GetProgramResultInfo overlapping");
    let PyRep::Tuple(ovl_tuple) = &prog_ovl else { panic!() };
    let ovl_c0 = ovl_tuple[0].as_i64().unwrap();
    assert!(ovl_c0 < sep_c0);

    // 2. Schematic input/output routing across Basic (P0->P1), Advanced (P1->P2), and High-Tech (P3->P4)
    let find_schem = |out_tid: TypeId| -> i64 {
        ctx.sde
            .planet_schematics
            .iter()
            .find(|(_, s)| s.types.iter().any(|t| !t.is_input && t.type_id == out_tid))
            .map(|(&id, _)| i64::from(id))
            .unwrap_or_else(|| panic!("missing SDE schematic for output {out_tid}"))
    };
    let water_schem = find_schem(3645);
    let biocells_schem = find_schem(9832);
    let nanofactory_schem = find_schem(2875);

    let non_fac_err = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(store_id), PyRep::Integer(water_schem)])]),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Installing schematic on Storage pin must fail");
    assert_eq!(non_fac_err.code, "InvalidSchematic");

    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(basic_id), PyRep::Integer(water_schem)]),
                PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(adv_id), PyRep::Integer(biocells_schem)]),
                PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(htech_id), PyRep::Integer(nanofactory_schem)]),
            ]),
        ],
        &kw,
        None,
    )
    .await
    .expect("Install P1, P2, and P4 schematics");

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

    ctx.db.with_mut_snapshot(|snap| {
        let mut net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 5);
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
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetColony", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("GetColony without routes");
    assert_eq!(col_unrouted.dict_get("simulatedProcessedQuantity"), Some(&PyRep::Integer(0)));

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
    .expect("Create input and output routes for P1, P2, and P4 schematics");

    ctx.advance_virtual_clock(3_600);
    let col_routed = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetColony", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("GetColony with full routes");
    assert_eq!(col_routed.dict_get("simulatedProcessedQuantity"), Some(&PyRep::Integer(46)));
}

#[tokio::test]
async fn test_pi_poco_gantry_deploy_reinforce_tax_rates_and_standings() {
    // parity: feat:pi.customs-office-gantry-deploy-reinforce, feat:pi.customs-office-tax-and-standings, feat:pi.poco-customs-office-tax-rates
    eve_testkit::attest(&[
        "feat:pi.customs-office-gantry-deploy-reinforce",
        "feat:pi.customs-office-tax-and-standings",
        "feat:pi.poco-customs-office-tax-rates",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let owner_char = 90_000_001;
    let trader_char = 90_000_002;
    let owner_corp = 98_000_721;
    let ally_corp = 98_000_722;
    let ext_corp = 98_000_723;
    let alliance_id = 99_000_721;

    let mut s_owner = setup_pilot(&ctx, 707, "demo", "demo-secret", owner_char).await;
    let mut s_trader = setup_pilot(&ctx, 708, "other", "other-secret", trader_char).await;

    let owner_ship = s_owner.ship_id.expect("Owner ship");
    let (gantry_hs_id, gantry_ls_id, poco_upgrade_id) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&owner_char) {
            ch.corporation_id = owner_corp;
            ch.alliance_id = Some(alliance_id);
            ch.station_id = None;
            ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
        }
        if let Some(ch) = snap.characters.get_mut(&trader_char) {
            ch.corporation_id = ally_corp;
            ch.alliance_id = Some(alliance_id);
            ch.station_id = None;
            ch.solar_system_id = LOWSEC_SOLAR_SYSTEM_ID;
            ch.balance = IskCents::from_isk(100_000_000);
        }
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Orbital Alliance".into(),
                short_name: "ORBA".into(),
                executor_corp_id: owner_corp,
                member_corp_ids: HashSet::from([owner_corp, ally_corp]),
            },
        );
        for (cid, ceo, al) in [
            (owner_corp, owner_char, Some(alliance_id)),
            (ally_corp, trader_char, Some(alliance_id)),
            (ext_corp, trader_char, None),
        ] {
            snap.corporations.insert(
                cid,
                CorporationRecord {
                    corporation_id: cid,
                    name: format!("Corp {cid}"),
                    ticker: "CRP".into(),
                    ceo_id: ceo,
                    alliance_id: al,
                    tax_rate: 0.05,
                    master_wallet: IskCents::from_isk(10_000_000),
                    members: HashSet::from([ceo]),
                    member_roles: HashMap::from([(ceo, u64::MAX)]),
                    applications: HashMap::new(),
                    wallet_divisions: Vec::new(),
                    description: String::new(),
                    url: String::new(),
                    hq_station_id: Some(LOWSEC_STATION_ID),
                },
            );
        }
        (
            seed_item(snap, 3962, owner_char, owner_ship, InventoryFlag::Cargo, 1, false, false, None),
            seed_item(snap, 3962, owner_char, owner_ship, InventoryFlag::Cargo, 1, false, false, None),
            seed_item(snap, 2233, owner_char, owner_ship, InventoryFlag::Cargo, 1, false, false, None),
        )
    });

    // 1. Customs Office Gantry deploy & upgrade to POCO
    s_owner.station_id = None;
    s_owner.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
    let hs_err = ctx
        .dispatch_rpc(&mut s_owner, "posMgr", None, "AnchorOrbital", &[PyRep::Integer(gantry_hs_id), PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect_err("Anchoring Gantry in high-sec must fail");
    assert_eq!(hs_err.code, "CannotAnchorOrbitalInHighSec");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&owner_char) {
            ch.solar_system_id = LOWSEC_SOLAR_SYSTEM_ID;
        }
    });
    s_owner.solar_system_id = Some(LOWSEC_SOLAR_SYSTEM_ID);
    let gantry_res = ctx
        .dispatch_rpc(&mut s_owner, "posMgr", None, "AnchorOrbital", &[PyRep::Integer(gantry_ls_id), PyRep::Integer(LOWSEC_PLANET_ID)], &kw, None)
        .await
        .expect("Anchor Customs Office Gantry at low-sec planet");
    let orbital_id = gantry_res.dict_get("orbitalID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(gantry_res.dict_get("typeID"), Some(&PyRep::Integer(3962)));

    let online_res = ctx
        .dispatch_rpc(&mut s_owner, "posMgr", None, "OnlineOrbital", &[PyRep::Integer(orbital_id), PyRep::Integer(LOWSEC_PLANET_ID)], &kw, None)
        .await
        .expect("Upgrade Gantry to POCO via OnlineOrbital");
    assert_eq!(online_res.dict_get("typeID"), Some(&PyRep::Integer(2233)));
    assert!(ctx.db.get_item(poco_upgrade_id).is_none());

    // 2. Configure POCO tax rates for Corp, Alliance, and Standing brackets (+10, +5, 0, -5, -10) and standing threshold
    ctx.dispatch_rpc(
        &mut s_owner,
        "planetOrbitalRegistryBroker",
        None,
        "UpdateSettings",
        &[
            PyRep::Integer(LOWSEC_PLANET_ID),
            PyRep::Integer(18),
            PyRep::keyval([
                ("corporation", PyRep::Real(0.02)),
                ("alliance", PyRep::Real(0.04)),
                ("standingHigh", PyRep::Real(0.05)),
                ("standingGood", PyRep::Real(0.08)),
                ("standingNeutral", PyRep::Real(0.12)),
                ("standingBad", PyRep::Real(0.18)),
                ("standingHorrible", PyRep::Real(0.25)),
            ]),
            PyRep::Real(-5.0),
            PyRep::Bool(true),
            PyRep::Bool(true),
        ],
        &kw,
        None,
    )
    .await
    .expect("UpdateSettings on POCO");

    assert_eq!(
        ctx.dispatch_rpc(&mut s_owner, "planetOrbitalRegistryBroker", None, "GetTaxRate", &[PyRep::Integer(LOWSEC_PLANET_ID)], &kw, None).await.unwrap(),
        PyRep::Real(0.02)
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut s_trader, "planetOrbitalRegistryBroker", None, "GetTaxRate", &[PyRep::Integer(LOWSEC_PLANET_ID)], &kw, None).await.unwrap(),
        PyRep::Real(0.04)
    );

    for (standing_val, expected_rate) in [
        (8.0, Some(0.05)),
        (3.0, Some(0.08)),
        (0.0, Some(0.12)),
        (-5.0, Some(0.18)),
        (-8.0, None),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&trader_char) {
                ch.corporation_id = ext_corp;
                ch.alliance_id = None;
                ch.standings.insert(owner_corp, standing_val);
            }
        });
        let rate_res = ctx
            .dispatch_rpc(&mut s_trader, "planetOrbitalRegistryBroker", None, "GetTaxRate", &[PyRep::Integer(LOWSEC_PLANET_ID)], &kw, None)
            .await;
        match expected_rate {
            Some(r) => assert_eq!(rate_res.unwrap(), PyRep::Real(r)),
            None => assert_eq!(rate_res.unwrap_err().code, "CustomsOfficeAccessDenied"),
        }
    }

    // 3. POCO export/import tax formula across P0..P4 tiers
    s_trader.solar_system_id = Some(LOWSEC_SOLAR_SYSTEM_ID);
    s_trader.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&trader_char) {
            ch.standings.insert(owner_corp, 0.0);
        }
        snap.planetary_colonies.insert(
            (trader_char, LOWSEC_PLANET_ID),
            eve_db::PlanetaryColonyRecord {
                owner_id: trader_char,
                planet_id: LOWSEC_PLANET_ID,
                solar_system_id: LOWSEC_SOLAR_SYSTEM_ID,
                command_center_level: 5,
                pins_count: 2,
                extracted_commodities: HashMap::from([
                    (2268, 100),
                    (3645, 10),
                    (2329, 10),
                    (2345, 5),
                    (2867, 2),
                ]),
                last_extracted_unix: 0,
            },
        );
    });
    ctx.dispatch_rpc(
        &mut s_owner,
        "planetOrbitalRegistryBroker",
        None,
        "UpdateSettings",
        &[
            PyRep::Integer(LOWSEC_PLANET_ID),
            PyRep::Integer(18),
            PyRep::keyval([("standingNeutral", PyRep::Real(0.10))]),
            PyRep::Real(-10.0),
            PyRep::Bool(true),
            PyRep::Bool(true),
        ],
        &kw,
        None,
    )
    .await
    .expect("Set neutral tax rate to 10%");

    let corp_bal_before = ctx.db.with_snapshot(|snap| snap.corporations[&owner_corp].master_wallet);
    for (tid, base_val) in [
        (2268_u32, 5.0_f64),
        (3645, 400.0),
        (2329, 7_200.0),
        (2345, 60_000.0),
        (2867, 1_200_000.0),
    ] {
        let exp = ctx
            .dispatch_rpc(
                &mut s_trader,
                "planetMgr",
                None,
                "UserLaunchCommodities",
                &[PyRep::Integer(LOWSEC_PLANET_ID), PyRep::Integer(tid as i64), PyRep::Integer(1), PyRep::String("CustomsOffice".into())],
                &kw,
                None,
            )
            .await
            .expect("Export tier commodity");
        let exp_tax = exp.dict_get("exportTaxISK").and_then(|v| v.as_f64()).unwrap();
        assert!((exp_tax - base_val * 0.10).abs() < 1e-4);

        let imp = ctx
            .dispatch_rpc(
                &mut s_trader,
                "planetMgr",
                None,
                "UserLaunchCommodities",
                &[PyRep::Integer(LOWSEC_PLANET_ID), PyRep::Integer(tid as i64), PyRep::Integer(1), PyRep::String("ImportToColony".into())],
                &kw,
                None,
            )
            .await
            .expect("Import tier commodity");
        let imp_tax = imp.dict_get("importTaxISK").and_then(|v| v.as_f64()).unwrap();
        assert!((imp_tax - 0.5 * base_val * 0.10).abs() < 1e-4);
    }
    let corp_bal_after = ctx.db.with_snapshot(|snap| snap.corporations[&owner_corp].master_wallet);
    assert!(corp_bal_after.0 > corp_bal_before.0);

    // 4. Reinforce and destroy POCO via a single shield reinforcement timer
    let mut ext = StructureExtendedState::default();
    let now_u = ctx.now_unix();
    let dmg_1 = ctx.db.with_mut_snapshot(|snap| {
        life::apply_structure_damage(&ctx, snap, &mut ext, orbital_id, 50_000_000.0, 5.0, owner_char, now_u)
    }).expect("Apply shield damage to POCO");
    assert_eq!(dmg_1.state, life::STATE_ARMOR_REINFORCED);

    let rf_until = ctx.db.with_snapshot(|snap| snap.structures[&orbital_id].reinforced_until_unix);
    let delta = (rf_until + 10).saturating_sub(ctx.now_unix());
    ctx.advance_virtual_clock(delta);
    let now_u2 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        life::sync_structure(&ctx, snap, &mut ext, orbital_id, now_u2);
    });
    let dmg_2 = ctx.db.with_mut_snapshot(|snap| {
        life::apply_structure_damage(&ctx, snap, &mut ext, orbital_id, 50_000_000.0, 5.0, owner_char, now_u2)
    }).expect("Apply armor damage to POCO");
    assert_eq!(dmg_2.state, life::STATE_HULL_VULNERABLE);

    let dmg_3 = ctx.db.with_mut_snapshot(|snap| {
        life::apply_structure_damage(&ctx, snap, &mut ext, orbital_id, 50_000_000.0, 5.0, owner_char, now_u2)
    }).expect("Destroy POCO after single reinforcement timer");
    assert_eq!(dmg_3.state, life::STATE_DESTROYED);
}

async fn get_lp(ctx: &ServerContext, sess: &mut SessionState, corp_id: i64) -> i64 {
    let kw = HashMap::new();
    let lps = ctx
        .dispatch_rpc(sess, "LPSvc", None, "GetAllMyCharacterWalletLPBalances", &[], &kw, None)
        .await
        .expect("GetAllMyCharacterWalletLPBalances");
    if let PyRep::List(list) = lps {
        for entry in list {
            if entry.dict_get("corpID").and_then(|v| v.as_i64()) == Some(corp_id) {
                return entry.dict_get("loyaltyPoints").and_then(|v| v.as_i64()).unwrap_or(0);
            }
        }
    }
    0
}

#[tokio::test]
async fn test_wallet_evermarks_and_item_trader() {
    // parity: feat:wallet.evermarks, feat:wallet.item-trader
    eve_testkit::attest(&["feat:wallet.evermarks", "feat:wallet.item-trader"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 709, "demo", "demo-secret", char_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_015_169);
            let ship_id = ch.active_ship_id;
            if let Some(ship) = snap.items.get_mut(&ship_id) {
                ship.location_id = 60_015_169;
            }
        }
    });
    sess.station_id = Some(60_015_169);

    let paragon_corp_id: i64 = 1_000_419;
    let iris_agent_id: i64 = 3_020_034; // IRIS - Jita

    let em_before = get_lp(&ctx, &mut sess, paragon_corp_id).await;
    assert_eq!(em_before, 0);

    let m_req = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(2), PyRep::Integer(iris_agent_id)], &kw, None)
        .await
        .expect("Request Paragon IRIS mission");
    let dest_st = m_req.dict_get("destinationStationID").and_then(|v| v.as_i64()).unwrap_or(60_015_169);
    let dest_sys = ctx.sde.stations.get(&dest_st).map(|s| s.solar_system_id).unwrap_or(JITA_SOLAR_SYSTEM_ID);
    ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(3), PyRep::Integer(iris_agent_id)], &kw, None)
        .await
        .expect("Accept Paragon IRIS mission");

    // Dock character at courier destination station with delivery cargo
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(dest_st);
            ch.solar_system_id = dest_sys;
            for it in snap.items.values_mut() {
                if it.owner_id == char_id && it.location_id != ch.active_ship_id {
                    it.location_id = dest_st;
                    it.flag = InventoryFlag::Hangar;
                }
            }
        }
    });
    sess.station_id = Some(dest_st);
    sess.solar_system_id = Some(dest_sys);

    let mission_done = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(6), PyRep::Integer(iris_agent_id)], &kw, None)
        .await
        .expect("Complete Paragon IRIS mission");
    let mission_em = mission_done.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap();
    assert!(mission_em > 0);

    // Return character to Jita 4-4 for subsequent undocking tests
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_015_169);
            ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
            let ship_id = ch.active_ship_id;
            if let Some(ship) = snap.items.get_mut(&ship_id) {
                ship.location_id = 60_015_169;
            }
        }
    });
    sess.station_id = Some(60_015_169);
    sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);

    use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
    let mk_quasar_req = |corr: &[u8], type_name: &str, payload: Vec<u8>| {
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
            application_instance_uuid: b"app-instance-001".to_vec(),
            authoritative_context: None,
            trace_context: None,
        }
    };

    // Empty SKINR InitiateRequest must NOT mint free EverMarks
    let empty_req = mk_quasar_req(
        b"corr-seq-empty",
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        Vec::new(),
    );
    let empty_rsp = ctx.quasar_gateway.handle_public_request(&sess, &empty_req, None, false, ctx.now_unix());
    assert_eq!(empty_rsp.status_code, 200);
    assert_eq!(get_lp(&ctx, &mut sess, paragon_corp_id).await, mission_em);

    // Activate a real SDE SKINR coating component item & initiate validated SKINR sequencing jobs
    let skinr_cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
    let coating_comp = skinr_cat
        .components
        .values()
        .find(|c| !c.is_pattern() && !c.associated_types.is_empty() && !c.disallowed_slots.contains(&1))
        .expect("SDE SKINR coating component");
    let (coating_item_type_id, _) = coating_comp.associated_types[0];
    let coating_item_id = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(1_000_000);
        }
        seed_item(snap, coating_item_type_id, char_id, 60_015_169, InventoryFlag::Hangar, 2, false, false, None)
    });
    let mut seq_item = Vec::new();
    encode_varint_field(1, coating_item_id as u64, &mut seq_item);
    let mut item_outer = Vec::new();
    encode_bytes_field(1, &seq_item, &mut item_outer);
    let mut consume_req = Vec::new();
    encode_bytes_field(1, &item_outer, &mut consume_req);
    encode_varint_field(2, 2, &mut consume_req);
    let consume_rsp = ctx.quasar_gateway.handle_public_request(
        &sess,
        &mk_quasar_req(b"corr-consume-01", "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest", consume_req),
        None,
        false,
        ctx.now_unix(),
    );
    assert_eq!(consume_rsp.status_code, 200, "Consume coating item: {}", consume_rsp.status_message);

    let build_skinr_init = |design_name: &str| {
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
        encode_varint_field(1, 587, &mut seq_hull); // Rifter
        let mut layout = Vec::new();
        encode_bytes_field(1, &slot_msg, &mut layout);
        encode_bytes_field(2, &seq_hull, &mut layout);
        encode_varint_field(3, 1, &mut layout);
        let mut init_req = Vec::new();
        encode_string_field(1, design_name, &mut init_req);
        encode_string_field(2, "Paragon Line", &mut init_req);
        encode_bytes_field(3, &layout, &mut init_req);
        encode_varint_field(5, 1, &mut init_req);
        init_req
    };

    let rsp1 = ctx.quasar_gateway.handle_public_request(
        &sess,
        &mk_quasar_req(b"corr-seq-job01", "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", build_skinr_init("Valkyrie I")),
        None,
        false,
        ctx.now_unix(),
    );
    assert_eq!(rsp1.status_code, 200, "SKINR sequencing job status: {}", rsp1.status_message);
    assert_eq!(get_lp(&ctx, &mut sess, paragon_corp_id).await, mission_em + 500);

    let rsp2 = ctx.quasar_gateway.handle_public_request(
        &sess,
        &mk_quasar_req(b"corr-seq-job02", "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", build_skinr_init("Valkyrie II")),
        None,
        false,
        ctx.now_unix(),
    );
    assert_eq!(rsp2.status_code, 200);
    let em_earned = get_lp(&ctx, &mut sess, paragon_corp_id).await;
    assert_eq!(em_earned, mission_em + 1_000);

    // Query Paragon V24 SDE EverMarks Ship Cosmetic Emblem catalog without synthetic seeding
    let paragon_offers = ctx
        .dispatch_rpc(&mut sess, "LPStoreMgr", None, "GetAvailableOffersFromCorp", &[PyRep::Integer(paragon_corp_id)], &kw, None)
        .await
        .expect("Paragon SDE EverMarks offers");
    let PyRep::List(offer_list) = paragon_offers else { panic!("Expected List of Paragon offers") };
    assert!(offer_list.len() > 800, "Expected >800 V24 SDE Paragon ship emblem offers, got {}", offer_list.len());
    let rifter_emblem_offer_id = 600_000 + 72_964;
    let rifter_emblem_cost = offer_list
        .iter()
        .find(|o| o.dict_get("offerID") == Some(&PyRep::Integer(rifter_emblem_offer_id)) && o.dict_get("typeID") == Some(&PyRep::Integer(72964)))
        .and_then(|o| o.dict_get("lpCost"))
        .and_then(|v| v.as_i64())
        .expect("Rifter emblem offer in Paragon catalog");
    assert!(rifter_emblem_cost > 0, "DERIVED Paragon emblem EverMarks price must be positive");

    let take_emblem = ctx
        .dispatch_rpc(
            &mut sess,
            "LPStoreMgr",
            None,
            "TakeOfferForCharacter",
            &[PyRep::Integer(paragon_corp_id), PyRep::Integer(rifter_emblem_offer_id), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("Redeem EverMarks for ship emblem at Paragon store");
    assert_eq!(take_emblem.dict_get("deliveredTypeID"), Some(&PyRep::Integer(72964)));
    assert_eq!(take_emblem.dict_get("remainingLP"), Some(&PyRep::Integer(em_earned - rifter_emblem_cost)));

    // 2. In-space Item Trader (`beyonce.CallComponentFromClient` -> `itemTrader.ProcessTrade`)
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock into Jita space");
    let ship_id = sess.ship_id.expect("Active ship in space");
    let sys_handle = ctx.systems.get_or_spawn(JITA_SOLAR_SYSTEM_ID);
    assert!(sys_handle.set_ball_position(ship_id, glam::DVec3::ZERO).await);

    let trader_entity_id = ctx.db.with_mut_snapshot(|snap| {
        let tid = seed_item(snap, 86278, 1_000_125, JITA_SOLAR_SYSTEM_ID as i64, InventoryFlag::Hangar, 1, true, false, None);
        snap.item_positions.insert(tid, [40_000.0, 0.0, 0.0]);
        tid
    });

    let range_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CallComponentFromClient",
            &[
                PyRep::Integer(trader_entity_id),
                PyRep::String("itemTrader".into()),
                PyRep::String("ProcessTrade".into()),
                PyRep::Integer(67),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Item trader outside interactionRange must fail");
    assert_eq!(range_err.code, "ItemTraderOutOfRange");

    assert!(sys_handle.set_ball_position(ship_id, glam::DVec3::new(15_000.0, 0.0, 0.0)).await);

    let bad_recipe_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CallComponentFromClient",
            &[
                PyRep::Integer(trader_entity_id),
                PyRep::String("itemTrader".into()),
                PyRep::String("ProcessTrade".into()),
                PyRep::Integer(1),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Recipe not offered by trader must fail");
    assert_eq!(bad_recipe_err.code, "RecipeNotAvailableAtTrader");

    let missing_in_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CallComponentFromClient",
            &[
                PyRep::Integer(trader_entity_id),
                PyRep::String("itemTrader".into()),
                PyRep::String("ProcessTrade".into()),
                PyRep::Integer(67),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Missing input items must fail");
    assert_eq!(missing_in_err.code, "InsufficientInputItems");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(50_000_000);
        }
        seed_item(snap, 85316, char_id, ship_id, InventoryFlag::Cargo, 30, false, false, None);
    });
    let bal_before = ctx.db.get_character(char_id).unwrap().balance;
    let trade_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CallComponentFromClient",
            &[
                PyRep::Integer(trader_entity_id),
                PyRep::String("itemTrader".into()),
                PyRep::String("ProcessTrade".into()),
                PyRep::Integer(67),
                PyRep::Integer(2),
            ],
            &kw,
            None,
        )
        .await
        .expect("Execute 2x recipe 67 at Item Trader");
    assert!(matches!(trade_ok, PyRep::Tuple(ref items) if items.first() == Some(&PyRep::Bool(true))));

    let bal_after = ctx.db.get_character(char_id).unwrap().balance;
    assert_eq!(bal_before.0 - bal_after.0, 0);
    let (rem_in, out_qty) = ctx.db.with_snapshot(|snap| {
        let r_in: i32 = snap
            .items
            .values()
            .filter(|i| i.owner_id == char_id && i.location_id == ship_id && i.type_id == 85316)
            .map(|i| i.quantity)
            .sum();
        let r_out: i32 = snap
            .items
            .values()
            .filter(|i| i.owner_id == char_id && i.location_id == ship_id && i.type_id == 86272)
            .map(|i| i.quantity)
            .sum();
        (r_in, r_out)
    });
    assert_eq!(rem_in, 0);
    assert_eq!(out_qty, 2);
}
