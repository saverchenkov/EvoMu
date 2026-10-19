//! Comprehensive Phase 5M Mining, Harvesting, Processing & Moons test suite (`MN-01..MN-14, MN-16, C-5, C-6`).
//!
// parity: feat:mining.mining-crits, feat:ships.special-holds, feat:mining.ore-hold, feat:mining.mining-to-jetcan, feat:mining.modulated-crystals, feat:mining.mining-skills, feat:mining.mining-upgrades, feat:mining.mining-fleet-boosts, feat:mining.ore-ships, feat:mining.mining-destroyers, feat:drones.mining-drones, feat:mining.mining-drones-yield, feat:mining.excavator-drones, feat:modules.survey-scanner, feat:mining.survey-scan, feat:mining.compression-space, feat:mining.compression-structure, feat:mining.gas-decompression, feat:mining.reprocessing-yield, feat:mining.reprocessing-tax, feat:mining.reprocessing-modules, feat:structures.service-reprocessing, feat:scanning.moon-survey-probe, feat:mining.moon-extraction, feat:mining.moon-fracture, feat:mining.moon-field-lifetime, feat:mining.moon-jackpot, feat:mining.metenox, feat:mining.corp-ledger

#![forbid(unsafe_code)]

use eve_db::{
    AsteroidRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;
use std::collections::HashMap;

async fn setup_test_session(ctx: &ServerContext, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(1001);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
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

    // Initialize character with high skills
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        for (skill_id, level) in [
            (3386, 5),  // Mining V
            (3410, 5),  // Astrogeology V
            (17940, 5), // Mining Barge V
            (22551, 5), // Exhumers V
            (16281, 5), // Ice Harvesting V
            (25544, 5), // Gas Cloud Harvesting V
            (3439, 5),  // Mining Drone Operation V
            (3436, 5),  // Drone Interfacing V
            (3385, 5),  // Reprocessing V
            (3389, 5),  // Reprocessing Efficiency V
            (12196, 5), // Scrapmetal Processing V
            (18025, 5), // Veldspar Processing V
            (18024, 5), // Scordite Processing V
            (60377, 5), // Simple Ore Processing V
        ] {
            skills.insert(
                skill_id,
                TrainedSkillRecord {
                    type_id: skill_id,
                    level,
                    skill_points: 500_000.0,
                },
            );
        }
    });

    sess
}

fn get_dict_val<'a>(dict: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match dict {
        PyRep::KeyVal(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == key)
            .map(|(_, v)| v),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| match k {
                PyRep::String(s) => s == key,
                _ => false,
            })
            .map(|(_, v)| v),
        _ => None,
    }
}

