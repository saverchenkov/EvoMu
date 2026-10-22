//! Phase 5M Mining Drones, Survey Scanner, Compression, Reprocessing, Moon Mining & Ledgers (`MN-09..MN-14, MN-16, C-6`).
//!
//! Split from `tests/mining_complete.rs` so both test files remain under 1,500 lines (`AGENTS.md:85`).

#![forbid(unsafe_code)]

use eve_db::{
    AsteroidRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;
use std::collections::HashMap;

fn make_item(
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

fn make_structure(
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
    }
}

async fn setup_test_session(ctx: &ServerContext, char_id: i64) -> SessionState {
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

fn get_dict_val<'a>(dict: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match dict {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
            .map(|(_, v)| v),
        _ => None,
    }
}

fn decode_crowset_rows(rep: &PyRep) -> Vec<Vec<PyRep>> {
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
// Test 6: MN-09: Mining Drones & Excavators Yield
// ====================================================================
#[tokio::test]
async fn test_mn09_mining_drones_and_excavators() {
    // parity: feat:drones.mining-drones, feat:mining.mining-drones-yield, feat:mining.excavator-drones, rpc:entity.CmdMineRepeatedly, rpc:entity.CmdReturnBay, rpc:ship.LaunchDrones
    eve_testkit::attest(&["feat:drones.mining-drones", "feat:mining.mining-drones-yield", "feat:mining.excavator-drones"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let undock = ctx.dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    let ast_id = 750_000_112i64;
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: ast_id,
        solar_system_id: 30000142,
        belt_id: 40000001,
        type_id: 1230,
        quantity: 60_000,
        initial_quantity: 60_000,
        position: (ship_ball.position.x + 400.0, ship_ball.position.y, ship_ball.position.z),
        radius: 60.0,
    });
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: ast_id,
            belt_id: 40000001,
            type_id: 1230,
            initial_units: 60_000,
            remaining_units: 60_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: ast_id,
            type_id: 1230,
            position: ship_ball.position + DVec3::new(400.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e10,
            radius: 60.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();

    // 1. Mining Drone II (type 10250, group 101)
    let drone_id = 800_000_120i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 17478; // Retriever
        }
        snap.items.insert(
            drone_id,
            make_item(drone_id, 10250, char_id, ship_id, InventoryFlag::DroneBay, 1, true),
        );
    });

    let err_in_bay = ctx
        .dispatch_rpc(&mut session, "entity", None, "CmdMineRepeatedly", &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)], &kw, None)
        .await
        .expect_err("Mining drone in DroneBay cannot mine until launched");
    assert_eq!(err_in_bay.code, "DroneNotInSpace");

    ctx.dispatch_rpc(&mut session, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(drone_id)])], &kw, None)
        .await
        .expect("ship.LaunchDrones");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(skills) = snap.skills.get_mut(&char_id) {
            skills.insert(3438, TrainedSkillRecord { type_id: 3438, level: 1, skill_points: 1000.0 });
            skills.remove(&3439);
            skills.remove(&3436);
            skills.remove(&22541);
        }
    });
    let low_drone_res = ctx
        .dispatch_rpc(&mut session, "entity", None, "CmdMineRepeatedly", &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)], &kw, None)
        .await
        .expect("entity.CmdMineRepeatedly low skills");
    let low_mined_units = get_dict_val(&low_drone_res, "minedUnits").unwrap().as_i64().unwrap();

    let rig_id = 800_000_122i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(3438, TrainedSkillRecord { type_id: 3438, level: 5, skill_points: 500_000.0 });
        skills.insert(3436, TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 500_000.0 });
        skills.insert(22541, TrainedSkillRecord { type_id: 22541, level: 5, skill_points: 500_000.0 });
        snap.items.insert(
            rig_id,
            make_item(rig_id, 25918, char_id, ship_id, InventoryFlag::RigSlot0, 1, true),
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let drone_res = ctx
        .dispatch_rpc(&mut session, "entity", None, "CmdMineRepeatedly", &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)], &kw, None)
        .await
        .expect("entity.CmdMineRepeatedly high skills + rig");
    let mined_units = get_dict_val(&drone_res, "minedUnits").unwrap().as_i64().unwrap();
    let is_drone = get_dict_val(&drone_res, "isDrone").unwrap().as_bool().unwrap();
    assert!(is_drone, "Must be flagged as drone mining");
    assert!(mined_units > low_mined_units, "Skills + rig must boost drone yield");
    assert!(mined_units >= 300, "Mining Drone II with V/V skills must yield >= 300 units");

    ctx.dispatch_rpc(&mut session, "entity", None, "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(drone_id)])], &kw, None)
        .await
        .expect("entity.CmdReturnBay");

    // 2. 'Excavator' Mining Drone (type 41030, `feat:mining.excavator-drones`):
    let exc_drone_id = 800_000_121i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            exc_drone_id,
            make_item(exc_drone_id, 41030, char_id, ship_id, InventoryFlag::DroneBay, 1, true),
        );
    });

    ctx.dispatch_rpc(&mut session, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(exc_drone_id)])], &kw, None)
        .await
        .expect("Launch Excavator drone");

    let err_non_rorq = ctx
        .dispatch_rpc(&mut session, "entity", None, "CmdMineRepeatedly", &[PyRep::List(vec![PyRep::Integer(exc_drone_id)]), PyRep::Integer(ast_id)], &kw, None)
        .await
        .expect_err("Excavator must require a Rorqual hull");
    assert_eq!(err_non_rorq.code, "InvalidShipForExcavator");

    let ind_core_id = 800_000_123i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 28352; // Rorqual
        }
        snap.items.retain(|_, item| item.location_id != ship_id || item.flag == InventoryFlag::DroneBay);
        snap.items.insert(
            ind_core_id,
            make_item(ind_core_id, 28583, char_id, ship_id, InventoryFlag::HiSlot0, 1, true),
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let err_no_core = ctx
        .dispatch_rpc(&mut session, "entity", None, "CmdMineRepeatedly", &[PyRep::List(vec![PyRep::Integer(exc_drone_id)]), PyRep::Integer(ast_id)], &kw, None)
        .await
        .expect_err("Excavator must require active Industrial Core on Rorqual");
    assert_eq!(err_no_core.code, "IndustrialCoreRequired");

    ctx.dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(ind_core_id), PyRep::String("industrialCore".into())], &kw, None)
        .await
        .expect("Activate Capital Industrial Core I");

    let exc_res = ctx
        .dispatch_rpc(&mut session, "entity", None, "CmdMineRepeatedly", &[PyRep::List(vec![PyRep::Integer(exc_drone_id)]), PyRep::Integer(ast_id)], &kw, None)
        .await
        .expect("entity.CmdMineRepeatedly with Excavator on active-core Rorqual");
    let exc_mined = get_dict_val(&exc_res, "minedUnits").unwrap().as_i64().unwrap();
    assert!(exc_mined > mined_units, "Excavator drone must produce vastly higher yield than T2 mining drone");
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
        snap.items.insert(
            scanner_id,
            make_item(scanner_id, 444, char_id, ship_id, InventoryFlag::MedSlot0, 1, true),
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    ctx.dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(scanner_id), PyRep::String("surveyScan".into())], &kw, None)
        .await
        .expect("dogmaIM.Activate Survey Scanner");

    let scan_res = ctx
        .dispatch_rpc(&mut session, "miningScanMgr", None, "perform_scan", &[PyRep::Integer(scanner_id)], &kw, None)
        .await
        .expect("miningScanMgr.perform_scan");
    let PyRep::List(asteroids) = get_dict_val(&scan_res, "asteroids").unwrap() else {
        panic!("Expected asteroids list");
    };
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
        snap.items.insert(
            veld_item_id,
            make_item(veld_item_id, 1230, char_id, ship_id, InventoryFlag::SpecializedOreHold, 1_234, false),
        );
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
        snap.items.insert(
            comp_mod_id,
            make_item(comp_mod_id, 62625, char_id, ship_id, InventoryFlag::HiSlot0, 1, true),
        );
        snap.items.insert(
            core_mod_id,
            make_item(core_mod_id, 58945, char_id, ship_id, InventoryFlag::HiSlot1, 1, true),
        );
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
        snap.items.insert(
            fleetmate_ship_id,
            make_item(fleetmate_ship_id, 32880, fleetmate_char_id, sys_id as i64, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            fleetmate_ore_id,
            make_item(fleetmate_ore_id, 1228, fleetmate_char_id, fleetmate_ship_id, InventoryFlag::SpecializedOreHold, 500, false),
        );
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
            id: fleetmate_ship_id,
            type_id: 32880,
            position: orca_pos + glam::DVec3::new(120_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
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
        snap.skills.entry(char_id).or_default().insert(
            62453,
            TrainedSkillRecord { type_id: 62453, level: 5, skill_points: 512_000.0 },
        );
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
        snap.structures.insert(
            structure_id,
            make_structure(structure_id, 1000001, 35835, 30000144, "Perimeter Mining Hub", [0.0, 0.0, 0.0]),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(structure_id);
        }
        snap.items.insert(
            struct_ore_id,
            make_item(struct_ore_id, 1228, char_id, structure_id, InventoryFlag::Hangar, 500, false),
        );
        snap.items.insert(
            gas_item_id,
            make_item(gas_item_id, 62399, char_id, structure_id, InventoryFlag::Hangar, 200, false),
        );
        snap.items.insert(
            reproc_svc_mod_id,
            make_item(reproc_svc_mod_id, 35892, 1000001, structure_id, InventoryFlag::Hangar, 1, true),
        );
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

// ====================================================================
// Test 9: MN-12, C-6: Real Reprocessing Yield, Tax, Scrapmetal, Structure & Rigs
// ====================================================================
#[tokio::test]
async fn test_mn12_c6_reprocessing_yield_tax_modules_and_structure() {
    // parity: feat:mining.reprocessing-yield, feat:mining.reprocessing-tax, feat:mining.reprocessing-modules, feat:structures.service-reprocessing, feat:mining.upwell-refinery-base-yield, feat:structures.reprocessing-rig-security-multipliers
    eve_testkit::attest(&["feat:mining.reprocessing-yield", "feat:mining.reprocessing-tax", "feat:mining.reprocessing-modules", "feat:structures.service-reprocessing", "feat:mining.upwell-refinery-base-yield", "feat:structures.reprocessing-rig-security-multipliers"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let station_id = 60003760i64; // Jita IV - 4 (owner corp 1000035 Caldari Navy)
    session.station_id = Some(station_id);

    // 1. Reprocess 1,050 Veldspar (portionSize 100 = 10 portions + 50 remainder) with NPC standing 2.0
    let veld_item_id = 800_000_150i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1000035, 2.0);
        }
        snap.items.insert(
            veld_item_id,
            make_item(veld_item_id, 1230, char_id, station_id, InventoryFlag::Hangar, 1_050, false),
        );
    });

    let reproc_res = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(veld_item_id)], &kw, None)
        .await
        .expect("reprocessingSvc.Reprocess");
    let yield_rate = get_dict_val(&reproc_res, "yieldRate").unwrap().as_f64().unwrap();
    let trit_qty = get_dict_val(&reproc_res, "tritaniumQuantity").unwrap().as_i64().unwrap();
    let tax_rate = get_dict_val(&reproc_res, "stationTaxRate").unwrap().as_f64().unwrap();
    let expected_yield: f64 = 0.50 * 1.15 * 1.10 * 1.10; // 0.69575
    let expected_tax: f64 = 0.05 - 0.0075 * 2.0; // 0.035
    let expected_trit = (10.0_f64 * 400.0 * expected_yield * (1.0 - expected_tax)).floor() as i64; // 2685
    assert!((yield_rate - expected_yield).abs() < 1e-6);
    assert!((tax_rate - expected_tax).abs() < 1e-6);
    assert_eq!(trit_qty, expected_trit);

    let (rem_veld_qty, has_tax_journal) = ctx.db.with_snapshot(|snap| {
        let rem = snap.items.get(&veld_item_id).map(|it| it.quantity).unwrap_or(0);
        let has_journal = snap
            .wallet_journal
            .iter()
            .any(|e| e.character_id == char_id && e.entry_type == "ReprocessingTax" && e.amount.0 > 0);
        (rem, has_journal)
    });
    assert_eq!(rem_veld_qty, 50, "Non-multiple remainder of 50 Veldspar must remain in hangar");
    assert!(has_tax_journal, "NPC station reprocessing with tax > 0 must record a ReprocessingTax wallet journal entry");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1000035, 6.67);
        }
    });
    let quotes_zero_tax = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "GetQuotes", &[], &kw, None)
        .await
        .expect("GetQuotes with 6.67 standing");
    assert_eq!(get_dict_val(&quotes_zero_tax, "stationTaxRate").unwrap().as_f64(), Some(0.0));

    // 1b. Implant factor (`feat:mining.reprocessing-yield`): Zainou 'Beancounter' Reprocessing RX-804 (27174, +4% attr 379)
    let veld_imp_id = 800_000_152i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.entry(char_id).or_default().push(27174);
        snap.items.insert(
            veld_imp_id,
            make_item(veld_imp_id, 1230, char_id, station_id, InventoryFlag::Hangar, 1_050, false),
        );
    });
    let imp_res = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(veld_imp_id)], &kw, None)
        .await
        .expect("Reprocess with RX-804 implant");
    let expected_imp_yield: f64 = 0.50 * 1.15 * 1.10 * 1.10 * 1.04; // 0.72358
    assert!((get_dict_val(&imp_res, "yieldRate").unwrap().as_f64().unwrap() - expected_imp_yield).abs() < 1e-6);
    assert_eq!(
        get_dict_val(&imp_res, "tritaniumQuantity").unwrap().as_i64(),
        Some((10.0_f64 * 400.0 * expected_imp_yield).floor() as i64)
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&veld_imp_id).map(|i| i.quantity)), Some(50));
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.remove(&char_id);
    });

    // 2. Reprocess Module with Scrapmetal Processing (12196):
    let module_item_id = 800_000_151i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            module_item_id,
            make_item(module_item_id, 482, char_id, station_id, InventoryFlag::Hangar, 1, false),
        );
    });

    let mod_reproc_res = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(module_item_id)], &kw, None)
        .await
        .expect("Reprocess module");
    let mod_yield = get_dict_val(&mod_reproc_res, "yieldRate").unwrap().as_f64().unwrap();
    let expected_mod_yield: f64 = 0.50 * (1.0 + 0.02 * 5.0); // 0.55
    assert!((mod_yield - expected_mod_yield).abs() < 1e-6);
    let PyRep::List(mod_outputs) = get_dict_val(&mod_reproc_res, "outputs").unwrap() else {
        panic!("Expected outputs list from module Reprocess");
    };
    let sde_mod_mats = ctx.sde.type_materials.get(&482).expect("Miner II (482) must have SDE typeMaterials");
    for mat in sde_mod_mats {
        let expected_mat_qty = ((mat.quantity as f64) * expected_mod_yield).floor() as i64;
        if expected_mat_qty > 0 {
            let actual_mat_qty = mod_outputs
                .iter()
                .find(|o| get_dict_val(o, "typeID").and_then(|v| v.as_i64()) == Some(mat.material_type_id as i64))
                .and_then(|o| get_dict_val(o, "quantity"))
                .and_then(|v| v.as_i64());
            assert_eq!(actual_mat_qty, Some(expected_mat_qty));
        }
    }

    // 3. Upwell Structure Reprocessing Service (`feat:structures.service-reprocessing`, `feat:mining.upwell-refinery-base-yield`):
    // Verify GetQuotes AND actual Reprocess output in Astrahus (50%), Athanor (51%), and Tatara (52%)
    let structure_id = 1_000_000_001_002i64;
    let tatara_id = 1_000_000_001_012i64;
    let citadel_id = 1_000_000_001_013i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (idx, (sid, st_type, name)) in [
            (structure_id, 35835u32, "Athanor Refinery"),
            (tatara_id, 35836u32, "Tatara Refinery"),
            (citadel_id, 35825u32, "Astrahus Citadel"),
        ]
        .into_iter()
        .enumerate()
        {
            snap.structures.insert(
                sid,
                make_structure(sid, 1000001, st_type, 30000142, name, [0.0, 0.0, 0.0]),
            );
            let svc_item_id = 800_000_160 + (idx as i64) * 2;
            let ore_item_id = svc_item_id + 1;
            snap.items.insert(
                svc_item_id,
                make_item(svc_item_id, 35892, 1000001, sid, InventoryFlag::ServiceSlot0, 1, true),
            );
            snap.items.insert(
                ore_item_id,
                make_item(ore_item_id, 1230, char_id, sid, InventoryFlag::Hangar, 1_000, false),
            );
        }
    });

    for (idx, (sid, expected_base, label)) in [
        (structure_id, 0.51_f64, "Athanor (35835)"),
        (tatara_id, 0.52_f64, "Tatara (35836)"),
        (citadel_id, 0.50_f64, "Astrahus Citadel (35825)"),
    ]
    .into_iter()
    .enumerate()
    {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = None;
                ch.structure_id = Some(sid);
            }
        });
        session.station_id = None;
        session.structure_id = Some(sid);

        let struct_info = ctx
            .dispatch_rpc(&mut session, "reprocessingSvc", None, "GetQuotes", &[], &kw, None)
            .await
            .expect("GetQuotes on structure");
        let struct_base = get_dict_val(&struct_info, "stationBaseYield").unwrap().as_f64().unwrap();
        assert!(
            (struct_base - expected_base).abs() < 1e-6,
            "{label} GetQuotes must report {expected_base} base reprocessing yield (got {struct_base})"
        );

        let ore_item_id = 800_000_161 + (idx as i64) * 2;
        let struct_reproc_res = ctx
            .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(ore_item_id)], &kw, None)
            .await
            .expect("Reprocess in Upwell structure");
        let struct_yield = get_dict_val(&struct_reproc_res, "yieldRate").unwrap().as_f64().unwrap();
        let struct_tax = get_dict_val(&struct_reproc_res, "stationTaxRate").unwrap().as_f64().unwrap();
        let struct_trit = get_dict_val(&struct_reproc_res, "tritaniumQuantity").unwrap().as_i64().unwrap();
        let expected_struct_yield: f64 = expected_base * 1.15 * 1.10 * 1.10;
        assert!((struct_yield - expected_struct_yield).abs() < 1e-6);
        assert_eq!(struct_tax, 0.0, "{label} reprocessing must apply 0% NPC station tax");
        assert_eq!(struct_trit, (10.0_f64 * 400.0 * expected_struct_yield).floor() as i64);
    }

    // 4. Standup Reprocessing Rig Security Multipliers (`feat:structures.reprocessing-rig-security-multipliers`):
    // Fit Standup M-Set Asteroid Ore Grading Processor I (46633, group 1941) on Athanor (35835):
    // Verify High-Sec (1.0x -> 0.51*1.0*1.02 = 0.5202), Low-Sec (1.06x -> 0.551412), Null-Sec (1.12x -> 0.582624) on Asteroid Ore,
    // while Ice (16262 Clear Icicle) stays at unrigged 0.51 on that Athanor because M-Set 46633 is Asteroid-Ore-specific.
    let athanor_rig_id = 800_000_170i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            athanor_rig_id,
            make_item(athanor_rig_id, 46633, 1000001, structure_id, InventoryFlag::RigSlot0, 1, true),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = Some(structure_id);
        }
    });
    session.structure_id = Some(structure_id);

    for (idx, (sys_id, sec_mult)) in [
        (30000142u32, 1.0_f64),  // Jita High-Sec
        (30002537u32, 1.06_f64), // Amamake Low-Sec
        (30000208u32, 1.12_f64), // Null-Sec
    ]
    .into_iter()
    .enumerate()
    {
        let ore_id = 800_000_171 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&structure_id) {
                st.solar_system_id = sys_id;
            }
            snap.items.insert(
                ore_id,
                make_item(ore_id, 1230, char_id, structure_id, InventoryFlag::Hangar, 1_000, false),
            );
        });
        let expected_rigged_base = 0.51 * sec_mult * 1.02;
        let q = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
        let q_base = get_dict_val(&q, "stationBaseYield").unwrap().as_f64().unwrap();
        assert!((q_base - expected_rigged_base).abs() < 1e-6, "Athanor M-Set T1 rig in sys {sys_id} base yield mismatch");

        let r = ctx
            .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(ore_id)], &kw, None)
            .await
            .unwrap();
        let r_base = get_dict_val(&r, "stationBaseYield").unwrap().as_f64().unwrap();
        assert!((r_base - expected_rigged_base).abs() < 1e-6);
    }

    // Ice (16262) on Athanor with only Asteroid Ore M-Set rig (46633) stays at unrigged 0.51 base yield:
    let athanor_ice_id = 800_000_175i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            athanor_ice_id,
            make_item(athanor_ice_id, 16262, char_id, structure_id, InventoryFlag::Hangar, 10, false),
        );
    });
    let ice_on_ore_rig = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(athanor_ice_id)], &kw, None)
        .await
        .unwrap();
    assert!(
        (get_dict_val(&ice_on_ore_rig, "stationBaseYield").unwrap().as_f64().unwrap() - 0.51).abs() < 1e-6,
        "Asteroid-Ore-specific M-Set rig on Athanor must not boost Ice reprocessing yield"
    );

    // Fit Standup L-Set Reprocessing Monitor II (46640, group 1944) on Tatara (35836) in Null-Sec (30000208):
    // Unified L-Set T2 rig boosts Asteroid Ore (1230), Ice (16262), AND Moon Ore (45490) to 0.53 * 1.12 * 1.04 = 0.617344 base yield.
    let tatara_rig_id = 800_000_180i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&tatara_id) {
            st.solar_system_id = 30000208;
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = Some(tatara_id);
        }
        snap.items.insert(
            tatara_rig_id,
            make_item(tatara_rig_id, 46640, 1000001, tatara_id, InventoryFlag::RigSlot0, 1, true),
        );
    });
    session.structure_id = Some(tatara_id);
    let expected_tatara_t2_null_base = 0.53 * 1.12 * 1.04; // 0.617344
    for (idx, (type_id, qty)) in [(1230u32, 1_000), (16262u32, 10), (45490u32, 1_000)].into_iter().enumerate() {
        let item_id = 800_000_181 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                item_id,
                make_item(item_id, type_id, char_id, tatara_id, InventoryFlag::Hangar, qty, false),
            );
        });
        let res = ctx
            .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(item_id)], &kw, None)
            .await
            .unwrap();
        let base_y = get_dict_val(&res, "stationBaseYield").unwrap().as_f64().unwrap();
        assert!(
            (base_y - expected_tatara_t2_null_base).abs() < 1e-6,
            "Tatara L-Set T2 unified rig in Null-Sec must give {expected_tatara_t2_null_base} base yield for type {type_id} (got {base_y})"
        );
    }

    // 5. Structure Owner Reprocessing Tax (`feat:mining.reprocessing-tax`):
    let owner_corp_id = 98_000_777i64;
    let taxed_ore_id = 800_000_189i64;
    let journal_before = ctx.db.with_snapshot(|snap| snap.wallet_journal.len());
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            owner_corp_id,
            eve_db::CorporationRecord {
                corporation_id: owner_corp_id,
                name: "Refinery Owner Corp".into(),
                ticker: "RFOWN".into(),
                ceo_id: char_id,
                alliance_id: None,
                tax_rate: 0.03, // 3% structure reprocessing tax
                master_wallet: eve_db::IskCents(0),
                wallet_divisions: Vec::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(station_id),
                members: Default::default(),
                member_roles: Default::default(),
                applications: Default::default(),
            },
        );
        if let Some(st) = snap.structures.get_mut(&tatara_id) {
            st.owner_corp_id = owner_corp_id;
        }
        snap.items.insert(
            taxed_ore_id,
            make_item(taxed_ore_id, 1230, char_id, tatara_id, InventoryFlag::Hangar, 1_000, false),
        );
    });
    let st_tax_res = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(taxed_ore_id)], &kw, None)
        .await
        .expect("Reprocess in structure with 3% owner tax");
    assert!((get_dict_val(&st_tax_res, "stationTaxRate").unwrap().as_f64().unwrap() - 0.03).abs() < 1e-6);
    assert!(get_dict_val(&st_tax_res, "reprocessingTaxISK").unwrap().as_f64().unwrap() > 0.0);
    let (journal_added, corp_wallet) = ctx.db.with_snapshot(|snap| {
        let added = snap.wallet_journal[journal_before..]
            .iter()
            .any(|e| e.character_id == char_id && e.entry_type == "ReprocessingTax" && e.amount.0 > 0);
        let cw = snap.corporations.get(&owner_corp_id).map(|c| c.master_wallet.0).unwrap_or(0);
        (added, cw)
    });
    assert!(journal_added && corp_wallet > 0, "Structure owner reprocessing tax must record ReprocessingTax journal entry and credit owner corp wallet");
}

