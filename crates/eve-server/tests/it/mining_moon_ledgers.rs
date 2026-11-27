//! Phase 5M Survey Scanner & Compression (`MN-10, MN-11`).

#![forbid(unsafe_code)]

use eve_db::{
    AsteroidRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

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
        snap.items.insert(fleetmate_ship_id, make_item(fleetmate_ship_id, 32880, fleetmate_char_id, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(fleetmate_ore_id, make_item(fleetmate_ore_id, 1228, fleetmate_char_id, fleetmate_ship_id, InventoryFlag::SpecializedOreHold, 500, false));
        eve_testkit::legal_fixture::ensure_legal_test_pilot_in_snap(
            snap,
            &ctx.sde,
            &eve_testkit::legal_fixture::LegalTestPilotSpec {
                char_id: fleetmate_char_id,
                location: eve_testkit::legal_fixture::PilotLocationSpec::InSpace { solar_system_id: sys_id },
                ship_type_id: Some(32880),
                ..Default::default()
            },
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
        if let Some(fch) = snap.characters.get_mut(&fleetmate_char_id) {
            fch.fleet_id = Some(fleet_id);
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
    let mut fleetmate_sess = crate::common::session_for_character(&ctx, 10_991, fleetmate_char_id);
    crate::common::assert_it_legal_fixture(&ctx, "mn11_fleetmate", &fleetmate_sess, false, 0);

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
    let struct_owner_corp_id = 98_046_101i64;
    let struct_ore_id = 800_000_143i64;
    let gas_item_id = 800_000_141i64;
    let reproc_svc_mod_id = 800_000_144i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = make_structure(structure_id, struct_owner_corp_id, 35835, 30000144, "Perimeter Mining Hub", [0.0, 0.0, 0.0]);
        st.fuel_blocks = 5_000;
        snap.structures.insert(structure_id, st);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = 30000144;
            ch.station_id = None;
            ch.structure_id = Some(structure_id);
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = structure_id;
            ship.flag = InventoryFlag::Hangar;
        }
        snap.items.insert(struct_ore_id, make_item(struct_ore_id, 1228, char_id, structure_id, InventoryFlag::Hangar, 500, false));
        snap.items.insert(gas_item_id, make_item(gas_item_id, 62399, char_id, structure_id, InventoryFlag::Hangar, 200, false));
        snap.items.insert(reproc_svc_mod_id, make_item(reproc_svc_mod_id, 35899, struct_owner_corp_id, structure_id, InventoryFlag::Hangar, 1, true));
    });
    crate::common::grant_legal_active_ship_skills(&ctx, char_id);
    crate::common::sync_legal_corp_member(
        &ctx,
        &mut session,
        char_id,
        struct_owner_corp_id,
        "Perimeter Mining Corp",
        "PMC",
        true,
        eve_db::corp_roles::ROLE_DIRECTOR,
        None,
    );
    session = crate::common::session_for_character(&ctx, session.session_id, char_id);
    crate::common::assert_it_legal_fixture(&ctx, "mn11_structure_compression", &session, true, eve_db::corp_roles::ROLE_DIRECTOR);

    let offline_comp_err = ctx
        .dispatch_rpc(&mut session, "structureCompressionMgr", None, "CompressItemInStructure", &[PyRep::Integer(struct_ore_id)], &kw, None)
        .await
        .expect_err("Structure compression must fail when reprocessing service module is unfitted in hangar");
    assert_eq!(offline_comp_err.code, "ReprocessingServiceOffline");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.items.get_mut(&reproc_svc_mod_id) {
            m.flag = InventoryFlag::ServiceSlot0;
        }
        snap.module_online_states.insert(reproc_svc_mod_id, true);
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
    assert_eq!(eff_res, PyRep::Tuple(vec![PyRep::Real(0.89), PyRep::Real(0.89)]));

    let decomp_res = ctx
        .dispatch_rpc(&mut session, "structureCompressionMgr", None, "DecompressGasInStructure", &[PyRep::Integer(gas_item_id)], &kw, None)
        .await
        .expect("DecompressGasInStructure");
    assert_eq!(get_dict_val(&decomp_res, "decompressedUnits").unwrap().as_i64(), Some(178));
}

#[allow(unused_imports, dead_code)]
mod moon_survey_probes_extraction_and_drill_ledger {
    use super::*;

// Phase 5M Moon Survey Probes, Moon Extraction, Fracture, Decay, Metenox Drill & Corp Ledger (`MN-13, MN-14, MN-16`).


use super::{decode_crowset_rows, get_dict_val, make_item, make_structure, setup_test_session};
use eve_db::InventoryFlag;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

// ====================================================================
// Test 10a: MN-13: Moon Survey Probes, Launch Rules, Flight Time & Security-Band Moon Ores
// ====================================================================
#[tokio::test]
async fn test_mn13_moon_survey_probes_launch_alignment_and_resources() {
    // parity: feat:scanning.moon-survey-probe, feat:scanning.moon-survey-probe-launch-rules, feat:mining.moon-resources, feat:mining.moon-ore-types, feat:mining.moon-ore-class-by-security, rpc:moonExtractions.GetMoonResourcesForStructure
    eve_testkit::attest(&[
        "feat:scanning.moon-survey-probe",
        "feat:scanning.moon-survey-probe-launch-rules",
        "feat:mining.moon-resources",
        "feat:mining.moon-ore-types",
        "feat:mining.moon-ore-class-by-security",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();
    let station_id = 60_003_760i64;

    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let sid = snap.characters.get(&char_id).map(|c| c.active_ship_id).unwrap_or(100_000_001);
        if let Some(ship) = snap.items.get_mut(&sid) {
            ship.type_id = 32880; // Venture
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

    ctx.dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let _ = session.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);
    let jita = ctx.systems.get_or_spawn(30000142);
    let moon_40009087_pos = jita.get_ball_snapshot(40009087).await.unwrap().unwrap().1.position;
    let ship_near_moon = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;

    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ship_near_moon;
        sb.velocity = -(moon_40009087_pos - ship_near_moon).normalize() * 100.0;
        let _ = jita.spawn_ball(sb).await;
    }
    let miss_unaligned = ctx
        .dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009087)], &kw, None)
        .await
        .expect("Launch while aligned away");
    assert_eq!(get_dict_val(&miss_unaligned, "moonID"), Some(&PyRep::None));
    assert_eq!(get_dict_val(&miss_unaligned, "flightTimeSec").unwrap().as_i64(), Some(300));
    assert_eq!(get_dict_val(&miss_unaligned, "remainingInLauncher").unwrap().as_i64(), Some(1));
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&800_000_301)));

    let miss_probe_id = get_dict_val(&miss_unaligned, "probeID").unwrap().as_i64().unwrap();
    let launch_notifs = session.drain_notifications();
    assert!(launch_notifs.iter().any(|n| n.0 == "OnNewMoonProbe"));
    assert!(!launch_notifs.iter().any(|n| n.0 == "OnMoonScanComplete"));

    let recover_res = ctx
        .dispatch_rpc(&mut session, "scanMgr", None, "RecoverProbes", &[PyRep::List(vec![PyRep::Integer(miss_probe_id)])], &kw, None)
        .await
        .expect("RecoverProbes");
    assert_eq!(recover_res, PyRep::List(Vec::new()));

    ctx.advance_time_seconds(300);
    let miss_arr_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(miss_arr_notifs.iter().any(|n| n.event == "OnRemoveMoonProbe"));
    assert!(!miss_arr_notifs.iter().any(|n| n.event == "OnMoonScanComplete"));

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
    assert_eq!(get_dict_val(&miss_range, "moonID"), Some(&PyRep::None));
    assert_eq!(get_dict_val(&miss_range, "remainingInLauncher").unwrap().as_i64(), Some(1));
    ctx.advance_time_seconds(300);
    let _ = ctx.drain_notifications_for_char(char_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.items.get_mut(&800_000_301) {
            m.type_id = 33272;
        }
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.item_id != 800_000_301));
        snap.items.insert(800_000_304, make_item(800_000_304, 18637, char_id, ship_id, InventoryFlag::HiSlot0, 1, false));
        snap.items.insert(800_000_305, make_item(800_000_305, 18626, char_id, ship_id, InventoryFlag::Cargo, 1, false));
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
    assert_eq!(get_dict_val(&gaze_launch, "flightTimeSec").unwrap().as_i64(), Some(135));

    let quest_launch = ctx
        .dispatch_rpc(&mut session, "dogmaIM", None, "Activate", &[PyRep::Integer(800_000_301), PyRep::String("useMissiles".into()), PyRep::Integer(40009087)], &kw, None)
        .await
        .expect("Launch Quest Survey Probe I from T2 launcher");
    assert_eq!(get_dict_val(&quest_launch, "flightTimeSec").unwrap().as_i64(), Some(1080));

    ctx.dispatch_rpc(&mut session, "stationSvc", None, "Dock", &[PyRep::Integer(station_id)], &kw, None).await.expect("Dock before probes arrive");
    let _ = session.drain_notifications();
    ctx.advance_time_seconds(1080);
    let docked_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(docked_notifs.iter().any(|n| n.event == "OnRemoveMoonProbe"));
    assert!(!docked_notifs.iter().any(|n| n.event == "OnMoonScanComplete"));

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
    assert!(!early_notifs.iter().any(|n| n.event == "OnMoonScanComplete" || n.event == "OnRemoveMoonProbe"));
    ctx.advance_time_seconds(1);
    let scan_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(scan_notifs.iter().any(|n| n.event == "OnRemoveMoonProbe"));
    let scan_complete = scan_notifs.iter().find(|n| n.event == "OnMoonScanComplete").expect("Must emit OnMoonScanComplete");
    assert_eq!(scan_complete.args.first().and_then(|v| v.as_i64()), Some(40009087));
    let Some(PyRep::Dict(comp_entries)) = scan_complete.args.get(1) else { panic!("Expected Dict") };
    assert!(!comp_entries.is_empty());
    for (k, v) in comp_entries {
        let tid = k.as_i64().unwrap() as u32;
        assert!(v.as_f64().unwrap() > 0.0);
        assert_eq!(ctx.sde.types.get(&tid).map(|t| t.group_id), Some(1884));
    }

    let char_corp_id = 98_046_201i64;
    crate::common::sync_legal_corp_member(
        &ctx,
        &mut session,
        char_id,
        char_corp_id,
        "Moon Survey Corp",
        "MSVC",
        true,
        eve_db::corp_roles::ROLE_DIRECTOR,
        None,
    );
    crate::common::assert_it_legal_fixture(
        &ctx,
        "mn13_moon_survey_structures",
        &session,
        true,
        eve_db::corp_roles::ROLE_DIRECTOR,
    );
    let wh_structure_id = 1_000_000_001_005i64;
    let ls_structure_id = 1_000_000_001_006i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(wh_structure_id, make_structure(wh_structure_id, char_corp_id, 35835, 31000007, "WH Athanor", [1000.0, 2000.0, 3000.0]));
        snap.structures.insert(ls_structure_id, make_structure(ls_structure_id, char_corp_id, 35835, 30000012, "LS Athanor", [1000.0, 2000.0, 3000.0]));
    });
    let PyRep::List(wh_ores) = ctx.dispatch_rpc(&mut session, "moonExtractions", None, "GetMoonResourcesForStructure", &[PyRep::Integer(wh_structure_id)], &kw, None).await.unwrap() else { panic!() };
    for entry in &wh_ores {
        let tid = get_dict_val(entry, "typeID").unwrap().as_i64().unwrap() as u32;
        assert_eq!(ctx.sde.types.get(&tid).map(|t| t.group_id), Some(1884));
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
    eve_testkit::attest(&[
        "feat:mining.moon-extraction",
        "feat:mining.moon-extraction-duration-yield",
        "feat:mining.moon-fracture",
        "feat:mining.moon-chunk-auto-fracture",
        "feat:mining.moon-jackpot",
        "feat:mining.moon-field-lifetime",
        "feat:structures.service-moon-drill",
        "feat:modules.mining-survey-chipset",
        "feat:mining.metenox",
        "feat:mining.corp-ledger",
    ]);
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
    let char_corp_id = 98_000_777i64;
    crate::common::sync_legal_corp_member(
        &ctx,
        &mut session,
        char_id,
        char_corp_id,
        "Moon Test Corp",
        "MTC",
        false,
        eve_db::corp_roles::ROLE_STATION_MANAGER,
        None,
    );
    crate::common::assert_it_legal_fixture(
        &ctx,
        "mining_moon_ledgers",
        &session,
        true,
        eve_db::corp_roles::ROLE_STATION_MANAGER,
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&char_corp_id) {
            corp.members.insert(90_000_002);
        }
        for (idx, (sid, sys_id, st_type, name)) in [
            (structure_id, 30000142u32, 35835u32, "Moon Mining Athanor (Jita High-Sec)"),
            (metenox_id, 30000012u32, 81826u32, "Automated Metenox Drill (Asabona Low-Sec)"),
            (wh_structure_id, 31000007u32, 35835u32, "Wormhole Athanor (J102512)"),
            (auto_structure_id, 30000012u32, 35835u32, "Low-Sec Auto-Fracture Athanor"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut st = make_structure(sid, char_corp_id, st_type, sys_id, name, [1000.0, 2000.0, 3000.0]);
            if st_type == 35835 {
                st.fuel_blocks = 10_000;
            }
            snap.structures.insert(sid, st);
            if st_type == 35835 {
                let drill_id = 800_000_390 + idx as i64;
                snap.items.insert(drill_id, make_item(drill_id, 45009, char_corp_id, sid, InventoryFlag::ServiceSlot0, 1, true));
                snap.module_online_states.insert(drill_id, true);
            }
        }
        snap.items.insert(800_000_305, make_item(800_000_305, 81143, char_corp_id, metenox_id, InventoryFlag::StructureFuel, 200, false));
    });

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
    assert_eq!(arr_ts - start_ts, dur_10d);
    assert!((chunk_vol - 7_200_000.0).abs() < 1.0);

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
    let PyRep::List(ast_ids_1) = get_dict_val(&frac1_res, "asteroidIDs").unwrap() else { panic!() };
    let PyRep::List(ore_types_1) = get_dict_val(&frac1_res, "oreTypes").unwrap() else { panic!() };
    assert_eq!(ast_ids_1.len(), 6);
    assert!(ore_types_1.iter().any(|t| ctx.sde.types.get(&(t.as_i64().unwrap() as u32)).is_some_and(|ty| ty.name.starts_with("Brimful "))));
    assert!(ore_types_1.iter().any(|t| ctx.sde.types.get(&(t.as_i64().unwrap() as u32)).is_some_and(|ty| !ty.name.starts_with("Brimful "))));

    let first_ast_id = ast_ids_1[0].as_i64().unwrap();
    let first_ast_type = ore_types_1[0].as_i64().unwrap() as u32;
    assert!(jita.get_ball_snapshot(first_ast_id).await.unwrap().is_some());

    let miner_mod_id = 800_000_310i64;
    let chipset_id = 800_000_311i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880;
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
    let mine_moon_res = crate::mining_cycle_engine::mine_once(&ctx, &mut session, miner_mod_id, first_ast_id).await.expect("Mine #1");
    let mined_moon_qty_1 = mine_moon_res.units_delivered;
    assert_eq!(mine_moon_res.ore_type_id, first_ast_type);
    assert!(mined_moon_qty_1 > 0);
    let mine_moon_res_2 = crate::mining_cycle_engine::mine_once(&ctx, &mut session, miner_mod_id, first_ast_id).await.expect("Mine #2");
    let mined_moon_qty = mined_moon_qty_1 + mine_moon_res_2.units_delivered;

    let dup_err = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect_err("Repeated FractureChunkWithStructure must fail");
    assert_eq!(dup_err.code, "ExtractionAlreadyFractured");

    ctx.advance_time_seconds(172_801);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let PyRep::Tuple(ext_tup) = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "GetExtractionAndEventIDFromStructureID", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("GetExtractionAndEventIDFromStructureID after decay")
    else {
        panic!("Expected 2-tuple");
    };
    assert_eq!(get_dict_val(&ext_tup[0], "status").unwrap().as_str(), Some("Decayed"));
    assert!(jita.get_ball_snapshot(first_ast_id).await.unwrap().is_none());

    ctx.dispatch_rpc(&mut session, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(auto_structure_id), PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction on auto_structure_id");
    ctx.advance_time_seconds(518_400 + 12_000);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let PyRep::Tuple(auto_tup) = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "GetExtractionAndEventIDFromStructureID", &[PyRep::Integer(auto_structure_id)], &kw, None)
        .await
        .expect("GetExtractionAndEventIDFromStructureID after auto-fracture window")
    else {
        panic!("Expected 2-tuple");
    };
    assert_eq!(get_dict_val(&auto_tup[0], "status").unwrap().as_str(), Some("Fractured"));
    assert_eq!(get_dict_val(&auto_tup[0], "autoFractured").unwrap().as_bool(), Some(true));
    assert!(get_dict_val(&auto_tup[0], "activeAsteroidCount").unwrap().as_i64().unwrap() >= 6);

    let wh_start = ctx
        .dispatch_rpc(&mut session, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(wh_structure_id), PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction on wh_structure_id");
    let PyRep::List(wh_base_ores) = get_dict_val(&wh_start, "moonOres").unwrap() else { panic!() };
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
    let PyRep::List(jp_ores) = get_dict_val(&jp_frac, "oreTypes").unwrap() else { panic!() };
    assert_eq!(jp_ores.len(), wh_base_ores.len());
    for (idx, t) in jp_ores.iter().enumerate() {
        let jp_tid = t.as_i64().unwrap() as u32;
        let base_tid = get_dict_val(&wh_base_ores[idx], "typeID").unwrap().as_i64().unwrap() as u32;
        let name = &ctx.sde.types.get(&jp_tid).unwrap().name;
        assert!(name.starts_with("Glistening ") || name.starts_with("Bountiful ") || name.starts_with("Shimmering ") || name.starts_with("Shining ") || name.starts_with("Resplendent ") || name.starts_with("Sparkling ") || name.starts_with("Dazzling "));
        let base_mats = ctx.sde.type_materials.get(&base_tid).unwrap();
        let jp_mats = ctx.sde.type_materials.get(&jp_tid).unwrap();
        for bm in base_mats {
            let jm = jp_mats.iter().find(|m| m.material_type_id == bm.material_type_id).unwrap();
            assert_eq!(jm.quantity, bm.quantity * 2);
        }
    }

    let corp_exts = ctx.dispatch_rpc(&mut session, "moonExtractions", None, "GetExtractionsForCorp", &[], &kw, None).await.expect("GetExtractionsForCorp");
    let PyRep::List(corp_ext_list) = corp_exts else { panic!() };
    assert!(corp_ext_list.len() >= 3);

    // 5. Automated Metenox Moon Drill (`feat:mining.metenox`):
    let err_athanor = ctx
        .dispatch_rpc(&mut session, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect_err("Athanor must be rejected by autoMoonMining.RequestMiningDetails");
    assert_eq!(err_athanor.code, "InvalidStructureType");

    // Reset Metenox reagents & output bay before testing exact 2-hour clock progression
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&metenox_id).unwrap().fuel_blocks = 100;
        snap.items.retain(|_, i| !(i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold));
        snap.items.insert(800_000_305, make_item(800_000_305, 81143, char_corp_id, metenox_id, InventoryFlag::StructureFuel, 200, false));
        snap.auto_moon_miners.remove(&metenox_id);
    });
    let details = ctx
        .dispatch_rpc(&mut session, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(metenox_id)], &kw, None)
        .await
        .expect("RequestMiningDetails on Metenox Moon Drill");
    assert_eq!(get_dict_val(&details, "efficiency").unwrap().as_f64(), Some(0.40));
    let hourly_yield = get_dict_val(&details, "hourlyYield").unwrap().as_i64().unwrap();

    let output = ctx
        .dispatch_rpc(&mut session, "autoMoonMining", None, "RequestMiningCycleOutput", &[PyRep::Integer(metenox_id)], &kw, None)
        .await
        .expect("RequestMiningCycleOutput read-only on Metenox Moon Drill");
    let PyRep::List(rate_tuples) = output else { panic!("Expected List") };
    assert!(!rate_tuples.is_empty());

    ctx.advance_time_seconds(7_200);
    let details_2h = ctx
        .dispatch_rpc(&mut session, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(metenox_id)], &kw, None)
        .await
        .expect("RequestMiningDetails after 2h");
    assert_eq!(get_dict_val(&details_2h, "fuelRemaining").unwrap().as_i64(), Some(90));
    assert_eq!(get_dict_val(&details_2h, "magmaticGasRemaining").unwrap().as_i64(), Some(180));
    assert_eq!(get_dict_val(&details_2h, "harvestedUnitsInBay").unwrap().as_i64(), Some(hourly_yield * 2));

    // 6. Corporate Mining Ledger (`feat:mining.corp-ledger`):
    crate::common::set_legal_corp_roles(&ctx, &mut session, char_id, char_corp_id, 0);
    let unpriv_err = ctx
        .dispatch_rpc(&mut session, "corpMiningLedger", None, "GetObserversWithMiningEvents", &[], &kw, None)
        .await
        .expect_err("Unprivileged pilot must be denied corpMiningLedger access");
    assert_eq!(unpriv_err.code, "CrpAccessDenied");

    crate::common::set_legal_corp_roles(
        &ctx,
        &mut session,
        char_id,
        char_corp_id,
        eve_db::corp_roles::ROLE_ACCOUNTANT,
    );
    let corp_ledger = ctx
        .dispatch_rpc(&mut session, "corpMiningLedger", None, "GetObserversWithMiningEvents", &[], &kw, None)
        .await
        .expect("GetObserversWithMiningEvents");
    assert!(corp_ledger.is_crowset());
    let obs_rows = decode_crowset_rows(&corp_ledger);
    assert!(obs_rows.iter().any(|r| r.first().and_then(|v| v.as_i64()) == Some(structure_id)));
    let obs_struct_row = corp_ledger
        .crowset_rows()
        .unwrap()
        .iter()
        .find(|r| r.dbrow_get("itemID").and_then(PyRep::as_i64) == Some(structure_id))
        .expect("GetObserversWithMiningEvents must emit itemID row for observer structure");
    assert_eq!(obs_struct_row.dbrow_get("solarSystemID").and_then(PyRep::as_i64), Some(30_000_142));
    assert_eq!(
        obs_struct_row.dbrow_get("itemName").and_then(PyRep::as_str),
        Some("Moon Mining Athanor (Jita High-Sec)")
    );

    let entries = ctx
        .dispatch_rpc(&mut session, "corpMiningLedger", None, "GetObserverLedger", &[PyRep::Integer(structure_id)], &kw, None)
        .await
        .expect("GetObserverLedger for structure_id");
    assert!(entries.is_crowset());
    let ledger_rows = decode_crowset_rows(&entries);
    let matching_rows: Vec<&Vec<PyRep>> = ledger_rows
        .iter()
        .filter(|r| {
            r.first().and_then(|v| v.as_i64()) == Some(char_id)
                && r.get(1).and_then(|v| v.as_i64()) == Some(char_corp_id)
                && r.get(2).and_then(|v| v.as_i64()) == Some(first_ast_type as i64)
        })
        .collect();
    assert_eq!(matching_rows.len(), 1);
    assert_eq!(matching_rows[0].get(3).and_then(|v| v.as_i64()), Some(mined_moon_qty));
    let matching_dbrow = entries
        .crowset_rows()
        .unwrap()
        .iter()
        .find(|r| {
            r.dbrow_get("characterID").and_then(PyRep::as_i64) == Some(char_id)
                && r.dbrow_get("typeID").and_then(PyRep::as_i64) == Some(first_ast_type as i64)
        })
        .expect("Matching GetObserverLedger DBRow");
    assert_eq!(matching_dbrow.dbrow_get("corporationID").and_then(PyRep::as_i64), Some(char_corp_id));
    assert!(
        matching_dbrow
            .dbrow_get("eventDate")
            .and_then(PyRep::as_i64)
            .is_some_and(|ft| ft > 116_444_736_000_000_000)
    );
    assert_eq!(matching_dbrow.dbrow_get("solarsystemID").and_then(PyRep::as_i64), Some(30_000_142));
}