// ====================================================================
// Test 1: MN-01, MN-02, C-5: Static Data, Belt Ore Distribution, Ice Belts
// ====================================================================
#[tokio::test]
async fn test_mn01_mn02_c5_static_data_and_belt_ore_distribution() {
    let ctx = ServerContext::new();

    // 1. Static data verification from SDE:
    // Veldspar (1230): Portion size 100, yields only Tritanium (34)
    let veldspar_mats = ctx.sde.type_materials.get(&1230).expect("Veldspar typeMaterials");
    assert_eq!(veldspar_mats.len(), 1, "Veldspar must yield Tritanium only");
    assert_eq!(veldspar_mats[0].material_type_id, 34);
    assert_eq!(veldspar_mats[0].quantity, 400);

    // Scordite (1228): Portion size 100, yields Tritanium (34) + Pyerite (35)
    let scordite_mats = ctx.sde.type_materials.get(&1228).expect("Scordite typeMaterials");
    assert_eq!(scordite_mats.len(), 2, "Scordite must yield Tritanium and Pyerite");
    let has_trit = scordite_mats.iter().any(|m| m.material_type_id == 34);
    let has_pye = scordite_mats.iter().any(|m| m.material_type_id == 35);
    assert!(has_trit && has_pye);

    // Ore variant comparison: Concentrated Veldspar (17470) yields +5% Tritanium (420 vs 400)
    let conc_veldspar_mats = ctx.sde.type_materials.get(&17470).expect("Concentrated Veldspar mats");
    assert_eq!(conc_veldspar_mats[0].quantity, 420);
    assert!(conc_veldspar_mats[0].quantity > veldspar_mats[0].quantity);

    // Clear Icicle (16262): Ice yields Heavy Water, Liquid Ozone, Strontium Clathrates, and Helium Isotopes
    let ice_mats = ctx.sde.type_materials.get(&16262).expect("Clear Icicle mats");
    let ice_types: Vec<u32> = ice_mats.iter().map(|m| m.material_type_id).collect();
    assert!(ice_types.contains(&16272), "Must contain Heavy Water");
    assert!(ice_types.contains(&16273), "Must contain Liquid Ozone");
    assert!(ice_types.contains(&16275), "Must contain Strontium Clathrates");
    assert!(ice_types.contains(&16274), "Must contain Helium Isotopes");

    // Compressible types:
    let comp_veld = ctx.sde.compressible_types.get(&1230);
    assert!(comp_veld.is_some(), "Veldspar must be a compressible type in SDE");

    // 2. Belt ore distribution verification by system security:
    let mut highsec_bp = eve_destiny::Ballpark::new(30000144);
    let highsec_states = eve_server::system_actor::populate_asteroid_belt_entities(
        &mut highsec_bp,
        30000144, // Perimeter: highsec 0.95
        &ctx.sde,
    );
    assert!(!highsec_states.is_empty(), "Highsec belts must generate asteroids");
    let highsec_type_ids: Vec<u32> = highsec_states.iter().map(|s| s.type_id).collect();
    assert!(
        highsec_type_ids.contains(&1230) && highsec_type_ids.contains(&1228),
        "Highsec belt must contain Veldspar and Scordite"
    );

    // Lowsec / Nullsec belt distribution:
    let mut nullsec_bp = eve_destiny::Ballpark::new(30000208); // LZ-6SU (nullsec -0.17)
    let nullsec_states = eve_server::system_actor::populate_asteroid_belt_entities(
        &mut nullsec_bp,
        30000208,
        &ctx.sde,
    );
    assert!(!nullsec_states.is_empty(), "Nullsec belts must generate asteroids");
    let nullsec_type_ids: Vec<u32> = nullsec_states.iter().map(|s| s.type_id).collect();
    assert!(
        nullsec_type_ids.contains(&21) || nullsec_type_ids.contains(&1229) || nullsec_type_ids.contains(&1232),
        "Nullsec belt must contain high-tier nullsec ores (Hedbergite/Bistot/Dark Ochre)"
    );
}

// ====================================================================
// Test 2: MN-04, MN-06: Mining Cycles, Crystals, Crits, Residue
// ====================================================================
#[tokio::test]
// parity: feat:mining.mining-crits, feat:mining.modulated-crystals
async fn test_mn04_mn06_crits_crystals_residue() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let undock = ctx
        .dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // Spawn Veldspar Asteroid
    let ast_id = 750_000_101i64;
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: ast_id,
        solar_system_id: 30000142,
        belt_id: 40000001,
        type_id: 1230,
        quantity: 10_000,
        initial_quantity: 10_000,
        position: (ship_ball.position.x + 500.0, ship_ball.position.y, ship_ball.position.z),
        radius: 60.0,
    });
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: ast_id,
            belt_id: 40000001,
            type_id: 1230,
            initial_units: 10_000,
            remaining_units: 10_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: ast_id,
            type_id: 1230,
            position: ship_ball.position + DVec3::new(500.0, 0.0, 0.0),
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

    // Fit Modulated Strip Miner II (type 17912) and Veldspar Mining Crystal I (type 18070)
    // Strip miners require a Mining Barge (e.g. Retriever 17478) or Exhumer
    let laser_id = 800_000_101i64;
    let crystal_id = 800_000_102i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 17478; // Retriever
        }
        snap.items.retain(|_, item| item.location_id != ship_id || matches!(item.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedOreHold));
        snap.items.insert(
            laser_id,
            InventoryItemRecord {
                item_id: laser_id,
                type_id: 17912, // Modulated Strip Miner II
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            crystal_id,
            InventoryItemRecord {
                item_id: crystal_id,
                type_id: 18066, // Veldspar Mining Crystal I (group 482)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(eve_sde::attr_ids::HP_STRUCT, 1.0)]),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // Run mining cycle with dogmaIM.Activate
    let mut act_kw = HashMap::new();
    act_kw.insert("targetID".into(), PyRep::Integer(ast_id));
    let act_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into())],
            &act_kw,
            None,
        )
        .await
        .expect("Activate mining laser");

    let yield_units = get_dict_val(&act_res, "yieldUnits").unwrap().as_i64().unwrap();
    let crystal_dmg = get_dict_val(&act_res, "crystalDamage").unwrap().as_f64().unwrap();
    let residue_units = get_dict_val(&act_res, "residueUnits").unwrap().as_i64().unwrap();
    assert!(yield_units > 0, "Laser cycle must produce ore yield");
    assert!(crystal_dmg > 0.0, "Mining crystal must take structural damage per cycle");
    assert!(residue_units > 0, "Modulated strip miner with crystal has waste probability > 0");

    // Mining crits test: force a critical hit on mining laser cycle
    let mut crit_kw = HashMap::new();
    crit_kw.insert("targetID".into(), PyRep::Integer(ast_id));
    crit_kw.insert("forceCrit".into(), PyRep::Bool(true));
    let crit_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into())],
            &crit_kw,
            None,
        )
        .await
        .expect("Activate with forceCrit");

    let is_crit = get_dict_val(&crit_res, "isCrit").unwrap().as_bool().unwrap();
    let crit_mult = get_dict_val(&crit_res, "critYieldMultiplier").unwrap().as_f64().unwrap();
    assert!(is_crit, "Mining crit must be flagged true");
    assert_eq!(crit_mult, 2.0, "Mining crit bonus yield must be 2.0x");
}

