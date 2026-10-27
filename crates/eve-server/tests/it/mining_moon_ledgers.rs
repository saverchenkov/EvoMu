//! Phase 5M Survey Scanner & Compression (`MN-10, MN-11`).
//!
//! Split into `mining_moon_ledgers_part2.rs` and `mining_moon_ledgers_part3.rs` so each test file stays under 685 lines.

#![forbid(unsafe_code)]

use eve_db::{
    AsteroidRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[path = "mining_moon_ledgers_part2.rs"]
mod mining_moon_ledgers_part2;
#[path = "mining_moon_ledgers_part3.rs"]
mod mining_moon_ledgers_part3;

pub(crate) fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

pub(crate) fn make_structure(
    structure_id: i64,
    owner_corp_id: i64,
    type_id: u32,
    solar_system_id: u32,
    name: &str,
    position: [f64; 3],
) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id,
        owner_corp_id,
        type_id,
        name: name.into(),
        solar_system_id,
        position,
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 100,
        shield_hp: 1000.0,
        armor_hp: 1000.0,
        hull_hp: 1000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

pub(crate) async fn setup_test_session(ctx: &ServerContext, char_id: i64) -> SessionState {
    if char_id != 90_000_001 {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.user_id = 1;
                ch.clone_grade = 2;
            } else if let Some(mut base_char) = snap.characters.get(&90_000_001).cloned() {
                base_char.character_id = char_id;
                base_char.user_id = 1;
                base_char.name = format!("Pilot {char_id}");
                base_char.station_id = Some(60_003_760);
                base_char.structure_id = None;
                base_char.solar_system_id = 30_000_142;
                snap.next_entity_id += 1;
                let new_ship_id = snap.next_entity_id;
                base_char.active_ship_id = new_ship_id;
                snap.characters.insert(char_id, base_char);
                snap.items.insert(
                    new_ship_id,
                    make_item(new_ship_id, 606, char_id, 60_003_760, InventoryFlag::Hangar, 1, true),
                );
            }
        });
    }
    let kw = HashMap::new();
    let mut sess = SessionState::new(1001);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
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

    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        for skill_id in [
            3386, 3410, 17940, 22551, 16281, 25544, 3438, 3439, 22541, 3436, 3385, 3389, 12196,
            18025, 12195, 18024, 60377, 22578, 3318, 3412, 28585, 58956, 62450, 62451, 62452,
            62453, 62454, 22536, 22552, 3348, 3426, 3413, 46152, 46153, 46154, 46155, 46156,
        ] {
            skills.insert(
                skill_id,
                TrainedSkillRecord {
                    type_id: skill_id,
                    level: 5,
                    skill_points: 500_000.0,
                },
            );
        }
    });
    sess
}

pub(crate) fn get_dict_val<'a>(dict: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match dict {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
            .map(|(_, v)| v),
        _ => None,
    }
}

pub(crate) fn decode_crowset_rows(rep: &PyRep) -> Vec<Vec<PyRep>> {
    if let PyRep::ObjectEx { list, .. } = rep {
        return list
            .iter()
            .filter_map(|r| match r {
                PyRep::DBRow { columns, .. } => {
                    Some(columns.iter().map(|(_, v)| v.clone()).collect())
                }
                PyRep::List(values) | PyRep::Tuple(values) => Some(values.clone()),
                _ => None,
            })
            .collect();
    }
    Vec::new()
}

