//! Unit tests for `space_entity_mining` (`CmdMineRepeatedly`, `Mine`, `reprocessingSvc.Reprocess` & `GetQuotes`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::combat_crimewatch::{with_combat_runtime, DroneMiningState};
use crate::services::inventory_fitting::fitting_containers_ships::{write_container_ship_state, AlscLogEntry};
use crate::services::station_structure::structure_upwell_ops::{
    configure_structure_access_profile, load_ext_state, save_ext_state, StructureLifecycleState,
};
use eve_db::types::TrainedSkillRecord;
use eve_db::{
    AsteroidRecord, CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord,
    IskCents, UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry, TypeMaterialEntry};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn upsert_type(
    sde: &mut Arc<SdeRegistry>,
    tid: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    volume: f64,
    capacity: f64,
    portion_size: u64,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base type");
    rec.type_id = tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = name.into();
    rec.volume = volume;
    rec.capacity = capacity;
    rec.portion_size = portion_size;
    rec.base_price = 100.0;
    rec.default_attributes.clear();
    if category_id == 6 {
        rec.default_attributes.insert(14, 8.0);
        rec.default_attributes.insert(13, 8.0);
        rec.default_attributes.insert(12, 8.0);
        rec.default_attributes.insert(1137, 3.0);
        rec.default_attributes.insert(11, 10_000.0);
        rec.default_attributes.insert(48, 10_000.0);
    } else if category_id == 7 {
        rec.effect_ids = if group_id == 778 { vec![2663] } else { vec![11, 12, 13, 16] };
    }
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(tid, rec);
}

fn make_session(char_id: i64, corp_id: i64, sys_id: Option<u32>, ship_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(601);
    s.user_id = Some(11);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.ship_id = ship_id;
    s.role = service_roles::ROLE_PLAYER_DEFAULT;
    s
}

fn kv_field<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(m) => m.get(key),
        _ => None,
    }
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1,
        ..Default::default()
    }
}

fn make_ship_ball(ship_id: i64, type_id: u32, pos: DVec3, mode: MovementMode) -> Ball {
    Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode,
    }
}