// ====================================================================
// Test 3: MN-05: Specialized Holds and Mining to Jetcan
// ====================================================================
#[tokio::test]
// parity: feat:ships.special-holds, feat:mining.ore-hold, feat:mining.mining-to-jetcan
async fn test_mn05_special_holds_and_mining_to_jetcan() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let undock = ctx
        .dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let beyonce_id = get_dict_val(&undock, "beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let jita = ctx.systems.get_or_spawn(30000142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // Ship is Venture (type 32880) with SpecializedOreHold (flag 134, 5000 m3)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880;
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let ast_id = 750_000_105i64;
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: ast_id,
            belt_id: 40000001,
            type_id: 1230,
            initial_units: 50_000,
            remaining_units: 50_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: ast_id,
            type_id: 1230,
            position: ship_ball.position + DVec3::new(600.0, 0.0, 0.0),
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

    // 1. Normal mining cycle deposits into Venture's SpecializedOreHold (133)
    let mine_res = ctx
        .dispatch_rpc(
            &mut session,
            "beyonce",
            Some(beyonce_id),
            "CmdMine",
            &[PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdMine on Venture");
    let flag_id = get_dict_val(&mine_res, "flagID").unwrap().as_i64().unwrap();
    assert_eq!(
        flag_id,
        InventoryFlag::SpecializedOreHold as i64,
        "Venture must deposit mined ore directly into SpecializedOreHold"
    );

    // 2. Mining to jetcan:
    let mut jetcan_kw = HashMap::new();
    jetcan_kw.insert("mineToJetcan".into(), PyRep::Bool(true));
    let jetcan_res = ctx
        .dispatch_rpc(
            &mut session,
            "beyonce",
            Some(beyonce_id),
            "CmdMine",
            &[PyRep::Integer(ast_id)],
            &jetcan_kw,
            None,
        )
        .await
        .expect("CmdMine to jetcan");

    let to_jetcan = get_dict_val(&jetcan_res, "toJetcan").unwrap().as_bool().unwrap();
    let jetcan_id = get_dict_val(&jetcan_res, "jetcanID").unwrap().as_i64().unwrap();
    assert!(to_jetcan, "toJetcan must be true");
    assert!(jetcan_id > 0, "A valid jetcan container ID must be generated");

    // Check jetcan exists in database
    let can_exists = ctx.db.with_snapshot(|snap| {
        snap.items.get(&jetcan_id).map(|i| i.type_id == 23).unwrap_or(false)
    });
    assert!(can_exists, "Jetcan cargo container (type 23) must exist in DB");
}

// ====================================================================
// Test 4: MN-07: Skills, MLU Upgrades, Fleet Boosts, ORE Ships & Destroyers
// ====================================================================
#[tokio::test]
// parity: feat:mining.mining-skills, feat:mining.mining-upgrades, feat:mining.mining-fleet-boosts, feat:mining.ore-ships, feat:mining.mining-destroyers
async fn test_mn07_skills_upgrades_boosts_ore_ships_and_destroyers() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let undock = ctx
        .dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    let ast_id = 750_000_107i64;
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: ast_id,
        solar_system_id: 30000142,
        belt_id: 40000001,
        type_id: 1230,
        quantity: 20_000,
        initial_quantity: 20_000,
        position: (ship_ball.position.x + 800.0, ship_ball.position.y, ship_ball.position.z),
        radius: 80.0,
    });

    let laser_id = 800_000_107i64;
    let mlu_id = 800_000_108i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, item| item.location_id != ship_id || matches!(item.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedOreHold));
        // Fit Miner II
        snap.items.insert(
            laser_id,
            InventoryItemRecord {
                item_id: laser_id,
                type_id: 482, // Miner II
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Fit Mining Laser Upgrade II (type 22291, group 546)
        snap.items.insert(
            mlu_id,
            InventoryItemRecord {
                item_id: mlu_id,
                type_id: 22291,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // 1. Run cycle with MLU and Mining V + Astrogeology V
    let mut act_kw = HashMap::new();
    act_kw.insert("targetID".into(), PyRep::Integer(ast_id));
    let act_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into())],
            &act_kw,
            None,
        )
        .await
        .expect("Activate with MLU");
    let yield_vol = get_dict_val(&act_res, "yieldVolume").unwrap().as_f64().unwrap();
    assert!(yield_vol > 20.0, "Miner II yield with Mining V + Astro V + MLU II must be boosted");

    // 2. Mining fleet boosts: Mining Foreman Bursts reduce cycle duration by 15%
    let mut fleet_kw = HashMap::new();
    fleet_kw.insert("targetID".into(), PyRep::Integer(ast_id));
    fleet_kw.insert("fleetBoost".into(), PyRep::Bool(true));
    let fleet_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into())],
            &fleet_kw,
            None,
        )
        .await
        .expect("Activate with fleetBoost");
    let cycle_time = get_dict_val(&fleet_res, "cycleTimeMs").unwrap().as_i64().unwrap();
    let fleet_active = get_dict_val(&fleet_res, "fleetBoostActive").unwrap().as_bool().unwrap();
    assert!(fleet_active, "fleetBoostActive must be true");
    assert!(cycle_time < 180_000, "Fleet boost must reduce laser cycle duration");

    // 3. Mining Destroyers (Pioneer 89240 and Perseverance 91174):
    // Switch ship to Pioneer
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 89240; // Pioneer
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    let pioneer_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into())],
            &act_kw,
            None,
        )
        .await
        .expect("Activate on Pioneer");
    let pioneer_yield = get_dict_val(&pioneer_res, "yieldVolume").unwrap().as_f64().unwrap();
    assert!(pioneer_yield > 0.0, "Pioneer mining destroyer must receive hull mining bonus");

    // Switch ship to Hulk (22544, Exhumer) and fit Strip Miner I (type 17482, non-turret)
    let strip_id = 800_000_109i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 22544; // Hulk
        }
        snap.items.retain(|_, item| item.location_id != ship_id || matches!(item.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedOreHold));
        snap.items.insert(
            strip_id,
            InventoryItemRecord {
                item_id: strip_id,
                type_id: 17482, // Strip Miner I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    let hulk_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(strip_id), PyRep::String("miningLaser".into())],
            &act_kw,
            None,
        )
        .await
        .expect("Activate on Hulk");
    let hulk_yield = get_dict_val(&hulk_res, "yieldVolume").unwrap().as_f64().unwrap();
    assert!(hulk_yield > 0.0, "Hulk exhumer must receive Exhumers skill yield bonus");
}