// ====================================================================
// Test 7: MN-10: Survey Scanner Results
// ====================================================================
#[tokio::test]
async fn test_mn10_survey_scanner() {
    // parity: feat:modules.survey-scanner, feat:mining.survey-scan, rpc:miningScanMgr.perform_scan
    eve_testkit::attest(&["feat:modules.survey-scanner", "feat:mining.survey-scan"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let undock = ctx.dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    let ast1_id = 750_000_130i64;
    let ast2_id = 750_000_131i64;
    let ast_far_id = 750_000_132i64;
    for (aid, tid, qty, dx, r) in [
        (ast1_id, 1230u32, 15_000, 300.0, 50.0),
        (ast2_id, 1228u32, 8_000, 500.0, 60.0),
        (ast_far_id, 1224u32, 25_000, 50_000.0, 75.0),
    ] {
        ctx.db.spawn_asteroid(AsteroidRecord {
            asteroid_id: aid,
            solar_system_id: 30000142,
            belt_id: 40000001,
            type_id: tid,
            quantity: qty,
            initial_quantity: qty,
            position: (ship_ball.position.x + dx, ship_ball.position.y, ship_ball.position.z),
            radius: r,
        });
    }

    let scanner_id = 800_000_130i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, item| item.location_id != ship_id || matches!(item.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedOreHold));
        snap.items.insert(scanner_id, make_item(scanner_id, 444, char_id, ship_id, InventoryFlag::MedSlot0, 1, true));
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    ctx.dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(scanner_id), PyRep::String("surveyScan".into())], &kw, None)
        .await
        .expect("dogmaIM.Activate Survey Scanner");

    let scan_res = ctx
        .dispatch_rpc(&mut session, "miningScanMgr", None, "perform_scan", &[PyRep::Integer(scanner_id)], &kw, None)
        .await
        .expect("miningScanMgr.perform_scan");
    let PyRep::List(asteroids) = get_dict_val(&scan_res, "asteroids").unwrap() else { panic!("Expected asteroids list") };
    let veld_entry = asteroids
        .iter()
        .find(|a| get_dict_val(a, "asteroidID").and_then(|v| v.as_i64()) == Some(ast1_id))
        .expect("ast1 in scan results");
    assert_eq!(get_dict_val(veld_entry, "quantity").and_then(|v| v.as_i64()), Some(15_000));
    assert_eq!(get_dict_val(veld_entry, "typeID").and_then(|v| v.as_i64()), Some(1230));
    let scord_entry = asteroids
        .iter()
        .find(|a| get_dict_val(a, "asteroidID").and_then(|v| v.as_i64()) == Some(ast2_id))
        .expect("ast2 in scan results");
    assert_eq!(get_dict_val(scord_entry, "quantity").and_then(|v| v.as_i64()), Some(8_000));
    assert_eq!(get_dict_val(scord_entry, "typeID").and_then(|v| v.as_i64()), Some(1228));
    let found_far = asteroids.iter().any(|a| get_dict_val(a, "asteroidID").and_then(|v| v.as_i64()) == Some(ast_far_id));
    assert_eq!(found_far, false, "Survey scanner must exclude out-of-range asteroid at 50 km (> 15 km range)");
}