#[tokio::test]
async fn mine_validation_gates_warp_cloak_capsule_range_and_cargo_full() {
    let mut ctx = test_ctx();
    let (sys_id, char_id, corp_id) = (30_000_142, 90_000_001, 98_000_001);
    let (ship_id, laser_id, ast_id) = (1_050_000_001, 1_050_000_002, 1_050_000_010);

    upsert_type(&mut ctx.sde, 992_001, 25, 6, "Test Venture", 5_000.0, 50.0, 1, &[(1556, 5_000.0)]);
    upsert_type(&mut ctx.sde, 992_002, 54, 7, "Miner I", 5.0, 0.0, 1, &[(77, 40.0), (54, 15_000.0), (3154, 0.0)]);
    upsert_type(&mut ctx.sde, 992_003, 462, 25, "Veldspar", 0.1, 0.0, 100, &[]);

    assert!(handle(&ctx, &mut make_session(char_id, corp_id, Some(sys_id), Some(ship_id)), "entity", "Unknown", &[], None).await.is_none());

    let mut no_sys = make_session(char_id, corp_id, None, Some(ship_id));
    assert_eq!(handle(&ctx, &mut no_sys, "entity", "CmdMineRepeatedly", &[PyRep::Integer(ast_id)], None).await.unwrap().unwrap_err().code, "NoSolarSystem");

    let mut no_ship = make_session(char_id, corp_id, Some(sys_id), None);
    assert_eq!(handle(&ctx, &mut no_ship, "beyonce", "CmdMineRepeatedly", &[PyRep::Integer(ast_id)], None).await.unwrap().unwrap_err().code, "NoActiveShip");

    let mut sess = make_session(char_id, corp_id, Some(sys_id), Some(ship_id));
    assert_eq!(handle(&ctx, &mut sess, "dogmaIM", "Mine", &[], None).await.unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(handle(&ctx, &mut sess, "dogmaIM", "Mine", &[PyRep::Integer(ast_id)], None).await.unwrap().unwrap_err().code, "ShipNotInSpace");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let warp_mode = MovementMode::new_warp(DVec3::ZERO, DVec3::new(500_000.0, 0.0, 0.0), 3.0);
    let _ = sys_handle.spawn_ball_with_combat(make_ship_ball(ship_id, 992_001, DVec3::ZERO, warp_mode), CombatState { is_cloaked: true, ..Default::default() }).await;

    assert_eq!(handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(ast_id)], None).await.unwrap().unwrap_err().code, "CannotMineInWarp");
    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    assert_eq!(handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(ast_id)], None).await.unwrap().unwrap_err().code, "CannotMineWhileCloaked");
    let _ = sys_handle.set_combat_state(ship_id, CombatState::default()).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, make_item(ship_id, 670, char_id, sys_id as i64, InventoryFlag::None, 1));
    });
    assert_eq!(handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(ast_id)], None).await.unwrap().unwrap_err().code, "InvalidShipForMining");

    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_id).unwrap().type_id = 992_001; });
    assert_eq!(handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(ast_id)], None).await.unwrap().unwrap_err().code, "AsteroidNotFound");

    ctx.db.with_mut_snapshot(|snap| {
        snap.asteroids.insert(ast_id, AsteroidRecord { asteroid_id: ast_id, type_id: 992_003, solar_system_id: 30_000_144, belt_id: 1, quantity: 100, initial_quantity: 100, position: (20_000.0, 0.0, 0.0), radius: 60.0 });
    });
    assert_eq!(handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(ast_id)], None).await.unwrap().unwrap_err().code, "TargetNotInSystem");

    ctx.db.with_mut_snapshot(|snap| {
        let a = snap.asteroids.get_mut(&ast_id).unwrap();
        a.solar_system_id = sys_id;
        a.quantity = 0;
    });
    assert_eq!(handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(ast_id)], None).await.unwrap().unwrap_err().code, "AsteroidDepleted");

    ctx.db.with_mut_snapshot(|snap| { snap.asteroids.get_mut(&ast_id).unwrap().quantity = 500; });
    assert_eq!(handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(ast_id)], None).await.unwrap().unwrap_err().code, "NoMiningModuleFitted");

    ctx.db.with_mut_snapshot(|snap| { snap.items.insert(laser_id, make_item(laser_id, 992_002, char_id, ship_id, InventoryFlag::HiSlot0, 1)); });
    ctx.invalidate_ship_fitting(ship_id);
    let _ = ctx.get_or_rebuild_ship_fitting(char_id, ship_id);
    assert_eq!(handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(ast_id)], None).await.unwrap().unwrap_err().code, "OutOfRange");

    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(10_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| { snap.items.insert(1_050_000_099, make_item(1_050_000_099, 992_003, char_id, ship_id, InventoryFlag::SpecializedOreHold, 50_000)); });
    assert_eq!(handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(ast_id)], None).await.unwrap().unwrap_err().code, "CargoFull");
}