// ====================================================================
// Test 5: MN-08: Ice Harvesting & Gas Cloud Harvesting
// ====================================================================
#[tokio::test]
async fn test_mn08_ice_and_gas_harvesting() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let undock = ctx
        .dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // 1. Ice Harvesting: Clear Icicle (16262, unit volume 1000 m3)
    let ice_ast_id = 750_000_108i64;
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: ice_ast_id,
        solar_system_id: 30000142,
        belt_id: 40000001,
        type_id: 16262,
        quantity: 50,
        initial_quantity: 50,
        position: (ship_ball.position.x + 1000.0, ship_ball.position.y, ship_ball.position.z),
        radius: 100.0,
    });
    let ice_harvester_id = 800_000_110i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 17478; // Retriever (Mining Barge)
        }
        snap.items.retain(|_, item| item.location_id != ship_id || matches!(item.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedOreHold));
        snap.items.insert(
            ice_harvester_id,
            InventoryItemRecord {
                item_id: ice_harvester_id,
                type_id: 16278, // Ice Harvester I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let mut ice_kw = HashMap::new();
    ice_kw.insert("targetID".into(), PyRep::Integer(ice_ast_id));
    let ice_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(ice_harvester_id), PyRep::String("miningLaser".into())],
            &ice_kw,
            None,
        )
        .await
        .expect("Activate Ice Harvester");
    let ice_units = get_dict_val(&ice_res, "yieldUnits").unwrap().as_i64().unwrap();
    assert_eq!(ice_units, 1, "Ice harvester cycle must harvest exactly 1 ice block (1000 m3)");

    // 2. Gas Harvesting: Fullerite-C50 (30370, unit volume 1 m3)
    let gas_ast_id = 750_000_109i64;
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: gas_ast_id,
        solar_system_id: 30000142,
        belt_id: 40000001,
        type_id: 30370,
        quantity: 500,
        initial_quantity: 500,
        position: (ship_ball.position.x + 1200.0, ship_ball.position.y, ship_ball.position.z),
        radius: 120.0,
    });
    let gas_harvester_id = 800_000_111i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880; // Venture (has turret hardpoints and gas bonus)
        }
        snap.items.retain(|_, item| item.location_id != ship_id || matches!(item.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedOreHold));
        snap.items.insert(
            gas_harvester_id,
            InventoryItemRecord {
                item_id: gas_harvester_id,
                type_id: 25542, // Gas Cloud Harvester I (group 737)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let mut gas_kw = HashMap::new();
    gas_kw.insert("targetID".into(), PyRep::Integer(gas_ast_id));
    let gas_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(gas_harvester_id), PyRep::String("miningLaser".into())],
            &gas_kw,
            None,
        )
        .await
        .expect("Activate Gas Harvester");
    let gas_units = get_dict_val(&gas_res, "yieldUnits").unwrap().as_i64().unwrap();
    assert!(gas_units >= 10, "Gas cloud harvester must extract gas units from cloud");
}