// ====================================================================
// Test 10a: MN-13: Moon Survey Probes, Launch Rules, Flight Time & Security-Band Moon Ores
// ====================================================================
#[tokio::test]
async fn test_mn13_moon_survey_probes_launch_alignment_and_resources() {
    // parity: feat:scanning.moon-survey-probe, feat:scanning.moon-survey-probe-launch-rules, feat:mining.moon-resources, feat:mining.moon-ore-types, feat:mining.moon-ore-class-by-security, rpc:moonExtractions.GetMoonResourcesForStructure
    eve_testkit::attest(&["feat:scanning.moon-survey-probe", "feat:scanning.moon-survey-probe-launch-rules", "feat:mining.moon-resources", "feat:mining.moon-ore-types", "feat:mining.moon-ore-class-by-security"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();
    let station_id = 60_003_760i64;

    // 1. Fitting rules (`feat:scanning.moon-survey-probe-launch-rules`): 0 hardpoints required, max 1 fitted per ship (attr 1544 == 1)
    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let sid = snap.characters.get(&char_id).map(|c| c.active_ship_id).unwrap_or(100_000_001);
        if let Some(ship) = snap.items.get_mut(&sid) {
            ship.type_id = 32880; // Venture (0 missile launcher hardpoints, 2 high slots)
        }
        snap.items.retain(|_, item| item.location_id != sid);
        snap.items.insert(800_000_301, make_item(800_000_301, 33270, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(800_000_302, make_item(800_000_302, 33272, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(800_000_303, make_item(800_000_303, 18635, char_id, sid, InventoryFlag::Cargo, 3, false));
        sid
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    ctx.dispatch_rpc(&mut session, "invbroker", None, "Add", &[PyRep::Integer(800_000_301), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::HiSlot0 as i64)], &kw, None)
        .await
        .expect("Survey Probe Launcher I fits on ship with 0 launcher hardpoints");
    let second_launcher_err = ctx
        .dispatch_rpc(&mut session, "invbroker", None, "Add", &[PyRep::Integer(800_000_302), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::HiSlot1 as i64)], &kw, None)
        .await
        .expect_err("Second Survey Probe Launcher must fail maxGroupFitted == 1");
    assert_eq!(second_launcher_err.code, "FittingValidationFailed");
    assert!(second_launcher_err.message.contains("MaxGroupFittedExceeded"));

    // 2. Undock and test launch rules: auto-reload from cargo, non-repeating cycle, non-recallable probes, miss when unaligned or > 100,000 km
    ctx.dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let _ = session.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);
    let jita = ctx.systems.get_or_spawn(30000142);
    let moon_40009087_pos = jita.get_ball_snapshot(40009087).await.unwrap().unwrap().1.position;
    let ship_near_moon = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position; // Next to Jita 4-4 station (~4,042 km from Moon 4)

    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ship_near_moon;
        sb.velocity = -(moon_40009087_pos - ship_near_moon).normalize() * 100.0; // Aligned AWAY from moon
        let _ = jita.spawn_ball(sb).await;
    }
    let miss_unaligned = ctx
        .dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009087)], &kw, None)
        .await
        .expect("Launch while aligned away");
    assert_eq!(get_dict_val(&miss_unaligned, "moonID"), Some(&PyRep::None), "Probe launched while unaligned must miss");
    assert_eq!(get_dict_val(&miss_unaligned, "flightTimeSec").unwrap().as_i64(), Some(300));
    assert_eq!(get_dict_val(&miss_unaligned, "remainingInLauncher").unwrap().as_i64(), Some(1), "Auto-reload from cargo loads 2 probes (10 m3 / 5 m3) and consumes 1");
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&800_000_301)), "Survey Probe Launcher must not auto-repeat");

    let miss_probe_id = get_dict_val(&miss_unaligned, "probeID").unwrap().as_i64().unwrap();
    let launch_notifs = session.drain_notifications();
    assert!(launch_notifs.iter().any(|n| n.0 == "OnNewMoonProbe"), "Launch must emit OnNewMoonProbe");
    assert!(!launch_notifs.iter().any(|n| n.0 == "OnMoonScanComplete"), "Results must not arrive before flight time elapses");

    let recover_res = ctx
        .dispatch_rpc(&mut session, "scanMgr", None, "RecoverProbes", &[PyRep::List(vec![PyRep::Integer(miss_probe_id)])], &kw, None)
        .await
        .expect("RecoverProbes");
    assert_eq!(get_dict_val(&recover_res, "recoveredCount").unwrap().as_i64(), Some(0), "Moon survey probes cannot be recalled after launch");

    ctx.advance_time_seconds(300);
    let miss_arr_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(miss_arr_notifs.iter().any(|n| n.event == "OnRemoveMoonProbe"), "Arrived probe must emit OnRemoveMoonProbe");
    assert!(!miss_arr_notifs.iter().any(|n| n.event == "OnMoonScanComplete"), "Missed probe must not emit OnMoonScanComplete");

    // Launch at distant moon > 100,000 km (`40009081`, Jita III - Moon 1) while aligned toward it -> misses due to > 100,000 km range & auto-reloads last cargo probe
    let moon_40009081_pos = jita.get_ball_snapshot(40009081).await.unwrap().unwrap().1.position;
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ship_near_moon;
        sb.velocity = (moon_40009081_pos - ship_near_moon).normalize() * 100.0;
        let _ = jita.spawn_ball(sb).await;
    }
    let miss_range = ctx
        .dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009081)], &kw, None)
        .await
        .expect("Launch at moon > 100,000 km");
    assert_eq!(get_dict_val(&miss_range, "moonID"), Some(&PyRep::None), "Moon beyond 100,000 km must miss");
    assert_eq!(get_dict_val(&miss_range, "remainingInLauncher").unwrap().as_i64(), Some(1), "Launcher must auto-reload last probe from cargo when emptied");
    ctx.advance_time_seconds(300);
    let _ = ctx.drain_notifications_for_char(char_id);

    // 3. T1/T2 flight times (300s / 150s / 1200s base, -10% on T2 33272), undocked-in-system requirement, and scan completion
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.items.get_mut(&800_000_301) {
            m.type_id = 33272; // Upgrade to Survey Probe Launcher II (-10% flight time)
        }
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.item_id != 800_000_301));
        snap.items.insert(800_000_304, make_item(800_000_304, 18637, char_id, ship_id, InventoryFlag::HiSlot0, 1, false)); // Gaze (150s base -> 135s T2)
        snap.items.insert(800_000_305, make_item(800_000_305, 18626, char_id, ship_id, InventoryFlag::Cargo, 1, false));   // Quest (1200s base -> 1080s T2)
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ship_near_moon;
        sb.velocity = (moon_40009087_pos - ship_near_moon).normalize() * 100.0;
        let _ = jita.spawn_ball(sb).await;
    }
    let gaze_launch = ctx
        .dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009087)], &kw, None)
        .await
        .expect("Launch Gaze Survey Probe I from T2 launcher");
    assert_eq!(get_dict_val(&gaze_launch, "moonID").unwrap().as_i64(), Some(40009087));
    assert_eq!(get_dict_val(&gaze_launch, "flightTimeSec").unwrap().as_i64(), Some(135), "Gaze (150s base) on T2 launcher (-10%) must fly 135s");

    let quest_launch = ctx
        .dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009087)], &kw, None)
        .await
        .expect("Launch Quest Survey Probe I from T2 launcher");
    assert_eq!(get_dict_val(&quest_launch, "flightTimeSec").unwrap().as_i64(), Some(1080), "Quest (1200s base) on T2 launcher (-10%) must fly 1080s");

    ctx.dispatch_rpc(&mut session, "stationSvc", None, "Dock", &[PyRep::Integer(station_id)], &kw, None).await.expect("Dock before probes arrive");
    let _ = session.drain_notifications();
    ctx.advance_time_seconds(1080);
    let docked_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(docked_notifs.iter().any(|n| n.event == "OnRemoveMoonProbe"));
    assert!(!docked_notifs.iter().any(|n| n.event == "OnMoonScanComplete"), "Pilot who docks before probe arrives must not receive OnMoonScanComplete");

    // Undock, align toward 40009087, launch Discovery probe (18635: 300s base -> 270s on T2) and remain undocked in system until 270s
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(800_000_306, make_item(800_000_306, 18635, char_id, ship_id, InventoryFlag::HiSlot0, 1, false));
    });
    ctx.dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock again");
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ship_near_moon;
        sb.velocity = (moon_40009087_pos - ship_near_moon).normalize() * 100.0;
        let _ = jita.spawn_ball(sb).await;
    }
    let _ = session.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);

    let disc_launch = ctx
        .dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009087)], &kw, None)
        .await
        .expect("Launch Discovery Survey Probe I from T2 launcher");
    assert_eq!(get_dict_val(&disc_launch, "moonID").unwrap().as_i64(), Some(40009087));
    assert_eq!(get_dict_val(&disc_launch, "flightTimeSec").unwrap().as_i64(), Some(270));
    let _ = session.drain_notifications();

    ctx.advance_time_seconds(269);
    let early_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        !early_notifs.iter().any(|n| n.event == "OnMoonScanComplete" || n.event == "OnRemoveMoonProbe"),
        "No moon probe scan results at 269s before 270s flight time completes"
    );
    ctx.advance_time_seconds(1);
    let scan_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(scan_notifs.iter().any(|n| n.event == "OnRemoveMoonProbe"), "Must emit OnRemoveMoonProbe at 270s");
    let scan_complete = scan_notifs.iter().find(|n| n.event == "OnMoonScanComplete").expect("Must emit OnMoonScanComplete at 270s when undocked in system");
    assert_eq!(scan_complete.args.first().and_then(|v| v.as_i64()), Some(40009087));
    let Some(PyRep::Dict(comp_entries)) = scan_complete.args.get(1) else {
        panic!("Expected OnMoonScanComplete payload[1] to be Dict of {{oreTypeID: abundance}}");
    };
    assert!(!comp_entries.is_empty(), "Moon scan must reveal moon ores");
    for (k, v) in comp_entries {
        let tid = k.as_i64().unwrap() as u32;
        assert!(v.as_f64().unwrap() > 0.0);
        assert_eq!(ctx.sde.types.get(&tid).map(|t| t.group_id), Some(1884), "High-Sec moon composition must only contain R4 Ubiquitous Moon Ores (group 1884)");
    }

    // Security-band ore class checks across Wormhole and Low-Sec (`feat:mining.moon-ore-types`, `feat:mining.moon-ore-class-by-security`):
    let char_corp_id = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(1000170));
    let wh_structure_id = 1_000_000_001_005i64;
    let ls_structure_id = 1_000_000_001_006i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(wh_structure_id, make_structure(wh_structure_id, char_corp_id, 35835, 31000007, "WH Athanor", [1000.0, 2000.0, 3000.0]));
        snap.structures.insert(ls_structure_id, make_structure(ls_structure_id, char_corp_id, 35835, 30000012, "LS Athanor", [1000.0, 2000.0, 3000.0]));
    });
    let PyRep::List(wh_ores) = ctx.dispatch_rpc(&mut session, "moonExtractions", None, "GetMoonResourcesForStructure", &[PyRep::Integer(wh_structure_id)], &kw, None).await.unwrap() else { panic!() };
    for entry in &wh_ores {
        let tid = get_dict_val(entry, "typeID").unwrap().as_i64().unwrap() as u32;
        assert_eq!(ctx.sde.types.get(&tid).map(|t| t.group_id), Some(1884), "Wormhole moon composition must only contain R4 Ubiquitous Moon Ores");
    }
    let PyRep::List(ls_ores) = ctx.dispatch_rpc(&mut session, "moonExtractions", None, "GetMoonResourcesForStructure", &[PyRep::Integer(ls_structure_id)], &kw, None).await.unwrap() else { panic!() };
    assert!(ls_ores.iter().any(|e| ctx.sde.types.get(&(get_dict_val(e, "typeID").unwrap().as_i64().unwrap() as u32)).is_some_and(|t| matches!(t.group_id, 1920..=1923))));
}