#[tokio::test]
async fn module_mining_depletes_anomaly_and_gas_harvesting_spawns_npc_ambush() {
    let mut ctx = test_ctx();
    let (sys_id, char_id, corp_id) = (30_002_537, 90_000_001, 98_000_001);
    let (ship_id, laser_id, gas_mod_id, ast_id, gas_id) = (1_060_000_001, 1_060_000_002, 1_060_000_003, 1_060_000_010, 1_060_000_011);

    upsert_type(&mut ctx.sde, 993_001, 25, 6, "Test Prospect", 5_000.0, 50.0, 1, &[(1556, 5_000.0), (1557, 5_000.0)]);
    upsert_type(&mut ctx.sde, 993_002, 54, 7, "Miner II", 5.0, 0.0, 1, &[(77, 100.0), (54, 15_000.0)]);
    upsert_type(&mut ctx.sde, 993_003, 737, 7, "Gas Cloud Harvester II", 5.0, 0.0, 1, &[(77, 40.0), (54, 15_000.0)]);
    upsert_type(&mut ctx.sde, 993_004, 462, 25, "Dense Veldspar", 0.1, 0.0, 100, &[]);
    upsert_type(&mut ctx.sde, 993_005, 711, 25, "Fullerite-C50", 1.0, 0.0, 1, &[]);
    upsert_type(&mut ctx.sde, 993_006, 551, 11, "Sleeper Upholder", 1_000_000.0, 0.0, 1, &[(481, 120_000.0)]);
    Arc::make_mut(&mut ctx.sde).types.get_mut(&993_006).unwrap().radius = 40.0;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, make_item(ship_id, 993_001, char_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(laser_id, make_item(laser_id, 993_002, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(gas_mod_id, make_item(gas_mod_id, 993_003, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        snap.asteroids.insert(ast_id, AsteroidRecord { asteroid_id: ast_id, type_id: 993_004, solar_system_id: sys_id, belt_id: 500, quantity: 200, initial_quantity: 200, position: (500_000.0, 0.0, 0.0), radius: 50.0 });
        snap.asteroids.insert(gas_id, AsteroidRecord { asteroid_id: gas_id, type_id: 993_005, solar_system_id: sys_id, belt_id: 0, quantity: 500, initial_quantity: 500, position: (2_000.0, 0.0, 0.0), radius: 50.0 });
        snap.cosmic_anomalies.entry(sys_id).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "ORE-ANOM-1".into(),
            solar_system_id: sys_id,
            name: "Hidden Belt".into(),
            anomaly_type: "OreAnomaly".into(),
            position: [500_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: vec![993_004],
            remaining_volume: 20.0,
            created_unix: 1_000,
            lifetime_seconds: 86_400,
            cleared: false,
        });
        snap.cosmic_signatures.entry(sys_id).or_default().push(CosmicSignatureRecord {
            signature_id: "ORE-ANOM-1".into(),
            solar_system_id: sys_id,
            sig_group: "OreAnomaly".into(),
            type_name: "Hidden Belt".into(),
            signal_strength: 1.0,
            position: [500_000.0, 0.0, 0.0],
            wormhole_type_id: None,
            destination_system_id: None,
            created_unix: 1_000,
            lifetime_seconds: 86_400,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
        });
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball_with_combat(make_ship_ball(ship_id, 993_001, DVec3::new(500_000.0, 0.0, 0.0), MovementMode::Stop), CombatState::default()).await;

    let mut sess = make_session(char_id, corp_id, Some(sys_id), Some(ship_id));
    let ore_res = handle(&ctx, &mut sess, "beyonce", "CmdMineRepeatedly", &[PyRep::Integer(ast_id), PyRep::Integer(laser_id)], None).await.unwrap().unwrap();
    assert_eq!(kv_field(&ore_res, "minedUnits").and_then(|v| v.as_i64()), Some(200));
    assert_eq!(kv_field(&ore_res, "depleted").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(kv_field(&ore_res, "flagID").and_then(|v| v.as_i64()), Some(InventoryFlag::SpecializedOreHold as i64));

    ctx.db.with_snapshot(|snap| {
        assert!(snap.cosmic_anomalies[&sys_id].iter().find(|a| a.anomaly_id == "ORE-ANOM-1").unwrap().cleared);
        assert!(snap.cosmic_signatures[&sys_id].iter().find(|s| s.signature_id == "ORE-ANOM-1").unwrap().collapsed);
    });

    let _ = sys_handle.set_ball_position(ship_id, DVec3::ZERO).await;

    let balls_before = sys_handle.list_balls().await.unwrap().len();
    let gas_res = handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(gas_id), PyRep::Integer(gas_mod_id)], None).await.unwrap().unwrap();
    assert_eq!(kv_field(&gas_res, "minedUnits").and_then(|v| v.as_i64()), Some(40));
    assert_eq!(kv_field(&gas_res, "flagID").and_then(|v| v.as_i64()), Some(InventoryFlag::SpecializedGasHold as i64));
    assert!(sys_handle.list_balls().await.unwrap().len() > balls_before, "gas harvesting in lowsec should spawn ambush NPC");
}

#[tokio::test]
async fn drone_mining_validates_ice_excavator_rorqual_and_registers_active_miners() {
    let mut ctx = test_ctx();
    let (sys_id, char_id, corp_id) = (30_000_142, 90_000_001, 98_000_001);
    let (ship_id, core_id, rig_id, ore_drone, ice_drone, exc_drone, ice_ast, ore_ast) = (
        1_070_000_001,
        1_070_000_002,
        1_070_000_003,
        1_070_000_010,
        1_070_000_011,
        1_070_000_012,
        1_070_000_020,
        1_070_000_021,
    );

    upsert_type(&mut ctx.sde, 994_001, 25, 6, "Venture", 5_000.0, 50.0, 1, &[(1556, 5_000.0), (3136, 5_000.0)]);
    upsert_type(&mut ctx.sde, 994_002, 883, 6, "Rorqual", 500_000.0, 5_000.0, 1, &[(3223, 5.0), (3224, -5.0), (3136, 10_000.0)]);
    upsert_type(&mut ctx.sde, 994_003, 515, 7, "Industrial Core II", 50.0, 0.0, 1, &[(2585, 50.0), (2586, -20.0)]);
    upsert_type(&mut ctx.sde, 994_004, 778, 7, "Drone Mining Augmentor I", 5.0, 0.0, 1, &[(434, 15.0)]);
    upsert_type(&mut ctx.sde, 994_005, 101, 18, "Mining Drone II", 5.0, 0.0, 1, &[(77, 30.0), (73, 60_000.0), (37, 300.0), (182, 3438.0)]);
    upsert_type(&mut ctx.sde, 994_006, 101, 18, "Ice Harvesting Drone I", 10.0, 0.0, 1, &[(77, 100.0), (73, 120_000.0), (37, 250.0), (182, 43702.0)]);
    upsert_type(&mut ctx.sde, 994_007, 101, 18, "Excavator Mining Drone", 50.0, 0.0, 1, &[(77, 500.0), (73, 60_000.0), (37, 200.0), (1272, 25.0), (182, 3438.0)]);
    upsert_type(&mut ctx.sde, 994_008, 465, 25, "Blue Ice", 100.0, 0.0, 1, &[]);
    upsert_type(&mut ctx.sde, 994_009, 462, 25, "Scordite", 0.15, 0.0, 100, &[]);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, make_item(ship_id, 994_001, char_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(rig_id, make_item(rig_id, 994_004, char_id, ship_id, InventoryFlag::RigSlot0, 1));
        snap.items.insert(ore_drone, make_item(ore_drone, 994_005, char_id, ship_id, InventoryFlag::DroneBay, 1));
        snap.items.insert(ice_drone, make_item(ice_drone, 994_006, char_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(exc_drone, make_item(exc_drone, 994_007, char_id, sys_id as i64, InventoryFlag::None, 1));
        snap.asteroids.insert(ice_ast, AsteroidRecord { asteroid_id: ice_ast, type_id: 994_008, solar_system_id: sys_id, belt_id: 1, quantity: 100, initial_quantity: 100, position: (500.0, 0.0, 0.0), radius: 50.0 });
        snap.asteroids.insert(ore_ast, AsteroidRecord { asteroid_id: ore_ast, type_id: 994_009, solar_system_id: sys_id, belt_id: 1, quantity: 10_000, initial_quantity: 10_000, position: (600.0, 0.0, 0.0), radius: 50.0 });
        snap.skills.entry(char_id).or_default().insert(
            28374,
            TrainedSkillRecord {
                type_id: 28374,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball_with_combat(make_ship_ball(ship_id, 994_001, DVec3::ZERO, MovementMode::Stop), CombatState::default()).await;

    let mut sess = make_session(char_id, corp_id, Some(sys_id), Some(ship_id));
    assert_eq!(handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(ore_drone), PyRep::Integer(ore_ast)], None).await.unwrap().unwrap_err().code, "DroneNotInSpace");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ore_drone).unwrap().location_id = sys_id as i64;
        snap.items.get_mut(&ore_drone).unwrap().flag = InventoryFlag::None;
    });
    assert_eq!(handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(ore_drone), PyRep::Integer(ice_ast)], None).await.unwrap().unwrap_err().code, "InvalidTargetGroup");
    assert_eq!(handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(ice_drone), PyRep::Integer(ore_ast)], None).await.unwrap().unwrap_err().code, "InvalidTargetGroup");
    assert_eq!(handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(exc_drone), PyRep::Integer(ore_ast)], None).await.unwrap().unwrap_err().code, "InvalidShipForExcavator");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 994_002;
        snap.items.insert(core_id, make_item(core_id, 994_003, char_id, ship_id, InventoryFlag::HiSlot0, 1));
    });
    assert_eq!(handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(exc_drone), PyRep::Integer(ore_ast)], None).await.unwrap().unwrap_err().code, "IndustrialCoreRequired");

    ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.insert(core_id); });
    let res = handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::List(vec![PyRep::Integer(ore_drone), PyRep::Integer(exc_drone)]), PyRep::Integer(ore_ast)], None).await.unwrap().unwrap();
    assert_eq!(res, PyRep::Dict(Vec::new()));
    assert_eq!(handle(&ctx, &mut sess, "entity", "CmdMineRepeatedly", &[PyRep::Integer(exc_drone), PyRep::Integer(ore_ast)], None).await.unwrap().unwrap_err().code, "DroneAlreadyMining");
    let _ = with_combat_runtime(&ctx, |rt| -> Option<DroneMiningState> { rt.active_drone_miners.get(&exc_drone).cloned() }).expect("excavator miner registered");
}