// ====================================================================
// Test 6: MN-09: Mining Drones & Excavators Yield
// ====================================================================
#[tokio::test]
// parity: feat:drones.mining-drones, feat:mining.mining-drones-yield, feat:mining.excavator-drones
async fn test_mn09_mining_drones_and_excavators() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let undock = ctx
        .dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    let ast_id = 750_000_112i64;
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: ast_id,
            belt_id: 40000001,
            type_id: 1230,
            initial_units: 30_000,
            remaining_units: 30_000,
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

    // 1. Mining Drone II (type 1201, group 101)
    let drone_id = 800_000_120i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: 10250, // Mining Drone II
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut drone_kw = HashMap::new();
    drone_kw.insert("droneID".into(), PyRep::Integer(drone_id));
    let drone_res = ctx
        .dispatch_rpc(
            &mut session,
            "droneMgr",
            None,
            "Mine",
            &[PyRep::Integer(ast_id)],
            &drone_kw,
            None,
        )
        .await
        .expect("droneMgr.Mine");
    let mined_units = get_dict_val(&drone_res, "minedUnits").unwrap().as_i64().unwrap();
    let is_drone = get_dict_val(&drone_res, "isDrone").unwrap().as_bool().unwrap();
    assert!(is_drone, "Must be flagged as drone mining");
    assert!(mined_units >= 300, "Mining Drone II with V/V skills must yield >= 300 units");

    // 2. 'Excavator' Mining Drone (type 41030)
    let exc_drone_id = 800_000_121i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            exc_drone_id,
            InventoryItemRecord {
                item_id: exc_drone_id,
                type_id: 41030, // 'Excavator' Mining Drone
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut exc_kw = HashMap::new();
    exc_kw.insert("droneID".into(), PyRep::Integer(exc_drone_id));
    let exc_res = ctx
        .dispatch_rpc(
            &mut session,
            "droneMgr",
            None,
            "MineAsteroid",
            &[PyRep::Integer(ast_id)],
            &exc_kw,
            None,
        )
        .await
        .expect("droneMgr.MineAsteroid with Excavator");
    let exc_mined = get_dict_val(&exc_res, "minedUnits").unwrap().as_i64().unwrap();
    assert!(exc_mined > mined_units, "Excavator drone must produce vastly higher yield than T2 mining drone");
}