#[tokio::test]
async fn test_observer_ledger_scoped_to_origin_moon_refinery_only() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let corp_a = 98_046_001_i64;
    let corp_b = 98_046_002_i64;

    let astrahus_id = 1_046_000_001_i64;
    let destroyed_ref_id = 1_046_000_002_i64;
    let offline_drill_ref_id = 1_046_000_003_i64;
    let metenox_id = 1_046_000_004_i64;
    let ref_b1_id = 1_046_000_005_i64;
    let ref_b2_id = 1_046_000_006_i64;
    let ship_id = 1_046_000_010_i64;

    let belt_ast_id = 1_046_000_101_i64;
    let ice_ast_id = 1_046_000_102_i64;
    let moon_b1_ast_id = 1_046_000_103_i64;
    let moon_b2_ast_id = 1_046_000_104_i64;

    let b1_pos = [100_000.0, 0.0, 0.0];
    let b2_pos = [1_000_000_000.0, 0.0, 0.0];

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .entry(corp_a)
            .or_insert_with(|| eve_db::CorporationRecord::new(corp_a, "Observer Corp A", "OCA", 90_000_991));
        snap.items.insert(
            ship_id,
            make_item(ship_id, 17478, char_id, sys_id as i64, InventoryFlag::None, 1, true),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
        }
        eve_testkit::legal_fixture::ensure_legal_test_pilot_in_snap(
            snap,
            &ctx.sde,
            &eve_testkit::legal_fixture::LegalTestPilotSpec {
                char_id,
                corporation_id: Some(corp_b),
                corporation_name: Some("Observer Corp B"),
                corporation_ticker: Some("OCB"),
                is_ceo: true,
                corp_roles: eve_db::corp_roles::ROLE_DIRECTOR,
                location: eve_testkit::legal_fixture::PilotLocationSpec::InSpace { solar_system_id: sys_id },
                ship_type_id: Some(17478),
                ..Default::default()
            },
        );
        // Corp A Astrahus next to Moon B1 must never observe any mining
        snap.structures.insert(astrahus_id, eve_db::UpwellStructureRecord {
            structure_id: astrahus_id,
            type_id: 35832,
            owner_corp_id: corp_a,
            solar_system_id: sys_id,
            name: "Corp A Astrahus".into(),
            position: [105_000.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            ..Default::default()
        });
        // Destroyed Athanor right on top of Moon B1
        snap.structures.insert(destroyed_ref_id, eve_db::UpwellStructureRecord {
            structure_id: destroyed_ref_id,
            type_id: 35835,
            owner_corp_id: corp_a,
            solar_system_id: sys_id,
            name: "Destroyed Athanor".into(),
            position: [101_000.0, 0.0, 0.0],
            state: "Destroyed".into(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            ..Default::default()
        });
        // Athanor with an offline Moon Drill module
        snap.structures.insert(offline_drill_ref_id, eve_db::UpwellStructureRecord {
            structure_id: offline_drill_ref_id,
            type_id: 35835,
            owner_corp_id: corp_a,
            solar_system_id: sys_id,
            name: "Offline Drill Athanor".into(),
            position: [102_000.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            ..Default::default()
        });
        snap.items.insert(
            1_046_000_020,
            make_item(1_046_000_020, 35894, corp_a, offline_drill_ref_id, InventoryFlag::ServiceSlot0, 1, true),
        );
        snap.module_online_states.insert(1_046_000_020, false);

        // Automated Metenox Moon Drill must not collect manual pilot observer entries
        snap.structures.insert(metenox_id, eve_db::UpwellStructureRecord {
            structure_id: metenox_id,
            type_id: 81826,
            owner_corp_id: corp_a,
            solar_system_id: sys_id,
            name: "Corp A Metenox".into(),
            position: [103_000.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            ..Default::default()
        });

        // Two operational Corp B Moon Refineries at different moons
        snap.structures.insert(ref_b1_id, eve_db::UpwellStructureRecord {
            structure_id: ref_b1_id,
            type_id: 35835,
            owner_corp_id: corp_b,
            solar_system_id: sys_id,
            name: "Corp B Athanor Moon 1".into(),
            position: b1_pos,
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            ..Default::default()
        });
        snap.items.insert(1_046_000_021, make_item(1_046_000_021, 45009, corp_b, ref_b1_id, InventoryFlag::ServiceSlot0, 1, true));
        snap.module_online_states.insert(1_046_000_021, true);
        snap.structures.insert(ref_b2_id, eve_db::UpwellStructureRecord {
            structure_id: ref_b2_id,
            type_id: 35836,
            owner_corp_id: corp_b,
            solar_system_id: sys_id,
            name: "Corp B Tatara Moon 20".into(),
            position: b2_pos,
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            ..Default::default()
        });
        snap.items.insert(1_046_000_022, make_item(1_046_000_022, 45009, corp_b, ref_b2_id, InventoryFlag::ServiceSlot0, 1, true));
        snap.module_online_states.insert(1_046_000_022, true);

        snap.moon_extractions.insert(
            ref_b1_id,
            eve_db::MoonExtractionRecord {
                structure_id: ref_b1_id,
                moon_id: 40_009_001,
                extraction_start_unix: ctx.now_unix(),
                chunk_arrival_unix: ctx.now_unix(),
                natural_decay_unix: ctx.now_unix() + 10_800,
                status: "Fractured".into(),
                auto_fracture: false,
                event_id: 0,
                moon_ores: Vec::new(),
                spawned_asteroid_ids: vec![moon_b1_ast_id],
            },
        );
        snap.moon_extractions.insert(
            ref_b2_id,
            eve_db::MoonExtractionRecord {
                structure_id: ref_b2_id,
                moon_id: 40_009_020,
                extraction_start_unix: ctx.now_unix(),
                chunk_arrival_unix: ctx.now_unix(),
                natural_decay_unix: ctx.now_unix() + 10_800,
                status: "Fractured".into(),
                auto_fracture: false,
                event_id: 0,
                moon_ores: Vec::new(),
                spawned_asteroid_ids: vec![moon_b2_ast_id],
            },
        );

        snap.asteroids.insert(belt_ast_id, eve_db::AsteroidRecord {
            asteroid_id: belt_ast_id,
            type_id: 1230, // Veldspar
            solar_system_id: sys_id,
            belt_id: 40_000_001,
            quantity: 10_000,
            initial_quantity: 10_000,
            position: (b1_pos[0] + 5_000.0, 0.0, 0.0),
            radius: 100.0,
        });
        snap.asteroids.insert(ice_ast_id, eve_db::AsteroidRecord {
            asteroid_id: ice_ast_id,
            type_id: 16264, // Blue Ice
            solar_system_id: sys_id,
            belt_id: 40_000_002,
            quantity: 100,
            initial_quantity: 100,
            position: (b1_pos[0] + 6_000.0, 0.0, 0.0),
            radius: 100.0,
        });
        snap.asteroids.insert(moon_b1_ast_id, eve_db::AsteroidRecord {
            asteroid_id: moon_b1_ast_id,
            type_id: 45492, // Bitumens
            solar_system_id: sys_id,
            belt_id: ref_b1_id,
            quantity: 10_000,
            initial_quantity: 10_000,
            position: (b1_pos[0] + 20_000.0, 0.0, 0.0),
            radius: 120.0,
        });
        snap.asteroids.insert(moon_b2_ast_id, eve_db::AsteroidRecord {
            asteroid_id: moon_b2_ast_id,
            type_id: 45490, // Zeolites
            solar_system_id: sys_id,
            belt_id: ref_b2_id,
            quantity: 10_000,
            initial_quantity: 10_000,
            position: (b2_pos[0] + 20_000.0, 0.0, 0.0),
            radius: 120.0,
        });
    });

    // 1. Mine belt Veldspar & Ice near Corp A/B structures -> ZERO observer ledger entries anywhere
    eve_server::services::space_mining_harvest::deposit_mined_ore_for_asteroid_and_record_ledger_with_waste(
        &ctx, char_id, ship_id, sys_id, InventoryFlag::Cargo, 1230, 500, 20, Some(belt_ast_id), Some([b1_pos[0] + 5_000.0, 0.0, 0.0]),
    );
    eve_server::services::space_mining_harvest::deposit_mined_ore_for_asteroid_and_record_ledger_with_waste(
        &ctx, char_id, ship_id, sys_id, InventoryFlag::Cargo, 16264, 4, 0, Some(ice_ast_id), Some([b1_pos[0] + 6_000.0, 0.0, 0.0]),
    );
    assert!(ctx.db.get_observers_with_mining_events().is_empty(), "Belt ore and ice must never create observer entries");

    // 2. Mine moon ore spawned by ref_b1 -> ONLY ref_b1 observes it
    eve_server::services::space_mining_harvest::deposit_mined_ore_for_asteroid_and_record_ledger_with_waste(
        &ctx, char_id, ship_id, sys_id, InventoryFlag::Cargo, 45492, 250, 0, Some(moon_b1_ast_id), Some([b1_pos[0] + 20_000.0, 0.0, 0.0]),
    );
    assert!(ctx.db.get_observer_mining_ledger(astrahus_id).is_empty());
    assert!(ctx.db.get_observer_mining_ledger(destroyed_ref_id).is_empty());
    assert!(ctx.db.get_observer_mining_ledger(offline_drill_ref_id).is_empty());
    assert!(ctx.db.get_observer_mining_ledger(metenox_id).is_empty());
    assert!(ctx.db.get_observer_mining_ledger(ref_b2_id).is_empty());
    let b1_obs = ctx.db.get_observer_mining_ledger(ref_b1_id);
    assert_eq!(b1_obs.len(), 1);
    assert_eq!(b1_obs[0].type_id, 45492);
    assert_eq!(b1_obs[0].quantity, 250);

    // 3. Mine moon ore spawned by ref_b2 at Moon 20 -> ONLY ref_b2 observes it
    eve_server::services::space_mining_harvest::deposit_mined_ore_for_asteroid_and_record_ledger_with_waste(
        &ctx, char_id, ship_id, sys_id, InventoryFlag::Cargo, 45490, 400, 0, Some(moon_b2_ast_id), Some([b2_pos[0] + 20_000.0, 0.0, 0.0]),
    );
    assert_eq!(ctx.db.get_observer_mining_ledger(ref_b1_id).len(), 1);
    let b2_obs = ctx.db.get_observer_mining_ledger(ref_b2_id);
    assert_eq!(b2_obs.len(), 1);
    assert_eq!(b2_obs[0].type_id, 45490);
    assert_eq!(b2_obs[0].quantity, 400);
}

}