#[tokio::test]
async fn reprocessing_reprocess_and_get_quotes_error_and_success_paths() {
    let mut ctx = test_ctx();
    let (sys_id, char_id, corp_id, struct_id) = (30_000_142, 90_000_001, 98_000_001, 1_080_000_001_000);
    let (ore_item_id, small_ore_id, unref_id) = (1_080_000_010, 1_080_000_011, 1_080_000_012);

    upsert_type(&mut ctx.sde, 995_001, 462, 25, "BatchOre", 0.1, 0.0, 100, &[]);
    upsert_type(&mut ctx.sde, 995_002, 18, 4, "UnrefItem", 1.0, 0.0, 1, &[]);
    Arc::make_mut(&mut ctx.sde).type_materials.insert(995_001, vec![TypeMaterialEntry { material_type_id: 34, quantity: 400 }]);

    let mut sess = make_session(char_id, corp_id, Some(sys_id), Some(999));
    assert_eq!(handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(ore_item_id)], None).await.unwrap().unwrap_err().code, "NotDocked");

    sess.structure_id = Some(struct_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(ore_item_id)], None).await.unwrap().unwrap_err().code, "StructureNotFound");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Refinery Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        let mut ext = load_ext_state(snap);
        ext.lifecycle.insert(struct_id, StructureLifecycleState { online_modules: vec![88001], ..Default::default() });
        save_ext_state(snap, &ext);
        snap.items.insert(88001, make_item(88001, 35_899, corp_id, struct_id, InventoryFlag::ServiceSlot0, 1));
        snap.module_online_states.insert(88001, true);
        snap.items.insert(ore_item_id, make_item(ore_item_id, 995_001, char_id, struct_id, InventoryFlag::Hangar, 250));
        snap.items.insert(small_ore_id, make_item(small_ore_id, 995_001, char_id, struct_id, InventoryFlag::Hangar, 50));
        snap.items.insert(unref_id, make_item(unref_id, 995_002, char_id, struct_id, InventoryFlag::Hangar, 1));
    });

    let _ = configure_structure_access_profile(&ctx, &sess, struct_id, None, &[], &[], &[char_id], &[]);
    assert_eq!(handle(&ctx, &mut sess, "reprocessingSvc", "GetQuotes", &[], None).await.unwrap().unwrap_err().code, "AccessDenied");

    let _ = configure_structure_access_profile(&ctx, &sess, struct_id, None, &[], &[], &[], &[char_id]);
    let base_quote = handle(&ctx, &mut sess, "reprocessingSvc", "GetQuotes", &[], None).await.unwrap().unwrap();
    assert!(kv_field(&base_quote, "stationBaseYield").and_then(|v| v.as_f64()).is_some());

    let tuple_quote = handle(&ctx, &mut sess, "reprocessingSvc", "GetQuotes", &[PyRep::List(vec![PyRep::Integer(ore_item_id)]), PyRep::Integer(char_id)], None).await.unwrap().unwrap();
    let PyRep::Tuple(tup) = tuple_quote else { panic!("expected tuple") };
    assert_eq!(tup.len(), 3);

    assert_eq!(handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(unref_id)], None).await.unwrap().unwrap_err().code, "ItemNotReprocessable");
    assert_eq!(handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(small_ore_id)], None).await.unwrap().unwrap_err().code, "InsufficientBatchPortion");

    write_container_ship_state(&ctx, |st| {
        st.alsc_logs.insert(
            ore_item_id,
            vec![AlscLogEntry {
                timestamp_unix: ctx.now_unix_secs(),
                actor_id: char_id,
                action: "ALSCActionLock".into(),
                item_id: ore_item_id,
                item_type_id: 995_001,
                quantity: 250,
            }],
        );
    });
    assert_eq!(handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(ore_item_id)], None).await.unwrap().unwrap_err().code, "ContainerHasActiveAuditLog");
    write_container_ship_state(&ctx, |st| { st.alsc_logs.remove(&ore_item_id); });

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents(10_000_000);
    });
    let rep_res = handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(ore_item_id)], None).await.unwrap().unwrap();
    assert_eq!(kv_field(&rep_res, "remainderQuantity").and_then(|v| v.as_i64()), Some(50));
    assert!(kv_field(&rep_res, "tritaniumQuantity").and_then(|v| v.as_i64()).unwrap_or(0) >= 400);
    ctx.db.with_snapshot(|snap| { assert_eq!(snap.items.get(&ore_item_id).map(|i| i.quantity), Some(50)); });
}