// ====================================================================
// Test 7: MN-10: Survey Scanner Results
// ====================================================================
#[tokio::test]
// parity: feat:modules.survey-scanner, feat:mining.survey-scan
async fn test_mn10_survey_scanner() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let undock = ctx
        .dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    let ast1_id = 750_000_130i64;
    let ast2_id = 750_000_131i64;
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: ast1_id,
        solar_system_id: 30000142,
        belt_id: 40000001,
        type_id: 1230, // Veldspar
        quantity: 15_000,
        initial_quantity: 15_000,
        position: (ship_ball.position.x + 300.0, ship_ball.position.y, ship_ball.position.z),
        radius: 50.0,
    });
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: ast2_id,
        solar_system_id: 30000142,
        belt_id: 40000001,
        type_id: 1228, // Scordite
        quantity: 8_000,
        initial_quantity: 8_000,
        position: (ship_ball.position.x + 500.0, ship_ball.position.y, ship_ball.position.z),
        radius: 60.0,
    });

    // Fit Mining Survey Chipset I (type 444, group 49)
    let scanner_id = 800_000_130i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, item| item.location_id != ship_id || matches!(item.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedOreHold));
        snap.items.insert(
            scanner_id,
            InventoryItemRecord {
                item_id: scanner_id,
                type_id: 444, // Mining Survey Chipset I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let scan_res = ctx
        .dispatch_rpc(
            &mut session,
            "miningScanMgr",
            None,
            "perform_scan",
            &[PyRep::Integer(scanner_id)],
            &kw,
            None,
        )
        .await
        .expect("miningScanMgr.perform_scan");

    let count = get_dict_val(&scan_res, "asteroidCount").unwrap().as_i64().unwrap();
    assert!(count >= 2, "Survey scanner must detect nearby asteroids in range");
    let PyRep::List(asteroids) = get_dict_val(&scan_res, "asteroids").unwrap() else {
        panic!("Expected asteroids list");
    };
    let found_veld = asteroids.iter().any(|a| {
        get_dict_val(a, "asteroidID").and_then(|v| v.as_i64()) == Some(ast1_id)
            && get_dict_val(a, "quantity").and_then(|v| v.as_i64()) == Some(15_000)
    });
    let found_scord = asteroids.iter().any(|a| {
        get_dict_val(a, "asteroidID").and_then(|v| v.as_i64()) == Some(ast2_id)
            && get_dict_val(a, "quantity").and_then(|v| v.as_i64()) == Some(8_000)
    });
    assert!(found_veld && found_scord, "Scan results must report accurate quantities and types");
}

// ====================================================================
// Test 8: MN-11: Compression In Space & Structure, Gas Decompression
// ====================================================================
#[tokio::test]
// parity: feat:mining.compression-space, feat:mining.compression-structure, feat:mining.gas-decompression
async fn test_mn11_compression_space_and_structure_and_gas() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let undock = ctx
        .dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();

    // 1. In-space compression: 1,234 Veldspar in ore hold (portionSize = 100)
    let veld_item_id = 800_000_140i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            veld_item_id,
            InventoryItemRecord {
                item_id: veld_item_id,
                type_id: 1230, // Veldspar
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedOreHold,
                quantity: 1_234,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let comp_space_res = ctx
        .dispatch_rpc(
            &mut session,
            "inSpaceCompressionMgr",
            None,
            "CompressItemInSpace",
            &[PyRep::Integer(veld_item_id)],
            &kw,
            None,
        )
        .await
        .expect("CompressItemInSpace");
    let comp_units = get_dict_val(&comp_space_res, "compressedUnits").unwrap().as_i64().unwrap();
    let rem_units = get_dict_val(&comp_space_res, "remainderUnits").unwrap().as_i64().unwrap();
    assert_eq!(comp_units, 12, "1,234 units / portionSize 100 = 12 compressed units");
    assert_eq!(rem_units, 34, "1,234 units % 100 = 34 remainder units");

    // 2. Structure compression and gas decompression:
    let structure_id = 1_000_000_001_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                owner_corp_id: 1000001,
                type_id: 35835, // Athanor
                name: "Perimeter Mining Hub".into(),
                solar_system_id: 30000144,
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(structure_id);
        }
    });
    session.station_id = None;
    session.structure_id = Some(structure_id);

    let gas_item_id = 800_000_141i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            gas_item_id,
            InventoryItemRecord {
                item_id: gas_item_id,
                type_id: 62580, // Compressed Fullerite-C50
                owner_id: char_id,
                location_id: structure_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let decomp_res = ctx
        .dispatch_rpc(
            &mut session,
            "structureCompressionMgr",
            None,
            "DecompressGasInStructure",
            &[PyRep::Integer(gas_item_id)],
            &kw,
            None,
        )
        .await
        .expect("DecompressGasInStructure");
    let decomp_units = get_dict_val(&decomp_res, "decompressedUnits").unwrap().as_i64().unwrap();
    assert!(decomp_units >= 100, "Decompressing gas must return raw gas units");
}

