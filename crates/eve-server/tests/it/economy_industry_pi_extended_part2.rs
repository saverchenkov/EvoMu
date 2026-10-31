//! Extended Planetary Interaction, Customs Offices, and Wallet/Item Trader parity tests (Part 2).

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const JITA_PLANET_ID: i64 = 40_009_082; // Jita IV (Gas)

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
    let non_fac_err = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(store_id), PyRep::Integer(66)])]),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Installing schematic on Storage pin must fail");
    assert_eq!(non_fac_err.code, "InvalidSchematic");

    let find_schem = |out_tid: u32, pin_tid: u32| -> i64 {
        ctx.sde
            .planet_schematics
            .iter()
            .find(|(_, s)| {
                (s.pins.is_empty() || s.pins.contains(&pin_tid))
                    && s.types.iter().any(|t| !t.is_input && t.type_id == out_tid)
            })
            .map(|(&id, _)| i64::from(id))
            .unwrap()
    };
    let nano_schem = find_schem(2875, 2482);
    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(basic_id), PyRep::Integer(find_schem(3645, 2492))]),
                PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(adv_id), PyRep::Integer(find_schem(9832, 2494))]),
                PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(htech_id), PyRep::Integer(nano_schem)]),
            ]),
        ],
        &kw,
        None,
    )
    .await
    .expect("Install P1, P2, and P4 schematics");

    let nano_inputs: Vec<(u32, u64)> = ctx
        .sde
        .planet_schematics
        .get(&(nano_schem as u32))
        .expect("Nano-Factory schematic in SDE")
        .types
        .iter()
        .filter(|t| t.is_input)
        .map(|t| (t.type_id, t.quantity))
        .collect();

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