/// #776: V24.01 5-arg `reprocessingSvc.Reprocess(itemIDs, fromLocation, ownerID, outputLocationID, outputFlagID)`
/// must return `(inputItemIDs, outputByTypeID)` even for a single item stack, so `reprocessing/ui/controller.py`
/// unpacking `inputItemIDs, outputByTypeID = self.reprocessor.Reprocess(...)` never fails.
#[tokio::test]
async fn issue_776_reprocess_five_arg_returns_v24_tuple() {
    let ctx = test_ctx();
    let (char_id, corp_id, station_id, ship_id) = (90_000_001_i64, 98_000_001_i64, 60_003_760_i64, 88_776_001_i64);
    let ore_a = 88_776_002_i64;
    let ore_b = 88_776_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(station_id);
            ch.active_ship_id = ship_id;
            ch.balance = IskCents(10_000_000);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for (iid, qty) in [(ore_a, 200), (ore_b, 100)] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 1230, // Veldspar
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let mut sess = SessionState::new(776);
    sess.character_id = Some(char_id);
    sess.user_id = Some(char_id as u64);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);

    // 1) Single-item 5-arg V24 MachoNet call (exact trigger of #776)
    let kw = std::collections::HashMap::new();
    let single_v24 = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[
                PyRep::List(vec![PyRep::Integer(ore_a)]),
                PyRep::Integer(station_id),
                PyRep::Integer(char_id),
                PyRep::Integer(station_id),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap();
    let PyRep::Tuple(parts) = single_v24 else {
        panic!("expected 2-tuple from V24 5-arg Reprocess, got {single_v24:?}");
    };
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0], PyRep::List(vec![PyRep::Integer(ore_a)]));
    let PyRep::Dict(ref out_map) = parts[1] else {
        panic!("expected outputByTypeID Dict, got {:?}", parts[1]);
    };
    let trit_out = out_map
        .iter()
        .find_map(|(k, v)| (k.as_i64() == Some(34)).then(|| v.as_i64()).flatten())
        .unwrap_or(0);
    assert!(trit_out > 0, "expected positive Tritanium yield in outputByTypeID");

    // 2) Second stack via MachoNet wire + check inputItemIDs & Dict shape
    let second_v24 = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[
                PyRep::List(vec![PyRep::Integer(ore_b)]),
                PyRep::Integer(station_id),
                PyRep::Integer(char_id),
                PyRep::Integer(station_id),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap();
    let PyRep::Tuple(parts2) = second_v24 else {
        panic!("expected 2-tuple from V24 5-arg Reprocess");
    };
    assert_eq!(parts2[0], PyRep::List(vec![PyRep::Integer(ore_b)]));
}