// ====================================================================
// Test 9: MN-12, C-6: Real Reprocessing Yield, Tax, Scrapmetal, Structure
// ====================================================================
#[tokio::test]
// parity: feat:mining.reprocessing-yield, feat:mining.reprocessing-tax, feat:mining.reprocessing-modules, feat:structures.service-reprocessing
async fn test_mn12_c6_reprocessing_yield_tax_modules_and_structure() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let station_id = 60003760i64; // Jita IV - 4
    session.station_id = Some(station_id);

    // 1. Reprocess 1,000 Veldspar (portionSize 100 = 10 portions)
    let veld_item_id = 800_000_150i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            veld_item_id,
            InventoryItemRecord {
                item_id: veld_item_id,
                type_id: 1230, // Veldspar
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let reproc_res = ctx
        .dispatch_rpc(
            &mut session,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(veld_item_id)],
            &kw,
            None,
        )
        .await
        .expect("reprocessingSvc.Reprocess");
    let yield_rate = get_dict_val(&reproc_res, "yieldRate").unwrap().as_f64().unwrap();
    let trit_qty = get_dict_val(&reproc_res, "tritaniumQuantity").unwrap().as_i64().unwrap();
    let tax_rate = get_dict_val(&reproc_res, "stationTaxRate").unwrap().as_f64().unwrap();
    assert!(yield_rate > 0.65, "Yield rate with Reprocessing V + Efficiency V + Veldspar V must exceed 65%");
    assert!(trit_qty > 2_500, "10 portions * 400 Trit * yield must produce > 2500 Tritanium");
    assert!(tax_rate <= 0.05, "Station tax rate must be <= 5%");

    // 2. Reprocess Module with Scrapmetal Processing (12196):
    let module_item_id = 800_000_151i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            module_item_id,
            InventoryItemRecord {
                item_id: module_item_id,
                type_id: 482, // Miner II (has typeMaterials)
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mod_reproc_res = ctx
        .dispatch_rpc(
            &mut session,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(module_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Reprocess module");
    let mod_yield = get_dict_val(&mod_reproc_res, "yieldRate").unwrap().as_f64().unwrap();
    assert!(mod_yield > 0.50, "Scrapmetal processing must scale module reprocessing yield");

    // 3. Upwell Structure Reprocessing Service (Athanor 52% base):
    let structure_id = 1_000_000_001_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                owner_corp_id: 1000001,
                type_id: 35835, // Athanor (52% base yield)
                name: "Athanor Refinery".into(),
                solar_system_id: 30000142,
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(structure_id);
        }
    });
    session.station_id = None;
    session.structure_id = Some(structure_id);

    let struct_info = ctx
        .dispatch_rpc(
            &mut session,
            "reprocessingSvc",
            None,
            "GetQuotes",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetQuotes on structure");
    let struct_base = get_dict_val(&struct_info, "stationBaseYield").unwrap().as_f64().unwrap();
    assert_eq!(struct_base, 0.52, "Athanor refinery must provide 52% base reprocessing efficiency");
}