// ====================================================================
// Test 10b: MN-13, MN-14, MN-16: Moon Extraction, Fracture, Decay, Drill & Ledger
// ====================================================================
#[tokio::test]
async fn test_mn13_mn14_mn16_moon_mining_probes_jackpot_decay_and_ledger() {
    // parity: feat:mining.moon-extraction, feat:mining.moon-extraction-duration-yield, feat:mining.moon-fracture, feat:mining.moon-chunk-auto-fracture, feat:mining.moon-jackpot, feat:mining.moon-field-lifetime, feat:structures.service-moon-drill, feat:modules.mining-survey-chipset, feat:mining.metenox, feat:mining.corp-ledger, rpc:autoMoonMining.RequestMiningCycleOutput, rpc:autoMoonMining.RequestMiningDetails, rpc:corpMiningLedger.GetObserverLedger, rpc:corpMiningLedger.GetObserversWithMiningEvents, rpc:moonExtractions.CancelExtraction, rpc:moonExtractions.FractureChunkWithStructure, rpc:moonExtractions.GetExtractionAndEventIDFromStructureID, rpc:moonExtractions.GetExtractionsForCorp, rpc:moonExtractions.StartNewExtraction
    eve_testkit::attest(&["feat:mining.moon-extraction", "feat:mining.moon-extraction-duration-yield", "feat:mining.moon-fracture", "feat:mining.moon-chunk-auto-fracture", "feat:mining.moon-jackpot", "feat:mining.moon-field-lifetime", "feat:structures.service-moon-drill", "feat:modules.mining-survey-chipset", "feat:mining.metenox", "feat:mining.corp-ledger"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let undock = ctx.dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);

    let structure_id = 1_000_000_001_003i64;
    let metenox_id = 1_000_000_001_004i64;
    let wh_structure_id = 1_000_000_001_005i64;
    let auto_structure_id = 1_000_000_001_006i64;
    let char_corp_id = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(1000170)
    });
    ctx.db.with_mut_snapshot(|snap| {
        for (sid, sys_id, st_type, name) in [
            (structure_id, 30000142u32, 35835u32, "Moon Mining Athanor (Jita High-Sec)"),
            (metenox_id, 30000012u32, 81826u32, "Automated Metenox Drill (Asabona Low-Sec)"),
            (wh_structure_id, 31000007u32, 35835u32, "Wormhole Athanor (J102512)"),
            (auto_structure_id, 30000012u32, 35835u32, "Low-Sec Auto-Fracture Athanor"),
        ] {
            snap.structures.insert(
                sid,
                make_structure(sid, char_corp_id, st_type, sys_id, name, [1000.0, 2000.0, 3000.0]),
            );
        }
        snap.items.insert(
            800_000_305,
            make_item(800_000_305, 81143, char_corp_id, metenox_id, InventoryFlag::Hangar, 200, false),
        );
    });

    // 1. Moon Extraction duration/yield (`feat:mining.moon-extraction`, `feat:mining.moon-extraction-duration-yield`, `feat:structures.service-moon-drill`):
    for invalid_dur in [86_400i64, 5_000_000i64] {
        let dur_err = ctx
            .dispatch_rpc(&mut session, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(structure_id), PyRep::Integer(invalid_dur)], &kw, None)
            .await
            .expect_err("Duration outside 6d..=56d must be rejected");
        assert_eq!(dur_err.code, "InvalidExtractionDuration");
    }

    let start_cancel_res = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(structure_id), PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction before cancel");
    assert_eq!(get_dict_val(&start_cancel_res, "status").unwrap().as_str(), Some("Started"));

    let cancel_res = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "CancelExtraction", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("CancelExtraction");
    assert_eq!(cancel_res, PyRep::Bool(true));
    let cancel_frac_err = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect_err("Cancelled extraction must produce no chunk and refuse fracture");
    assert_eq!(cancel_frac_err.code, "InvalidExtractionState");

    let dur_10d = 864_000i64;
    let start_res = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(structure_id), PyRep::Integer(dur_10d)], &kw, None)
        .await
        .expect("StartNewExtraction 10d");
    assert_eq!(get_dict_val(&start_res, "status").unwrap().as_str(), Some("Started"));
    let start_ts = get_dict_val(&start_res, "extractionStartTime").unwrap().as_i64().unwrap();
    let arr_ts = get_dict_val(&start_res, "chunkArrivalTime").unwrap().as_i64().unwrap();
    let chunk_vol = get_dict_val(&start_res, "chunkVolumeM3").unwrap().as_f64().unwrap();
    assert_eq!(arr_ts - start_ts, dur_10d, "Chunk must arrive at start + duration");
    assert!((chunk_vol - 7_200_000.0).abs() < 1.0, "240h extraction must yield 7,200,000 m³ chunk volume");

    let early_err = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect_err("Fracture before chunk arrival must fail");
    assert_eq!(early_err.code, "ChunkNotReadyToFracture");

    ctx.advance_time_seconds(864_000);
    let non_jp_seed = (1u64..10_000)
        .find(|&s| eve_server::rng::MechanicRng::seeded(s).try_unit_f64().unwrap() >= 0.15)
        .expect("non-jackpot seed");
    ctx.set_mechanic_rng_seed(Some(non_jp_seed));
    let frac1_res = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("FractureChunkWithStructure #1");
    let PyRep::List(ast_ids_1) = get_dict_val(&frac1_res, "asteroidIDs").unwrap() else {
        panic!("Expected asteroidIDs list");
    };
    let PyRep::List(ore_types_1) = get_dict_val(&frac1_res, "oreTypes").unwrap() else {
        panic!("Expected oreTypes list");
    };
    assert_eq!(ast_ids_1.len(), 6, "Non-jackpot fractured field must spawn 6 asteroids mixing basic and +15% improved variants");
    let has_improved_variant = ore_types_1.iter().any(|t| {
        let tid = t.as_i64().unwrap() as u32;
        ctx.sde.types.get(&tid).is_some_and(|ty| ty.name.starts_with("Brimful "))
    });
    let has_base_variant = ore_types_1.iter().any(|t| {
        let tid = t.as_i64().unwrap() as u32;
        ctx.sde.types.get(&tid).is_some_and(|ty| !ty.name.starts_with("Brimful "))
    });
    assert!(has_base_variant && has_improved_variant, "Non-jackpot field must mix basic and +15% improved quality of each ore");

    let first_ast_id = ast_ids_1[0].as_i64().unwrap();
    let first_ast_type = ore_types_1[0].as_i64().unwrap() as u32;
    assert!(jita.get_ball_snapshot(first_ast_id).await.unwrap().is_some());

    // Mine the fractured moon asteroid with our pilot so observer ledger records the pilot's mining event (`feat:mining.corp-ledger`, `feat:modules.mining-survey-chipset`)
    let miner_mod_id = 800_000_310i64;
    let chipset_id = 800_000_311i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880; // Venture
        }
        snap.items.retain(|_, item| item.location_id != ship_id);
        snap.items.insert(miner_mod_id, make_item(miner_mod_id, 483, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(chipset_id, make_item(chipset_id, 83463, char_id, ship_id, InventoryFlag::LoSlot0, 1, true));
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    let ast_pos = jita.get_ball_snapshot(first_ast_id).await.unwrap().unwrap().1.position;
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ast_pos + DVec3::new(500.0, 0.0, 0.0);
        let _ = jita.spawn_ball(sb).await;
    }
    let mine_moon_res = crate::mining_cycle_engine::mine_once(&ctx, &mut session, miner_mod_id, first_ast_id)
        .await
        .expect("Mine fractured moon asteroid");
    let mined_moon_qty = mine_moon_res.units_delivered;
    assert_eq!(mine_moon_res.ore_type_id, first_ast_type);
    assert!(mined_moon_qty > 0, "Mining fractured moon asteroid must yield positive moon ore units");

    let dup_err = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect_err("Repeated FractureChunkWithStructure must fail");
    assert_eq!(dup_err.code, "ExtractionAlreadyFractured");

    // 3. Moon field lifetime and natural decay (`feat:mining.moon-field-lifetime`):
    ctx.advance_time_seconds(172_801);
    let ext_info = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "GetExtractionAndEventIDFromStructureID", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("GetExtractionAndEventIDFromStructureID after decay");
    assert_eq!(get_dict_val(&ext_info, "status").unwrap().as_str(), Some("Decayed"));
    assert!(jita.get_ball_snapshot(first_ast_id).await.unwrap().is_none());

    // 4. Auto-fracture 3 hours after arrival (`feat:mining.moon-chunk-auto-fracture`):
    ctx.dispatch_rpc(&mut session, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(auto_structure_id), PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction on auto_structure_id");
    ctx.advance_time_seconds(518_400 + 10_800);
    let auto_info = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "GetExtractionAndEventIDFromStructureID", &[PyRep::Integer(auto_structure_id)], &kw, None)
        .await
        .expect("GetExtractionAndEventIDFromStructureID after auto-fracture window");
    assert_eq!(get_dict_val(&auto_info, "status").unwrap().as_str(), Some("Fractured"));
    assert_eq!(get_dict_val(&auto_info, "autoFractured").unwrap().as_bool(), Some(true));
    assert!(get_dict_val(&auto_info, "activeAsteroidCount").unwrap().as_i64().unwrap() >= 6);

    // Verify Jackpot (+100% yield variants: Glistening/Shining/Twinkling/Shimmering/Sparkling, `feat:mining.moon-jackpot`):
    ctx.dispatch_rpc(&mut session, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(wh_structure_id), PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction on wh_structure_id");
    ctx.advance_time_seconds(518_400);
    let jp_seed = (1u64..10_000)
        .find(|&s| eve_server::rng::MechanicRng::seeded(s).try_unit_f64().unwrap() < 0.15)
        .expect("jackpot seed");
    ctx.set_mechanic_rng_seed(Some(jp_seed));
    let jp_frac = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(wh_structure_id)], &kw, None)
        .await
        .expect("FractureChunkWithStructure jackpot");
    assert_eq!(get_dict_val(&jp_frac, "isJackpot").unwrap().as_bool(), Some(true));
    assert_eq!(get_dict_val(&jp_frac, "jackpotBonus").unwrap().as_f64(), Some(2.0));
    let PyRep::List(jp_ores) = get_dict_val(&jp_frac, "oreTypes").unwrap() else { panic!("Expected oreTypes") };
    assert!(!jp_ores.is_empty());
    for t in jp_ores {
        let tid = t.as_i64().unwrap() as u32;
        let name = &ctx.sde.types.get(&tid).unwrap().name;
        assert!(
            name.starts_with("Glistening ") || name.starts_with("Bountiful ") || name.starts_with("Shimmering ") || name.starts_with("Shining ") || name.starts_with("Resplendent ") || name.starts_with("Sparkling ") || name.starts_with("Dazzling "),
            "Jackpot fracture must only spawn +100% jackpot moon ore variants, got {name}"
        );
    }

    let corp_exts = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "GetExtractionsForCorp", &[], &kw, None)
        .await
        .expect("GetExtractionsForCorp");
    let PyRep::List(corp_ext_list) = corp_exts else { panic!("Expected List") };
    assert!(corp_ext_list.len() >= 3);

    // 5. Automated Metenox Moon Drill (`feat:mining.metenox`):
    let err_athanor = ctx
        .dispatch_rpc(&mut session, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect_err("Athanor must be rejected by autoMoonMining.RequestMiningDetails");
    assert_eq!(err_athanor.code, "InvalidStructureType");

    let details = ctx
        .dispatch_rpc(&mut session, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(metenox_id)], &kw, None)
        .await
        .expect("RequestMiningDetails on Metenox Moon Drill");
    assert_eq!(get_dict_val(&details, "efficiency").unwrap().as_f64(), Some(0.40));

    ctx.advance_time_seconds(7_200);
    let output = ctx
        .dispatch_rpc(&mut session, "autoMoonMining", None, "RequestMiningCycleOutput", &[PyRep::Integer(metenox_id), PyRep::Integer(2)], &kw, None)
        .await
        .expect("RequestMiningCycleOutput on Metenox Moon Drill");
    let produced = get_dict_val(&output, "unitsProduced").unwrap().as_i64().unwrap();
    assert_eq!(get_dict_val(&output, "fuelConsumed").unwrap().as_i64(), Some(10));
    assert_eq!(get_dict_val(&output, "magmaticGasConsumed").unwrap().as_i64(), Some(20));
    assert!(produced > 0);

    // 6. Corporate Mining Ledger (`feat:mining.corp-ledger`):
    session.corp_role = 0;
    let unpriv_err = ctx
        .dispatch_rpc(&mut session, "corpMiningLedger", None, "GetObserversWithMiningEvents", &[], &kw, None)
        .await
        .expect_err("Unprivileged pilot must be denied corpMiningLedger access");
    assert_eq!(unpriv_err.code, "CrpAccessDenied");

    session.corp_role = eve_db::corp_roles::ROLE_ACCOUNTANT;
    let corp_ledger = ctx
        .dispatch_rpc(&mut session, "corpMiningLedger", None, "GetObserversWithMiningEvents", &[], &kw, None)
        .await
        .expect("GetObserversWithMiningEvents");
    assert!(corp_ledger.is_crowset(), "GetObserversWithMiningEvents must return CRowset");
    let obs_rows = decode_crowset_rows(&corp_ledger);
    assert!(
        obs_rows.iter().any(|r| r.first().and_then(|v| v.as_i64()) == Some(structure_id)),
        "GetObserversWithMiningEvents CRowset must include structure_id {structure_id}"
    );

    let entries = ctx
        .dispatch_rpc(&mut session, "corpMiningLedger", None, "GetObserverLedger", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("GetObserverLedger for structure_id");
    assert!(entries.is_crowset(), "GetObserverLedger must return CRowset");
    let ledger_rows = decode_crowset_rows(&entries);
    let found_pilot_row = ledger_rows.iter().any(|r| {
        r.first().and_then(|v| v.as_i64()) == Some(char_id)
            && r.get(2).and_then(|v| v.as_i64()) == Some(first_ast_type as i64)
            && r.get(3).and_then(|v| v.as_i64()) == Some(mined_moon_qty)
    });
    assert!(
        found_pilot_row,
        "GetObserverLedger CRowset must contain pilot {char_id}, ore type {first_ast_type}, and quantity {mined_moon_qty}"
    );
}