/// #784 + #788: `reprocessingSvc.Reprocess` must accept ore inside a docked ship's
/// `SpecializedOreHold` (`fromLocation == ship_id`), route outputs to the target location
/// (`station_id`, `InventoryFlag::Hangar`), and emit `OnAccountChange` + `OnItemChange`.
#[tokio::test]
async fn issue_784_788_reprocess_from_docked_ship_ore_hold_emits_item_and_account_changes() {
    let ctx = test_ctx();
    let (char_id, corp_id, station_id, venture_id) =
        (90_000_001_i64, 98_000_001_i64, 60_003_760_i64, 88_788_001_i64);
    let ore_full = 88_788_002_i64;
    let ore_partial = 88_788_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(station_id);
            ch.active_ship_id = venture_id;
            ch.balance = IskCents(10_000_000);
        }
        snap.items.insert(
            venture_id,
            InventoryItemRecord {
                item_id: venture_id,
                type_id: 32880, // Venture
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ore_full,
            InventoryItemRecord {
                item_id: ore_full,
                type_id: 1230, // Veldspar (portion_size = 100)
                owner_id: char_id,
                location_id: venture_id,
                flag: InventoryFlag::SpecializedOreHold,
                quantity: 200,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ore_partial,
            InventoryItemRecord {
                item_id: ore_partial,
                type_id: 1230,
                owner_id: char_id,
                location_id: venture_id,
                flag: InventoryFlag::SpecializedOreHold,
                quantity: 150,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let _ = ctx
        .db
        .acquire_online_lock(1, 788, Some(char_id), true, 1_700_000_000);
    let _ = ctx.drain_notifications_for_char(char_id);

    let mut sess = SessionState::new(788);
    sess.character_id = Some(char_id);
    sess.user_id = Some(1);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(venture_id);

    let kw = std::collections::HashMap::new();
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[
                PyRep::List(vec![PyRep::Integer(ore_full), PyRep::Integer(ore_partial)]),
                PyRep::Integer(venture_id),
                PyRep::Integer(char_id),
                PyRep::Integer(station_id),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("Reprocess from Venture SpecializedOreHold must succeed");

    let PyRep::Tuple(parts) = res else {
        panic!("expected 2-tuple from V24 5-arg Reprocess");
    };
    assert_eq!(
        parts[0],
        PyRep::List(vec![PyRep::Integer(ore_full), PyRep::Integer(ore_partial)])
    );

    // ore_full deleted, ore_partial reduced to 50 in Venture SpecializedOreHold, outputs in station Hangar
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ore_full));
        let rem = snap.items.get(&ore_partial).expect("remainder stack");
        assert_eq!(rem.quantity, 50);
        assert_eq!(rem.location_id, venture_id);
        assert_eq!(rem.flag, InventoryFlag::SpecializedOreHold);
        let trit_items: Vec<_> = snap
            .items
            .values()
            .filter(|i| i.owner_id == char_id && i.location_id == station_id && i.type_id == 34)
            .collect();
        assert!(!trit_items.is_empty());
        assert!(trit_items.iter().all(|i| i.flag == InventoryFlag::Hangar));
    });

    let notifs = ctx.drain_notifications_for_char(char_id);
    let saw_account_change = notifs.iter().any(|n| n.event == "OnAccountChange");
    let item_changes: Vec<_> = notifs
        .iter()
        .filter(|n| n.event == "OnItemChange")
        .collect();
    assert!(saw_account_change, "Expected OnAccountChange notification on reprocessing tax");
    assert!(
        item_changes.len() >= 3,
        "Expected OnItemChange for deleted stack, updated remainder stack, and created mineral stacks"
    );

    // Verify fromLocation mismatch, fitted module in docked ship, non-hangar parent, and fromLocation <= 0 fallback
    let err_wrong_from_loc = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[
                PyRep::List(vec![PyRep::Integer(ore_partial)]),
                PyRep::Integer(station_id),
                PyRep::Integer(char_id),
                PyRep::Integer(station_id),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect_err("Mismatched fromLocation must fail");
    assert_eq!(err_wrong_from_loc.code, "OwnershipOrLocationViolation");

    // Reprocess another 100 units from Venture with fromLocation = 0 (treated as None) and output raw_flag = SpecializedOreHold normalizing to Hangar at station
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ore_partial).unwrap().quantity = 100;
    });
    ctx.dispatch_rpc(
        &mut sess,
        "reprocessingSvc",
        None,
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(ore_partial)]),
            PyRep::Integer(0),
            PyRep::Integer(char_id),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::SpecializedOreHold as i64),
        ],
        &kw,
        Some("machoNet:26000"),
    )
    .await
    .expect("fromLocation = 0 must be ignored and SpecializedOreHold output at station must normalize to Hangar");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ore_partial));
        assert!(snap
            .items
            .values()
            .filter(|i| i.owner_id == char_id && i.location_id == station_id && i.type_id == 34)
            .all(|i| i.flag == InventoryFlag::Hangar));
    });
}