// ====================================================================
// Test 10: MN-13, MN-14, MN-16: Moon Probes, Jackpot, Decay, Drill & Ledger
// ====================================================================
#[tokio::test]
// parity: feat:scanning.moon-survey-probe, feat:mining.moon-extraction, feat:mining.moon-fracture, feat:mining.moon-field-lifetime, feat:mining.moon-jackpot, feat:mining.metenox, feat:mining.corp-ledger
async fn test_mn13_mn14_mn16_moon_mining_probes_jackpot_decay_and_ledger() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    let structure_id = 1_000_000_001_003i64;
    let char_corp_id = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(1000170)
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                owner_corp_id: char_corp_id,
                type_id: 35835, // Athanor
                name: "Moon Mining Athanor".into(),
                solar_system_id: 30000142,
                position: [1000.0, 2000.0, 3000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });

    // 1. Moon Survey Probe:
    let probe_res = ctx
        .dispatch_rpc(
            &mut session,
            "moonExtractions",
            None,
            "LaunchMoonProbe",
            &[PyRep::Integer(40009087), PyRep::Integer(16654)],
            &kw,
            None,
        )
        .await
        .expect("LaunchMoonProbe");
    let probe_moon_id = get_dict_val(&probe_res, "moonID").unwrap().as_i64().unwrap();
    assert_eq!(probe_moon_id, 40009087, "Moon probe must scan target moon");
    let PyRep::List(probe_resources) = get_dict_val(&probe_res, "resources").unwrap() else {
        panic!("Expected resources list");
    };
    assert!(!probe_resources.is_empty(), "Moon scan must reveal moon ores");

    // 2. Moon Extraction lifecycle: StartNewExtraction
    let start_res = ctx
        .dispatch_rpc(
            &mut session,
            "moonExtractions",
            None,
            "StartNewExtraction",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect("StartNewExtraction");
    let status = get_dict_val(&start_res, "status").unwrap().as_str().unwrap();
    assert_eq!(status, "Started");

    // 3. Moon Jackpot fracture: FractureChunkWithStructure with jackpot: true
    let mut frac_kw = HashMap::new();
    frac_kw.insert("jackpot".into(), PyRep::Bool(true));
    let frac_res = ctx
        .dispatch_rpc(
            &mut session,
            "moonExtractions",
            None,
            "FractureChunkWithStructure",
            &[PyRep::Integer(structure_id)],
            &frac_kw,
            None,
        )
        .await
        .expect("FractureChunkWithStructure with jackpot");
    let is_jackpot = get_dict_val(&frac_res, "isJackpot").unwrap().as_bool().unwrap();
    let jackpot_mult = get_dict_val(&frac_res, "jackpotBonus").unwrap().as_f64().unwrap();
    assert!(is_jackpot, "isJackpot must be true");
    assert_eq!(jackpot_mult, 2.0, "Jackpot bonus must be 2.0x");

    // 4. Moon field lifetime and decay:
    let mut decay_kw = HashMap::new();
    decay_kw.insert("force".into(), PyRep::Bool(true));
    let decay_res = ctx
        .dispatch_rpc(
            &mut session,
            "moonExtractions",
            None,
            "ExpireMoonField",
            &[PyRep::Integer(structure_id)],
            &decay_kw,
            None,
        )
        .await
        .expect("ExpireMoonField");
    let decayed = get_dict_val(&decay_res, "decayed").unwrap().as_bool().unwrap();
    assert!(decayed, "Moon field must decay when expired");

    // 5. Automated Metenox Moon Drill (MN-14):
    let details = ctx
        .dispatch_rpc(
            &mut session,
            "autoMoonMining",
            None,
            "RequestMiningDetails",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect("RequestMiningDetails");
    let eff = get_dict_val(&details, "efficiency").unwrap().as_f64().unwrap();
    assert_eq!(eff, 0.40, "Metenox drill baseline efficiency must be 40%");

    let output = ctx
        .dispatch_rpc(
            &mut session,
            "autoMoonMining",
            None,
            "RequestMiningCycleOutput",
            &[PyRep::Integer(structure_id), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("RequestMiningCycleOutput");
    let produced = get_dict_val(&output, "unitsProduced").unwrap().as_i64().unwrap();
    let fuel_consumed = get_dict_val(&output, "fuelConsumed").unwrap().as_i64().unwrap();
    assert_eq!(fuel_consumed, 10, "2 cycles * 5 fuel blocks = 10 fuel blocks");
    assert_eq!(produced, 400, "2 * 500 * 0.40 = 400 units produced");

    // 6. Corporate Mining Ledger (MN-16):
    let corp_ledger = ctx
        .dispatch_rpc(
            &mut session,
            "corpMiningLedger",
            None,
            "GetObserversWithMiningEvents",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetObserversWithMiningEvents");
    assert!(corp_ledger.is_crowset(), "GetObserversWithMiningEvents must return CRowset");

    let entries = ctx
        .dispatch_rpc(
            &mut session,
            "corpMiningLedger",
            None,
            "GetObserverLedger",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect("GetObserverLedger");
    assert!(entries.is_crowset(), "GetObserverLedger must return CRowset");

    let ledger_entries = ctx.db.with_snapshot(|snap| {
        snap.observer_mining_ledger
            .get(&structure_id)
            .cloned()
            .unwrap_or_default()
    });
    assert!(!ledger_entries.is_empty(), "Observer ledger must have entries in DB");
    assert_eq!(ledger_entries.last().unwrap().quantity, 400);
}