// ====================================================================
// Test 8: MN-11: Compression In Space & Structure, Gas Decompression
// ====================================================================
#[tokio::test]
async fn test_mn11_compression_space_and_structure_and_gas() {
    // parity: feat:mining.compression-space, feat:mining.compression-structure, feat:mining.gas-decompression, rpc:inSpaceCompressionMgr.CompressItemInSpace, rpc:structureCompressionMgr.CompressItemInStructure, rpc:structureCompressionMgr.DecompressGasInStructure, rpc:structureCompressionMgr.GetMyGasDecompressionEfficiency
    eve_testkit::attest(&["feat:mining.compression-space", "feat:mining.compression-structure", "feat:mining.gas-decompression"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let undock = ctx.dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();

    let veld_item_id = 800_000_140i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(veld_item_id, make_item(veld_item_id, 1230, char_id, ship_id, InventoryFlag::SpecializedOreHold, 1_234, false));
    });

    let err_no_comp = ctx
        .dispatch_rpc(&mut session, "inSpaceCompressionMgr", None, "CompressItemInSpace", &[PyRep::Integer(veld_item_id)], &kw, None)
        .await
        .expect_err("In-space compression without active compressor must fail");
    assert_eq!(err_no_comp.code, "NoActiveCompressorInRange");

    let comp_mod_id = 800_000_142i64;
    let core_mod_id = 800_000_145i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 28606; // Orca
        }
        snap.items.retain(|_, item| item.location_id != ship_id || item.flag == InventoryFlag::SpecializedOreHold);
        snap.items.insert(comp_mod_id, make_item(comp_mod_id, 62625, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(core_mod_id, make_item(core_mod_id, 58945, char_id, ship_id, InventoryFlag::HiSlot1, 1, true));
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let no_core_act_err = ctx
        .dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(comp_mod_id), PyRep::String("compressor".into())], &kw, None)
        .await
        .expect_err("Activating compressor without active Industrial Core must fail");
    assert_eq!(no_core_act_err.code, "IndustrialCoreNotActive");

    ctx.dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(core_mod_id), PyRep::String("industrialCore".into())], &kw, None)
        .await
        .expect("Activate Large Industrial Core I");
    ctx.dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(comp_mod_id), PyRep::String("compressor".into())], &kw, None)
        .await
        .expect("Activate Large Asteroid Ore Compressor I");

    let comp_space_res = ctx
        .dispatch_rpc(&mut session, "inSpaceCompressionMgr", None, "CompressItemInSpace", &[PyRep::Integer(veld_item_id)], &kw, None)
        .await
        .expect("CompressItemInSpace with active Orca compressor");
    assert_eq!(get_dict_val(&comp_space_res, "compressedUnits").unwrap().as_i64(), Some(1_234));
    assert_eq!(get_dict_val(&comp_space_res, "remainderUnits").unwrap().as_i64(), Some(0));

    let fleetmate_char_id = 90_000_002i64;
    let fleetmate_ship_id = 800_000_148i64;
    let fleetmate_ore_id = 800_000_149i64;
    let sys_id = session.solar_system_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(fch) = snap.characters.get_mut(&fleetmate_char_id) {
            fch.solar_system_id = sys_id;
            fch.station_id = None;
            fch.structure_id = None;
            fch.active_ship_id = fleetmate_ship_id;
        }
        snap.items.insert(fleetmate_ship_id, make_item(fleetmate_ship_id, 32880, fleetmate_char_id, sys_id as i64, InventoryFlag::Hangar, 1, true));
        snap.items.insert(fleetmate_ore_id, make_item(fleetmate_ore_id, 1228, fleetmate_char_id, fleetmate_ship_id, InventoryFlag::SpecializedOreHold, 500, false));
    });
    let fleet_init = ctx.dispatch_rpc(&mut session, "fleetSvc", None, "InitFleet", &[], &kw, None).await.expect("fleetSvc.InitFleet");
    let fleet_id = get_dict_val(&fleet_init, "fleetID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(fl) = snap.fleets.get_mut(&fleet_id) {
            if !fl.members.contains(&fleetmate_char_id) {
                fl.members.push(fleetmate_char_id);
            }
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let orca_pos = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: fleetmate_ship_id, type_id: 32880, position: orca_pos + glam::DVec3::new(120_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_200_000.0, radius: 40.0, speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let mut fleetmate_sess = SessionState::new(10_991);
    fleetmate_sess.user_id = Some(2);
    fleetmate_sess.character_id = Some(fleetmate_char_id);
    fleetmate_sess.solar_system_id = Some(sys_id);
    fleetmate_sess.ship_id = Some(fleetmate_ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sk) = snap.skills.get_mut(&char_id) {
            sk.remove(&62453);
        }
    });

    let out_of_range_err = ctx
        .dispatch_rpc(&mut fleetmate_sess, "inSpaceCompressionMgr", None, "CompressItemInSpace", &[PyRep::Integer(fleetmate_ore_id)], &kw, None)
        .await
        .expect_err("Fleetmate at 120 km must be out of 83 km Large Compressor range");
    assert_eq!(out_of_range_err.code, "NoActiveCompressorInRange");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(62453, TrainedSkillRecord { type_id: 62453, level: 5, skill_points: 512_000.0 });
    });
    let in_range_res = ctx
        .dispatch_rpc(&mut fleetmate_sess, "inSpaceCompressionMgr", None, "CompressItemInSpace", &[PyRep::Integer(fleetmate_ore_id)], &kw, None)
        .await
        .expect("Fleetmate at 120 km must succeed with Fleet Compression Logistics V");
    assert_eq!(get_dict_val(&in_range_res, "compressedUnits").unwrap().as_i64(), Some(500));

    // 2. Structure compression (`feat:mining.compression-structure`) and gas decompression (`feat:mining.gas-decompression`):
    let structure_id = 1_000_000_001_001i64;
    let struct_ore_id = 800_000_143i64;
    let gas_item_id = 800_000_141i64;
    let reproc_svc_mod_id = 800_000_144i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(structure_id, make_structure(structure_id, 1000001, 35835, 30000144, "Perimeter Mining Hub", [0.0, 0.0, 0.0]));
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(structure_id);
        }
        snap.items.insert(struct_ore_id, make_item(struct_ore_id, 1228, char_id, structure_id, InventoryFlag::Hangar, 500, false));
        snap.items.insert(gas_item_id, make_item(gas_item_id, 62399, char_id, structure_id, InventoryFlag::Hangar, 200, false));
        snap.items.insert(reproc_svc_mod_id, make_item(reproc_svc_mod_id, 35892, 1000001, structure_id, InventoryFlag::Hangar, 1, true));
    });
    session.station_id = None;
    session.structure_id = Some(structure_id);

    let offline_comp_err = ctx
        .dispatch_rpc(&mut session, "structureCompressionMgr", None, "CompressItemInStructure", &[PyRep::Integer(struct_ore_id)], &kw, None)
        .await
        .expect_err("Structure compression must fail when reprocessing service module is unfitted in hangar");
    assert_eq!(offline_comp_err.code, "ReprocessingServiceOffline");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.items.get_mut(&reproc_svc_mod_id) {
            m.flag = InventoryFlag::ServiceSlot0;
        }
    });

    let comp_struct_res = ctx
        .dispatch_rpc(&mut session, "structureCompressionMgr", None, "CompressItemInStructure", &[PyRep::Integer(struct_ore_id)], &kw, None)
        .await
        .expect("CompressItemInStructure");
    assert_eq!(get_dict_val(&comp_struct_res, "compressedUnits").unwrap().as_i64(), Some(500));

    let eff_res = ctx
        .dispatch_rpc(&mut session, "structureCompressionMgr", None, "GetMyGasDecompressionEfficiency", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("GetMyGasDecompressionEfficiency");
    assert_eq!(eff_res.as_f64(), Some(0.89));

    let decomp_res = ctx
        .dispatch_rpc(&mut session, "structureCompressionMgr", None, "DecompressGasInStructure", &[PyRep::Integer(gas_item_id)], &kw, None)
        .await
        .expect("DecompressGasInStructure");
    assert_eq!(get_dict_val(&decomp_res, "decompressedUnits").unwrap().as_i64(), Some(178));
}