#[allow(unused_imports, dead_code)]
mod reprocessing_yield_tax_and_structure_rigs {
    use super::*;

// Phase 5M Reprocessing Yield, Tax, Scrapmetal, Structure & Rigs (`MN-12, C-6`).


use super::{get_dict_val, make_item, make_structure, setup_test_session};
use eve_db::{InventoryFlag, TrainedSkillRecord};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

// ====================================================================
// Test 9: MN-12, C-6: Real Reprocessing Yield, Tax, Scrapmetal, Structure & Rigs
// ====================================================================
#[tokio::test]
async fn test_mn12_c6_reprocessing_yield_tax_modules_and_structure() {
    // parity: feat:mining.reprocessing-yield, feat:mining.reprocessing-tax, feat:mining.reprocessing-modules, feat:structures.service-reprocessing, feat:mining.upwell-refinery-base-yield, feat:structures.reprocessing-rig-security-multipliers, rpc:structureCompressionMgr.CompressItemInStructure
    eve_testkit::attest(&[
        "feat:mining.reprocessing-yield",
        "feat:mining.reprocessing-tax",
        "feat:mining.reprocessing-modules",
        "feat:structures.service-reprocessing",
        "feat:mining.upwell-refinery-base-yield",
        "feat:structures.reprocessing-rig-security-multipliers",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let station_id = 60003760i64; // Jita IV - 4 (owner corp 1000035 Caldari Navy)
    assert_eq!(session.station_id, Some(station_id));
    crate::common::assert_it_legal_fixture(&ctx, "mn_reprocess_start", &session, false, 0);

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

    let expected_yield: f64 = 0.50 * 1.15 * 1.10 * 1.10; // 0.69575
    let expected_tax: f64 = 0.05 - 0.0075 * 2.0; // 0.035
    let expected_trit = (10.0_f64 * 400.0 * expected_yield).floor() as i64; // 2783
    let q_veld = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "GetQuotes", &[PyRep::List(vec![PyRep::Integer(veld_item_id)])], &kw, None)
        .await
        .expect("GetQuotes 2.0 standing");
    let PyRep::Tuple(ref qv_tup) = q_veld else { panic!("expected 3-tuple") };
    assert!((qv_tup[0].as_f64().unwrap() - expected_tax).abs() < 1e-6);

    let reproc_res = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(veld_item_id)], &kw, None)
        .await
        .expect("reprocessingSvc.Reprocess");
    assert_eq!(
        reproc_res,
        PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::Integer(veld_item_id)]),
            PyRep::Dict(vec![(PyRep::Integer(34), PyRep::Integer(expected_trit))]),
        ])
    );

    let (rem_veld_qty, has_tax_journal, minted_trit_qty) = ctx.db.with_snapshot(|snap| {
        let rem = snap.items.get(&veld_item_id).map(|it| it.quantity).unwrap_or(0);
        let has_journal = snap
            .wallet_journal
            .iter()
            .any(|e| e.character_id == char_id && e.entry_type == "ReprocessingTax" && e.amount.0 > 0);
        let minted: i64 = snap
            .items
            .values()
            .filter(|it| it.owner_id == char_id && it.location_id == station_id && it.type_id == 34)
            .map(|it| i64::from(it.quantity))
            .sum();
        (rem, has_journal, minted)
    });
    assert_eq!(rem_veld_qty, 50, "Non-multiple remainder of 50 Veldspar must remain in hangar");
    assert!(has_tax_journal, "NPC station reprocessing with tax > 0 must record a ReprocessingTax wallet journal entry");
    assert!(minted_trit_qty >= expected_trit);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1000035, 6.67);
        }
    });
    let quotes_zero_tax = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "GetQuotes", &[], &kw, None)
        .await
        .expect("GetQuotes with 6.67 standing");
    let PyRep::Tuple(ref qz_tup) = quotes_zero_tax else { panic!("expected 3-tuple") };
    assert_eq!(qz_tup[0].as_f64(), Some(0.0));

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
    let expected_imp_trit = (10.0_f64 * 400.0 * expected_imp_yield).floor() as i64;
    assert_eq!(
        imp_res,
        PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::Integer(veld_imp_id)]),
            PyRep::Dict(vec![(PyRep::Integer(34), PyRep::Integer(expected_imp_trit))]),
        ])
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&veld_imp_id).map(|i| i.quantity)), Some(50));
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.remove(&char_id);
    });

    // 2. Reprocess Modules and Ships with Scrapmetal Processing (`12196`, `feat:mining.reprocessing-modules`):
    for (idx, (scrap_lvl, expected_mod_yield)) in [(0u8, 0.50_f64), (3u8, 0.53_f64), (5u8, 0.55_f64)].into_iter().enumerate() {
        let mod_id = 800_000_151 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(char_id).or_default().insert(
                12196,
                TrainedSkillRecord { type_id: 12196, level: scrap_lvl, skill_points: 256_000.0 },
            );
            snap.active_implants.entry(char_id).or_default().push(27174);
            snap.items.insert(mod_id, make_item(mod_id, 482, char_id, station_id, InventoryFlag::Hangar, 1, false));
        });
        let mod_reproc_res = ctx
            .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(mod_id)], &kw, None)
            .await
            .expect("Reprocess module");
        let mut expected_mod_map = std::collections::BTreeMap::<i64, i64>::new();
        for mat in ctx.sde.type_materials.get(&482).expect("Miner II (482) SDE typeMaterials") {
            let expected_mat_qty = ((mat.quantity as f64) * expected_mod_yield).floor() as i64;
            if expected_mat_qty > 0 {
                *expected_mod_map.entry(i64::from(mat.material_type_id)).or_insert(0) += expected_mat_qty;
            }
        }
        let expected_mod_dict: Vec<(PyRep, PyRep)> = expected_mod_map
            .into_iter()
            .map(|(tid, qty)| (PyRep::Integer(tid), PyRep::Integer(qty)))
            .collect();
        assert_eq!(
            mod_reproc_res,
            PyRep::Tuple(vec![
                PyRep::List(vec![PyRep::Integer(mod_id)]),
                PyRep::Dict(expected_mod_dict),
            ])
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.remove(&char_id);
        snap.items.insert(800_000_155, make_item(800_000_155, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(800_000_156, make_item(800_000_156, 587, char_id, station_id, InventoryFlag::Hangar, 1, false));
    });
    let assembled_err = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(800_000_155)], &kw, None)
        .await
        .expect_err("Assembled singleton ship must require repackaging before reprocessing");
    assert_eq!(assembled_err.code, "ItemMustBeRepackaged");
    let ship_reproc_res = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(800_000_156)], &kw, None)
        .await
        .expect("Reprocess repackaged Rifter (587) at Scrapmetal Processing V");
    let mut expected_ship_map = std::collections::BTreeMap::<i64, i64>::new();
    for mat in ctx.sde.type_materials.get(&587).expect("Rifter SDE typeMaterials") {
        let expected_qty = ((mat.quantity as f64) * 0.55).floor() as i64;
        if expected_qty > 0 {
            *expected_ship_map.entry(i64::from(mat.material_type_id)).or_insert(0) += expected_qty;
        }
    }
    let expected_ship_dict: Vec<(PyRep, PyRep)> = expected_ship_map
        .into_iter()
        .map(|(tid, qty)| (PyRep::Integer(tid), PyRep::Integer(qty)))
        .collect();
    assert_eq!(
        ship_reproc_res,
        PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::Integer(800_000_156)]),
            PyRep::Dict(expected_ship_dict),
        ])
    );

    // 3. Upwell Structure Reprocessing Service (`feat:structures.service-reprocessing`, `feat:mining.upwell-refinery-base-yield`):
    let structure_id = 1_000_000_001_002i64;
    let tatara_id = 1_000_000_001_012i64;
    let citadel_id = 1_000_000_001_013i64;
    let struct_corp_id = 98_000_778i64;
    let ship_id = ctx.db.with_snapshot(|snap| snap.characters[&char_id].active_ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        for (idx, (sid, st_type, name)) in [
            (structure_id, 35835u32, "Athanor Refinery"),
            (tatara_id, 35836u32, "Tatara Refinery"),
            (citadel_id, 35825u32, "Astrahus Citadel"),
        ]
        .into_iter()
        .enumerate()
        {
            snap.structures.insert(sid, make_structure(sid, struct_corp_id, st_type, 30000142, name, [0.0, 0.0, 0.0]));
            let svc_item_id = 800_000_160 + (idx as i64) * 2;
            let ore_item_id = svc_item_id + 1;
            snap.items.insert(svc_item_id, make_item(svc_item_id, 35899, struct_corp_id, sid, InventoryFlag::ServiceSlot0, 1, true));
            snap.module_online_states.insert(svc_item_id, true);
            snap.items.insert(ore_item_id, make_item(ore_item_id, 1230, char_id, sid, InventoryFlag::Hangar, 1_000, false));
        }
        snap.items.insert(800_000_167, make_item(800_000_167, 1230, char_id, structure_id, InventoryFlag::Hangar, 500, false));
        snap.items.insert(800_000_168, make_item(800_000_168, 16262, char_id, structure_id, InventoryFlag::Hangar, 5, false));
    });

    crate::common::sync_legal_corp_member(
        &ctx,
        &mut session,
        char_id,
        struct_corp_id,
        "Upwell Test Holdings",
        "UTH",
        true,
        eve_db::corp_roles::ROLE_DIRECTOR,
        None,
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(structure_id);
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = structure_id;
            ship.flag = InventoryFlag::Hangar;
        }
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.fuel_blocks = 0;
        }
    });
    session = crate::common::session_for_character(&ctx, session.session_id, char_id);
    crate::common::assert_it_legal_fixture(&ctx, "reproc_unfueled_structure", &session, true, eve_db::corp_roles::ROLE_DIRECTOR);
    let unfueled_reproc_err = ctx
        .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(800_000_161)], &kw, None)
        .await
        .expect_err("Unfueled refinery must reject Reprocess");
    assert_eq!(unfueled_reproc_err.code, "ReprocessingServiceOffline");
    let unfueled_comp_err = ctx
        .dispatch_rpc(&mut session, "structureCompressionMgr", None, "CompressItemInStructure", &[PyRep::Integer(800_000_167)], &kw, None)
        .await
        .expect_err("Unfueled refinery must reject CompressItemInStructure");
    assert_eq!(unfueled_comp_err.code, "ReprocessingServiceOffline");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.fuel_blocks = 5_000;
        }
        snap.module_online_states.insert(800_000_160, true);
    });
    let comp_ore_res = ctx
        .dispatch_rpc(&mut session, "structureCompressionMgr", None, "CompressItemInStructure", &[PyRep::Integer(800_000_167)], &kw, None)
        .await
        .expect("CompressItemInStructure ore");
    assert_eq!(get_dict_val(&comp_ore_res, "compressedTypeID").unwrap().as_i64(), Some(62516));
    assert_eq!(get_dict_val(&comp_ore_res, "compressedUnits").unwrap().as_i64(), Some(500));
    let comp_ice_res = ctx
        .dispatch_rpc(&mut session, "structureCompressionMgr", None, "CompressItemInStructure", &[PyRep::Integer(800_000_168)], &kw, None)
        .await
        .expect("CompressItemInStructure ice");
    assert_eq!(get_dict_val(&comp_ice_res, "compressedTypeID").unwrap().as_i64(), Some(28434));
    assert_eq!(get_dict_val(&comp_ice_res, "compressedUnits").unwrap().as_i64(), Some(5));

    for (idx, (sid, expected_base, label)) in [
        (structure_id, 0.51_f64, "Athanor (35835)"),
        (tatara_id, 0.5275_f64, "Tatara (35836)"),
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
            if let Some(ship) = snap.items.get_mut(&ship_id) {
                ship.location_id = sid;
                ship.flag = InventoryFlag::Hangar;
            }
        });
        session = crate::common::session_for_character(&ctx, session.session_id, char_id);

        let struct_info = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.expect("GetQuotes");
        let PyRep::Tuple(ref si_tup) = struct_info else { panic!("expected 3-tuple") };
        assert_eq!(si_tup[0].as_f64(), Some(0.0));
        let PyRep::Dict(ref si_eff) = si_tup[1] else { panic!("expected eff dict") };
        let struct_base = si_eff.iter().find(|(k, _)| *k == PyRep::None).and_then(|(_, v)| v.as_f64()).unwrap();
        assert!((struct_base - expected_base).abs() < 1e-6, "{label} base yield mismatch");

        let ore_item_id = 800_000_161 + (idx as i64) * 2;
        let expected_struct_yield: f64 = expected_base * 1.15 * 1.10 * 1.10;
        let expected_struct_trit = (10.0_f64 * 400.0 * expected_struct_yield).floor() as i64;
        let struct_reproc_res = ctx
            .dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(ore_item_id)], &kw, None)
            .await
            .expect("Reprocess in Upwell structure");
        assert_eq!(
            struct_reproc_res,
            PyRep::Tuple(vec![
                PyRep::List(vec![PyRep::Integer(ore_item_id)]),
                PyRep::Dict(vec![(PyRep::Integer(34), PyRep::Integer(expected_struct_trit))]),
            ])
        );
    }

    // 4. Standup Reprocessing Rig Security Multipliers (`feat:structures.reprocessing-rig-security-multipliers`):
    let athanor_rig_id = 800_000_170i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(athanor_rig_id, make_item(athanor_rig_id, 46633, struct_corp_id, structure_id, InventoryFlag::RigSlot0, 1, true));
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = Some(structure_id);
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = structure_id;
            ship.flag = InventoryFlag::Hangar;
        }
    });
    session = crate::common::session_for_character(&ctx, session.session_id, char_id);

    for (idx, (sys_id, sec_mult)) in [(30000142u32, 1.0_f64), (30002537u32, 1.06_f64), (30000208u32, 1.12_f64)].into_iter().enumerate() {
        let ore_id = 800_000_171 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&structure_id) {
                st.solar_system_id = sys_id;
            }
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = sys_id;
            }
            snap.items.insert(ore_id, make_item(ore_id, 1230, char_id, structure_id, InventoryFlag::Hangar, 1_000, false));
        });
        session = crate::common::session_for_character(&ctx, session.session_id, char_id);
        let expected_rigged_base = 0.51 * sec_mult * 1.02;
        let q = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
        let PyRep::Tuple(ref q_tup) = q else { panic!("expected 3-tuple") };
        let PyRep::Dict(ref q_eff) = q_tup[1] else { panic!("expected eff dict") };
        let q_base = q_eff.iter().find(|(k, _)| *k == PyRep::None).and_then(|(_, v)| v.as_f64()).unwrap();
        assert!((q_base - expected_rigged_base).abs() < 1e-6);

        let expected_rig_trit = (10.0_f64 * 400.0 * (expected_rigged_base * 1.15 * 1.10 * 1.10)).floor() as i64;
        let r = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(ore_id)], &kw, None).await.unwrap();
        assert_eq!(
            r,
            PyRep::Tuple(vec![
                PyRep::List(vec![PyRep::Integer(ore_id)]),
                PyRep::Dict(vec![(PyRep::Integer(34), PyRep::Integer(expected_rig_trit))]),
            ])
        );
    }

    let athanor_ice_id = 800_000_175i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(athanor_ice_id, make_item(athanor_ice_id, 16262, char_id, structure_id, InventoryFlag::Hangar, 10, false));
    });
    let q_ice = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "GetQuotes", &[PyRep::List(vec![PyRep::Integer(athanor_ice_id)])], &kw, None).await.unwrap();
    let PyRep::Tuple(ref qi_tup) = q_ice else { panic!("expected 3-tuple") };
    let PyRep::Dict(ref qi_eff) = qi_tup[1] else { panic!("expected eff dict") };
    let qi_base = qi_eff.iter().find(|(k, _)| k.as_i64() == Some(16262)).and_then(|(_, v)| v.as_f64()).unwrap();
    assert!((qi_base - 0.51).abs() < 1e-6);
    let PyRep::Dict(ref qi_quotes) = qi_tup[2] else { panic!("expected quotes dict") };
    let qi_entry = qi_quotes.iter().find(|(k, _)| k.as_i64() == Some(athanor_ice_id)).map(|(_, v)| v).unwrap();
    let qi_expected_dict: Vec<(PyRep, PyRep)> = qi_entry
        .dict_get("recoverables")
        .and_then(PyRep::crowset_rows)
        .unwrap()
        .into_iter()
        .filter_map(|row| {
            let tid = row.dbrow_get("typeID")?.as_i64()?;
            let qty = row.dbrow_get("client")?.as_i64()?;
            (qty > 0).then_some((tid, qty))
        })
        .collect::<std::collections::BTreeMap<i64, i64>>()
        .into_iter()
        .map(|(tid, qty)| (PyRep::Integer(tid), PyRep::Integer(qty)))
        .collect();
    let ice_on_ore_rig = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(athanor_ice_id)], &kw, None).await.unwrap();
    assert_eq!(
        ice_on_ore_rig,
        PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::Integer(athanor_ice_id)]),
            PyRep::Dict(qi_expected_dict),
        ])
    );

    let tatara_rig_id = 800_000_180i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&tatara_id) {
            st.solar_system_id = 30000208;
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = 30000208;
            ch.structure_id = Some(tatara_id);
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = tatara_id;
            ship.flag = InventoryFlag::Hangar;
        }
        snap.items.insert(tatara_rig_id, make_item(tatara_rig_id, 46640, struct_corp_id, tatara_id, InventoryFlag::RigSlot0, 1, true));
    });
    session = crate::common::session_for_character(&ctx, session.session_id, char_id);
    let expected_tatara_t2_null_base = 0.53 * 1.12 * 1.055; // 0.626248
    for (idx, (type_id, qty)) in [(1230u32, 1_000), (16262u32, 10), (45490u32, 1_000)].into_iter().enumerate() {
        let item_id = 800_000_181 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(item_id, make_item(item_id, type_id, char_id, tatara_id, InventoryFlag::Hangar, qty, false));
        });
        let qt = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "GetQuotes", &[PyRep::List(vec![PyRep::Integer(item_id)])], &kw, None).await.unwrap();
        let PyRep::Tuple(ref qt_tup) = qt else { panic!("expected 3-tuple") };
        let PyRep::Dict(ref qt_eff) = qt_tup[1] else { panic!("expected eff dict") };
        let qt_base = qt_eff.iter().find(|(k, _)| k.as_i64() == Some(i64::from(type_id))).and_then(|(_, v)| v.as_f64()).unwrap();
        assert!((qt_base - expected_tatara_t2_null_base).abs() < 1e-6);
        let PyRep::Dict(ref qt_quotes) = qt_tup[2] else { panic!("expected quotes dict") };
        let qt_entry = qt_quotes.iter().find(|(k, _)| k.as_i64() == Some(item_id)).map(|(_, v)| v).unwrap();
        let qt_expected_dict: Vec<(PyRep, PyRep)> = qt_entry
            .dict_get("recoverables")
            .and_then(PyRep::crowset_rows)
            .unwrap()
            .into_iter()
            .filter_map(|row| {
                let tid = row.dbrow_get("typeID")?.as_i64()?;
                let q = row.dbrow_get("client")?.as_i64()?;
                (q > 0).then_some((tid, q))
            })
            .collect::<std::collections::BTreeMap<i64, i64>>()
            .into_iter()
            .map(|(tid, q)| (PyRep::Integer(tid), PyRep::Integer(q)))
            .collect();
        let res = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(item_id)], &kw, None).await.unwrap();
        assert_eq!(
            res,
            PyRep::Tuple(vec![
                PyRep::List(vec![PyRep::Integer(item_id)]),
                PyRep::Dict(qt_expected_dict),
            ])
        );
    }

    // 5. Structure Owner Reprocessing Tax (`feat:mining.reprocessing-tax`):
    let owner_corp_id = 98_000_777i64;
    let taxed_ore_id = 800_000_189i64;
    let journal_before = ctx.db.with_snapshot(|snap| snap.wallet_journal.len());
    crate::common::sync_legal_corp_member(
        &ctx,
        &mut session,
        char_id,
        owner_corp_id,
        "Refinery Owner Corp",
        "RFOWN",
        true,
        eve_db::corp_roles::ROLE_DIRECTOR,
        None,
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&owner_corp_id) {
            corp.tax_rate = 0.15;
        }
        if let Some(st) = snap.structures.get_mut(&tatara_id) {
            st.owner_corp_id = owner_corp_id;
        }
        snap.items.insert(taxed_ore_id, make_item(taxed_ore_id, 1230, char_id, tatara_id, InventoryFlag::Hangar, 1_000, false));
    });
    ctx.sync_session(&mut session);
    crate::common::assert_it_legal_fixture(
        &ctx,
        "reproc_owner_tax",
        &session,
        true,
        eve_db::corp_roles::ROLE_DIRECTOR,
    );
    ctx.dispatch_rpc(&mut session, "structureSettings", None, "SaveProfileSettings", &[PyRep::Integer(tatara_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Real(0.03)])])], &kw, None).await.expect("SaveProfileSettings 3% reprocessing tax");
    let q_st_tax = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "GetQuotes", &[PyRep::List(vec![PyRep::Integer(taxed_ore_id)])], &kw, None).await.expect("GetQuotes with 3% owner tax");
    let PyRep::Tuple(ref qst_tup) = q_st_tax else { panic!("expected 3-tuple") };
    assert!((qst_tup[0].as_f64().unwrap() - 0.03).abs() < 1e-6);
    let bal_before_st_tax = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    let st_tax_res = ctx.dispatch_rpc(&mut session, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(taxed_ore_id)], &kw, None).await.expect("Reprocess in structure with 3% owner tax");
    let PyRep::Tuple(ref st_tup) = st_tax_res else { panic!("expected 2-tuple") };
    assert_eq!(st_tup[0], PyRep::List(vec![PyRep::Integer(taxed_ore_id)]));
    assert!(matches!(&st_tup[1], PyRep::Dict(d) if d.iter().any(|(k, v)| k.as_i64() == Some(34) && v.as_i64().unwrap_or(0) > 0)));
    let (journal_added, corp_wallet, bal_after_st_tax) = ctx.db.with_snapshot(|snap| {
        let added = snap.wallet_journal
            .iter()
            .skip(journal_before)
            .any(|e| e.character_id == char_id && e.entry_type == "ReprocessingTax" && e.amount.0 > 0);
        let cw = snap.corporations.get(&owner_corp_id).map(|c| c.master_wallet_balance().0).unwrap_or(0);
        (added, cw, snap.characters[&char_id].balance.0)
    });
    assert!(journal_added && corp_wallet > 0 && bal_after_st_tax < bal_before_st_tax);
}

}
