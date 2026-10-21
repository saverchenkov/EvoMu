//! Comprehensive Phase 5M Mining, Harvesting, Processing & Moons test suite (`MN-01..MN-14, MN-16, C-5, C-6`).
//!
// parity: feat:mining.mining-crits, feat:ships.special-holds, feat:mining.ore-hold, feat:mining.mining-to-jetcan, feat:mining.modulated-crystals, feat:mining.mining-skills, feat:mining.mining-upgrades, feat:mining.mining-fleet-boosts, feat:mining.ore-ships, feat:mining.mining-destroyers, feat:drones.mining-drones, feat:mining.mining-drones-yield, feat:mining.excavator-drones, feat:modules.survey-scanner, feat:mining.survey-scan, feat:mining.compression-space, feat:mining.compression-structure, feat:mining.gas-decompression, feat:mining.reprocessing-yield, feat:mining.reprocessing-tax, feat:mining.reprocessing-modules, feat:structures.service-reprocessing, feat:scanning.moon-survey-probe, feat:mining.moon-extraction, feat:mining.moon-fracture, feat:mining.moon-field-lifetime, feat:mining.metenox, feat:mining.corp-ledger

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
                    InventoryItemRecord {
                        item_id: new_ship_id,
                        type_id: 606, // Velator
                        owner_id: char_id,
                        location_id: 60_003_760,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    },
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
            (3438, 5),  // Mining Drone Operation V (SDE type_id 3438)
            (3439, 5),  // Legacy Mining Drone Operation alias
            (22541, 5), // Mining Drone Specialization V
            (3436, 5),  // Drone Interfacing V
            (3385, 5),  // Reprocessing V
            (3389, 5),  // Reprocessing Efficiency V
            (12196, 5), // Scrapmetal Processing V
            (18025, 5), // Veldspar Processing V
            (12195, 5), // Veldspar Processing V (SDE type_id 12195)
            (18024, 5), // Scordite Processing V
            (60377, 5), // Simple Ore Processing V
            (22578, 5), // Mining Upgrades V
            (3318, 5),  // Weapon Upgrades V
            (3412, 5),  // Astrometrics V
            (28585, 5), // Industrial Reconfiguration V
            (58956, 5), // Industrial Command Core Skill V
            (62450, 5), // Fleet Compression Logistics V
            (62451, 5), // Capital Fleet Compression Logistics V
            (62452, 5), // Shipboard Compression Technology V
            (62453, 5), // Fleet Compression V
            (62454, 5), // Gas Decompression Efficiency V
            (22536, 5), // Mining Foreman V
            (22552, 5), // Mining Director V
            (3348, 5),  // Leadership V
            (3426, 5),  // CPU Management V
            (3413, 5),  // Power Grid Management V
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

    // Spawn Veldspar (1230) and Kernite (20) Asteroids
    let ast_id = 750_000_101i64;
    let kern_ast_id = 750_000_102i64;
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: ast_id,
        solar_system_id: 30000142,
        belt_id: 40000001,
        type_id: 1230,
        quantity: 50_000,
        initial_quantity: 50_000,
        position: (ship_ball.position.x + 500.0, ship_ball.position.y, ship_ball.position.z),
        radius: 60.0,
    });
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: kern_ast_id,
        solar_system_id: 30000142,
        belt_id: 40000001,
        type_id: 20, // Kernite (mismatched with Veldspar crystal)
        quantity: 20_000,
        initial_quantity: 20_000,
        position: (ship_ball.position.x + 550.0, ship_ball.position.y, ship_ball.position.z),
        radius: 60.0,
    });
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
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: kern_ast_id,
            belt_id: 40000001,
            type_id: 20,
            initial_units: 20_000,
            remaining_units: 20_000,
            unit_volume: 1.2,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: kern_ast_id,
            type_id: 20,
            position: ship_ball.position + DVec3::new(550.0, 0.0, 0.0),
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

    // Fit Modulated Strip Miner II (type 17912) and Veldspar Mining Crystal I (type 18066)
    let laser_id = 800_000_101i64;
    let crystal_id = 800_000_102i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 17478; // Retriever
        }
        snap.items.retain(|_, item| item.location_id != ship_id);
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

    // 1. Cycle 1: Matching crystal on Veldspar (1230)
    let act_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(laser_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(ast_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate mining laser on matching Veldspar");

    let yield_t1 = get_dict_val(&act_res, "yieldVolume").unwrap().as_f64().unwrap();
    let crystal_matched = get_dict_val(&act_res, "crystalMatched").unwrap().as_bool().unwrap();
    let crystal_mult_t1 = get_dict_val(&act_res, "crystalMultiplier").unwrap().as_f64().unwrap();
    let crystal_dmg = get_dict_val(&act_res, "crystalDamage").unwrap().as_f64().unwrap();
    let residue_units = get_dict_val(&act_res, "residueUnits").unwrap().as_i64().unwrap();
    assert!(crystal_matched, "Veldspar crystal must match Veldspar asteroid");
    assert!(crystal_mult_t1 >= 1.5, "T1 matching crystal must give >= 1.5x specialization bonus");
    assert!(crystal_dmg > 0.0, "Mining crystal must take volatility damage per cycle");
    assert!(residue_units > 0, "Modulated strip miner with crystal has waste probability > 0");

    // 2. Cycle 2: Mismatched crystal on Kernite (20) -> penalized yield (0.25x)
    let mismatch_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(laser_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(kern_ast_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate mining laser on mismatched Kernite");
    let mismatch_matched = get_dict_val(&mismatch_res, "crystalMatched").unwrap().as_bool().unwrap();
    let mismatch_mult = get_dict_val(&mismatch_res, "crystalMultiplier").unwrap().as_f64().unwrap();
    let yield_mismatch = get_dict_val(&mismatch_res, "yieldVolume").unwrap().as_f64().unwrap();
    assert!(!mismatch_matched, "Veldspar crystal must not match Kernite asteroid");
    assert_eq!(mismatch_mult, 0.25, "Mismatched crystal must apply 0.25x yield penalty");
    assert!(yield_mismatch < yield_t1, "Mismatched crystal yield volume must be much lower than matched T1");

    // 3. Upgrade crystal to Veldspar Mining Crystal II (18618, T2)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(cr) = snap.items.get_mut(&crystal_id) {
            cr.type_id = 18618; // Veldspar Mining Crystal II
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // Cycle 3 & Cycle 4: T2 matching crystal on Veldspar
    let t2_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(laser_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(ast_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate with T2 crystal (cycle 3)");
    let yield_t2 = get_dict_val(&t2_res, "yieldVolume").unwrap().as_f64().unwrap();
    let crystal_mult_t2 = get_dict_val(&t2_res, "crystalMultiplier").unwrap().as_f64().unwrap();
    assert!(crystal_mult_t2 > crystal_mult_t1, "T2 matching crystal must give higher specialization bonus than T1 ({crystal_mult_t2} > {crystal_mult_t1})");
    assert!(yield_t2 > yield_t1, "T2 crystal must yield more volume than T1 crystal");

    // 4. Natural probabilistic mining critical hit (`5967` `miningCritChance = 0.01`, `5969` `miningCritBonusYield = 2.0`):
    // Cycle the strip miner until a natural CSPRNG critical hit occurs, verifying both non-crit cycles
    // and that bonus crit units do NOT deplete additional units from the asteroid rock.
    let mut saw_non_crit = false;
    let mut crit_verified = false;
    for _ in 0..2000 {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(a) = snap.asteroids.get_mut(&ast_id) {
                a.quantity = 500_000;
            }
            if let Some(c) = snap.items.get_mut(&crystal_id) {
                c.dynamic_attributes
                    .insert(eve_sde::attr_ids::HP_STRUCT, 1.0);
            }
            snap.items
                .retain(|_, i| !(i.location_id == ship_id && i.type_id == 1230));
        });
        let ast_before_crit = ctx
            .db
            .with_snapshot(|snap| snap.asteroids.get(&ast_id).map(|a| a.quantity).unwrap());
        let hold_before_crit: i64 = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| i.location_id == ship_id && i.type_id == 1230)
                .map(|i| i.quantity as i64)
                .sum()
        });

        let cycle_res = ctx
            .dispatch_rpc(
                &mut session,
                "dogmaIM",
                None,
                "Activate",
                &[
                    PyRep::Integer(laser_id),
                    PyRep::String("miningLaser".into()),
                    PyRep::Integer(ast_id),
                ],
                &kw,
                None,
            )
            .await
            .expect("Activate mining cycle");

        let is_crit = get_dict_val(&cycle_res, "isCrit")
            .unwrap()
            .as_bool()
            .unwrap();
        if !is_crit {
            saw_non_crit = true;
            continue;
        }

        let crit_mult = get_dict_val(&cycle_res, "critYieldMultiplier")
            .unwrap()
            .as_f64()
            .unwrap();
        let normal_units = get_dict_val(&cycle_res, "normalUnits")
            .unwrap()
            .as_i64()
            .unwrap() as u64;
        let bonus_crit_units = get_dict_val(&cycle_res, "bonusCritUnits")
            .unwrap()
            .as_i64()
            .unwrap() as u64;
        let total_yield_units = get_dict_val(&cycle_res, "yieldUnits")
            .unwrap()
            .as_i64()
            .unwrap() as u64;
        let crit_residue_units = get_dict_val(&cycle_res, "residueUnits")
            .unwrap()
            .as_i64()
            .unwrap() as u64;

        assert_eq!(
            crit_mult, 2.0,
            "Mining crit bonus yield multiplier must be 2.0x"
        );
        assert!(
            bonus_crit_units > 0,
            "Mining crit must grant free bonus ore units"
        );
        assert_eq!(total_yield_units, normal_units + bonus_crit_units);

        let ast_after_crit = ctx
            .db
            .with_snapshot(|snap| snap.asteroids.get(&ast_id).map(|a| a.quantity).unwrap());
        let hold_after_crit: i64 = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| i.location_id == ship_id && i.type_id == 1230)
                .map(|i| i.quantity as i64)
                .sum()
        });

        let rock_depleted = (ast_before_crit - ast_after_crit) as u64;
        let hold_gained = (hold_after_crit - hold_before_crit) as u64;
        assert_eq!(
            rock_depleted,
            normal_units + crit_residue_units,
            "Asteroid rock must only lose normalUnits + residueUnits (bonusCritUnits are generated free of rock depletion)"
        );
        assert_eq!(
            hold_gained, total_yield_units,
            "Ship ore hold must receive normalUnits + bonusCritUnits"
        );
        crit_verified = true;
        if saw_non_crit {
            break;
        }
    }
    assert!(
        saw_non_crit && crit_verified,
        "Probabilistic mining crit check must observe both normal cycles and critical hits"
    );
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
    let jita = ctx.systems.get_or_spawn(30000142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // Ship is Venture (type 32880) with SpecializedOreHold (flag 134, 5000 m3) and Miner II (482)
    let laser_id = 800_000_105i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880; // Venture
        }
        snap.items.retain(|_, item| item.location_id != ship_id);
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
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let ast_id = 750_000_105i64;
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: ast_id,
        solar_system_id: 30000142,
        belt_id: 40000001,
        type_id: 1230,
        quantity: 50_000,
        initial_quantity: 50_000,
        position: (ship_ball.position.x + 600.0, ship_ball.position.y, ship_ball.position.z),
        radius: 60.0,
    });
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

    // 1. Normal mining cycle via dogmaIM.Activate deposits into Venture's SpecializedOreHold (134)
    let mine_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(laser_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(ast_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate Miner II on Venture");
    let flag_id = get_dict_val(&mine_res, "flagID").unwrap().as_i64().unwrap();
    assert_eq!(
        flag_id,
        InventoryFlag::SpecializedOreHold as i64,
        "Venture must deposit mined ore directly into SpecializedOreHold (134)"
    );

    // 2. Specialized holds item-group filtering & capacity limits (`feat:ships.special-holds`, `feat:mining.ore-hold`):
    let mineral_stack_id = 800_000_201i64;
    let oversized_ore_id = 800_000_202i64;
    let ammo_stack_id = 800_000_203i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mineral_stack_id,
            InventoryItemRecord {
                item_id: mineral_stack_id,
                type_id: 34, // Tritanium (Mineral, group 18)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            oversized_ore_id,
            InventoryItemRecord {
                item_id: oversized_ore_id,
                type_id: 1230, // Veldspar (0.1 m3/unit * 60,000 = 6,000 m3 > 5,000 m3 Venture Ore Hold)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 60_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ammo_stack_id,
            InventoryItemRecord {
                item_id: ammo_stack_id,
                type_id: 18066, // Veldspar Mining Crystal I (category 8 Charge)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Moving minerals into Venture's Ore Hold (134) must fail with InvalidItemTypeForHold
    let err_min_to_ore = ctx
        .dispatch_rpc(
            &mut session,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(mineral_stack_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::SpecializedOreHold as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Minerals must be rejected by SpecializedOreHold");
    assert_eq!(err_min_to_ore.code, "InvalidItemTypeForHold");

    // Moving 6,000 m3 of Veldspar into Venture's 5,000 m3 Ore Hold (134) must fail with CapacityExceeded
    let err_overflow = ctx
        .dispatch_rpc(
            &mut session,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(oversized_ore_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::SpecializedOreHold as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("6,000 m3 ore must exceed Venture 5,000 m3 Ore Hold");
    assert_eq!(err_overflow.code, "CapacityExceeded");

    // Switch ship to Kryos (654, SpecializedMineralHold flag 136):
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 654; // Kryos
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // Moving raw ore into Kryos's Mineral Hold (136) must fail with InvalidItemTypeForHold
    let mined_ore_item_id = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold)
            .map(|i| i.item_id)
            .unwrap()
    });
    let err_ore_to_min = ctx
        .dispatch_rpc(
            &mut session,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(mined_ore_item_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(InventoryFlag::SpecializedMineralHold as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Raw ore must be rejected by SpecializedMineralHold");
    assert_eq!(err_ore_to_min.code, "InvalidItemTypeForHold");

    // Moving Tritanium (34) into Kryos's Mineral Hold (136) must succeed
    ctx.dispatch_rpc(
        &mut session,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(mineral_stack_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::SpecializedMineralHold as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Tritanium must be accepted into Kryos SpecializedMineralHold");

    // Switch ship to Hoarder (651, SpecializedAmmoHold flag 143):
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 651; // Hoarder
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // Moving Charge/Crystal (cat 8) into Hoarder's SpecializedAmmoHold (143) must succeed
    ctx.dispatch_rpc(
        &mut session,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(ammo_stack_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::SpecializedAmmoHold as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Charges must be accepted into Hoarder SpecializedAmmoHold");

    // 3. Jettison mined ore into a jetcan, loot from a second character (Suspect flag), and verify expiry (`feat:mining.mining-to-jetcan`):
    let jettison_res = ctx
        .dispatch_rpc(
            &mut session,
            "ship",
            None,
            "Jettison",
            &[PyRep::List(vec![PyRep::Integer(mined_ore_item_id)])],
            &kw,
            None,
        )
        .await
        .expect("ship.Jettison mined ore");
    let jetcan_id = get_dict_val(&jettison_res, "jetcanID").unwrap().as_i64().unwrap();
    assert!(jetcan_id > 0, "ship.Jettison must spawn a valid Cargo Container jetcan");

    let can_exists = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&jetcan_id)
            .is_some_and(|i| i.type_id == 23 && i.location_id == 30000142)
    });
    assert!(can_exists, "Jetcan cargo container (type 23) must exist in system space");
    assert!(
        jita.get_ball_snapshot(jetcan_id).await.unwrap().is_some(),
        "Jetcan ball must exist in solar system ballpark"
    );

    // Second character loots the foreign jetcan -> receives CrimeWatch Suspect flag
    let thief_char_id = 90_000_002i64;
    let mut thief_session = setup_test_session(&ctx, thief_char_id).await;
    let thief_undock = ctx
        .dispatch_rpc(&mut thief_session, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock thief");
    let thief_ship_id = get_dict_val(&thief_undock, "shipID").unwrap().as_i64().unwrap();

    let loot_res = ctx
        .dispatch_rpc(
            &mut thief_session,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(mined_ore_item_id),
                PyRep::Integer(thief_ship_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("Thief loots ore from foreign jetcan");
    let suspect_flagged = get_dict_val(&loot_res, "suspectFlagged").unwrap().as_bool().unwrap();
    assert!(suspect_flagged, "Looting from another pilot's jetcan must trigger a CrimeWatch Suspect flag");
    let is_suspect_in_cw = ctx
        .crimewatch
        .read()
        .unwrap()
        .states
        .get(&thief_char_id)
        .is_some_and(|cw| cw.suspect_flag_until_unix > ctx.now_unix());
    assert!(is_suspect_in_cw, "Thief character must have active Suspect timer in CrimeWatch state");

    // Advance virtual clock past jetcan lifetime (7201s) and verify automatic jetcan despawn
    ctx.advance_time_seconds(7201);
    let can_after_expiry = ctx.db.with_snapshot(|snap| snap.items.contains_key(&jetcan_id));
    assert!(!can_after_expiry, "Expired jetcan must be removed from DB after lifetime");
    assert!(
        jita.get_ball_snapshot(jetcan_id).await.unwrap().is_none(),
        "Expired jetcan ball must be despawned from system ballpark"
    );
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
        quantity: 200_000,
        initial_quantity: 200_000,
        position: (ship_ball.position.x + 800.0, ship_ball.position.y, ship_ball.position.z),
        radius: 80.0,
    });

    let laser_id = 800_000_107i64;
    let mlu1_id = 800_000_108i64;
    let mlu2_id = 800_000_112i64;

    // 1. Mining Skills & Implants (`feat:mining.mining-skills`):
    // Start with Pioneer (89240, has 3 low slots and turret hardpoints), Miner II (482), low skills (Mining IV, Astrogeology 0)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 89240; // Pioneer
        }
        snap.items.retain(|_, item| item.location_id != ship_id);
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
        if let Some(skills) = snap.skills.get_mut(&char_id) {
            skills.insert(3386, TrainedSkillRecord { type_id: 3386, level: 4, skill_points: 45_255.0 });
            skills.remove(&3410);
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let low_skill_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(laser_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(ast_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate Miner II with low skills");
    let yield_low_skill = get_dict_val(&low_skill_res, "yieldVolume").unwrap().as_f64().unwrap();

    // Train Mining V (3386) + Astrogeology V (3410)
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(3386, TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 });
        skills.insert(3410, TrainedSkillRecord { type_id: 3410, level: 5, skill_points: 768_000.0 });
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let high_skill_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(laser_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(ast_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate Miner II with Mining V + Astrogeology V");
    let yield_high_skill = get_dict_val(&high_skill_res, "yieldVolume").unwrap().as_f64().unwrap();
    assert!(
        yield_high_skill > yield_low_skill * 1.25,
        "Mining V (+25%) and Astrogeology V (+25%) must substantially increase laser yield ({yield_high_skill} vs {yield_low_skill})"
    );

    // Plug in Highwall MX-1005 implant (22535, +5% mining yield)
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.entry(char_id).or_default().push(22535);
    });
    let implant_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(laser_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(ast_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate Miner II with Highwall MX-1005 implant");
    let yield_implant = get_dict_val(&implant_res, "yieldVolume").unwrap().as_f64().unwrap();
    let imp_mult = get_dict_val(&implant_res, "implantYieldMultiplier").unwrap().as_f64().unwrap();
    assert!((imp_mult - 1.05).abs() < 1e-6, "Highwall MX-1005 must provide 1.05x implant yield multiplier");
    assert!(yield_implant > yield_high_skill, "Mining implant must increase laser yield");

    // 2. Mining Laser Upgrades (`feat:mining.mining-upgrades`):
    // Measure CPU load and yield with 0x, 1x, and 2x Mining Laser Upgrade II (28576, group 546)
    let cpu_0_mlu = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .unwrap()
        .ship
        .get_attribute(eve_sde::attr_ids::CPU_LOAD, &ctx.sde);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mlu1_id,
            InventoryItemRecord {
                item_id: mlu1_id,
                type_id: 28576, // Mining Laser Upgrade II (+9% yield, group 546)
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
    let cpu_1_mlu = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .unwrap()
        .ship
        .get_attribute(eve_sde::attr_ids::CPU_LOAD, &ctx.sde);
    assert!(cpu_1_mlu > cpu_0_mlu, "Fitting Mining Laser Upgrade II must increase ship CPU load");

    let normal_0_mlu = get_dict_val(&implant_res, "normalUnits").unwrap().as_i64().unwrap() as f64;
    let mlu1_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(laser_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(ast_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate with 1x MLU II");
    let normal_1_mlu = get_dict_val(&mlu1_res, "normalUnits").unwrap().as_i64().unwrap() as f64;
    let mult_1_mlu = get_dict_val(&mlu1_res, "mluMultiplier").unwrap().as_f64().unwrap();
    assert!(normal_1_mlu > normal_0_mlu, "1x MLU II must increase mining yield");

    // Fit 2nd Mining Laser Upgrade II in LoSlot1 -> stacking-penalized bonus
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mlu2_id,
            InventoryItemRecord {
                item_id: mlu2_id,
                type_id: 28576,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    let mlu2_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(laser_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(ast_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate with 2x MLU II");
    let normal_2_mlu = get_dict_val(&mlu2_res, "normalUnits").unwrap().as_i64().unwrap() as f64;
    let mult_2_mlu = get_dict_val(&mlu2_res, "mluMultiplier").unwrap().as_f64().unwrap();
    let gain_first = mult_1_mlu - 1.0;
    let gain_second = mult_2_mlu - mult_1_mlu;
    assert!(normal_2_mlu > normal_1_mlu, "2nd MLU II must further increase normal yield");
    assert!(
        gain_second < gain_first,
        "2nd MLU II multiplier gain ({gain_second}) must be stacking-penalized relative to 1st ({gain_first})"
    );

    // 3. Real Fleet Mining Foreman Bursts (`feat:mining.mining-fleet-boosts`):
    // Create a fleet with a second character flying an Orca (28606) in the same system,
    // fitted with Mining Foreman Burst I (42528) + Mining Laser Optimization Charge (42830).
    let cycle_before_boost = get_dict_val(&mlu2_res, "cycleTimeMs").unwrap().as_i64().unwrap();
    assert!(!get_dict_val(&mlu2_res, "fleetBoostActive").unwrap().as_bool().unwrap());

    let booster_char_id = 90_000_002i64;
    let mut booster_sess = setup_test_session(&ctx, booster_char_id).await;
    let booster_undock = ctx
        .dispatch_rpc(&mut booster_sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock booster");
    let booster_ship_id = get_dict_val(&booster_undock, "shipID").unwrap().as_i64().unwrap();

    let burst_mod_id = 800_000_250i64;
    let burst_charge_id = 800_000_251i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(bship) = snap.items.get_mut(&booster_ship_id) {
            bship.type_id = 28606; // Orca
        }
        snap.items.retain(|_, item| item.location_id != booster_ship_id);
        snap.items.insert(
            burst_mod_id,
            InventoryItemRecord {
                item_id: burst_mod_id,
                type_id: 42528, // Mining Foreman Burst I (group 1770)
                owner_id: booster_char_id,
                location_id: booster_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            burst_charge_id,
            InventoryItemRecord {
                item_id: burst_charge_id,
                type_id: 42830, // Mining Laser Optimization Charge (group 1771)
                owner_id: booster_char_id,
                location_id: booster_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&booster_ship_id);

    // Form fleet and put both characters in it
    let fleet_init = ctx
        .dispatch_rpc(&mut booster_sess, "fleetSvc", None, "InitFleet", &[], &kw, None)
        .await
        .expect("fleetSvc.InitFleet");
    let fleet_id = get_dict_val(&fleet_init, "fleetID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(fl) = snap.fleets.get_mut(&fleet_id) {
            if !fl.members.contains(&char_id) {
                fl.members.push(char_id);
            }
        }
    });

    // Booster activates Mining Foreman Burst I via dogmaIM.Activate
    ctx.dispatch_rpc(
        &mut booster_sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(burst_mod_id), PyRep::String("commandBurst".into())],
        &kw,
        None,
    )
    .await
    .expect("Activate Mining Foreman Burst I on Orca");

    // Miner activates mining laser (with NO fleetBoost kwarg!) -> receives active fleet burst
    let fleet_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(laser_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(ast_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate laser under real fleet Mining Foreman Burst");
    let cycle_time = get_dict_val(&fleet_res, "cycleTimeMs").unwrap().as_i64().unwrap();
    let fleet_active = get_dict_val(&fleet_res, "fleetBoostActive").unwrap().as_bool().unwrap();
    assert!(fleet_active, "fleetBoostActive must be true from active fleet Mining Foreman Burst");
    assert!(
        cycle_time < cycle_before_boost,
        "Active Mining Foreman Burst must reduce laser cycle duration ({cycle_time} < {cycle_before_boost})"
    );

    // 4. Mining Destroyers (`feat:mining.mining-destroyers`: Pioneer 89240 and Perseverance 91174)
    // and ORE Barges / Exhumers (`feat:mining.ore-ships`: Covetor 17476, Procurer 17480, Retriever 17478, Hulk 22544, Skiff 22546, Mackinaw 22548):
    for destroyer_tid in [89240u32, 91174u32] {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ship) = snap.items.get_mut(&ship_id) {
                ship.type_id = destroyer_tid;
            }
        });
        ctx.ship_fittings.write().unwrap().remove(&ship_id);
        let dest_res = ctx
            .dispatch_rpc(
                &mut session,
                "dogmaIM",
                None,
                "Activate",
                &[
                    PyRep::Integer(laser_id),
                    PyRep::String("miningLaser".into()),
                    PyRep::Integer(ast_id),
                ],
                &kw,
                None,
            )
            .await
            .expect("Activate on Mining Destroyer");
        let dest_yield = get_dict_val(&dest_res, "yieldVolume").unwrap().as_f64().unwrap();
        assert!(dest_yield > 0.0, "Mining destroyer {destroyer_tid} must receive hull mining bonus");
    }

    // Fit Strip Miner I (type 17482) and compare Mining Barge (Retriever 17478) vs Exhumer (Hulk 22544)
    let strip_id = 800_000_109i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 17478; // Retriever
        }
        snap.items.retain(|_, item| item.location_id != ship_id);
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
    let ret_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(strip_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(ast_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate on Retriever");
    let ret_yield = get_dict_val(&ret_res, "yieldVolume").unwrap().as_f64().unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 22544; // Hulk (Exhumer)
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    let hulk_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(strip_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(ast_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate on Hulk");
    let hulk_yield = get_dict_val(&hulk_res, "yieldVolume").unwrap().as_f64().unwrap();
    assert!(
        hulk_yield > ret_yield,
        "Hulk exhumer with Exhumers V must out-mine Retriever barge ({hulk_yield} > {ret_yield})"
    );
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

    let ice_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(ice_harvester_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(ice_ast_id),
            ],
            &kw,
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

    let gas_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(gas_harvester_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(gas_ast_id),
            ],
            &kw,
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

    // Attempting to mine while drone is in DroneBay must fail with DroneNotInSpace
    let err_in_bay = ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Mining drone in DroneBay cannot mine until launched");
    assert_eq!(err_in_bay.code, "DroneNotInSpace");

    // Launch drone into space via ship.LaunchDrones
    ctx.dispatch_rpc(
        &mut session,
        "ship",
        None,
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(drone_id)])],
        &kw,
        None,
    )
    .await
    .expect("ship.LaunchDrones");

    // Measure drone yield with low skills first
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(skills) = snap.skills.get_mut(&char_id) {
            skills.insert(3438, TrainedSkillRecord { type_id: 3438, level: 1, skill_points: 1000.0 });
            skills.remove(&3439);
            skills.remove(&3436);
            skills.remove(&22541);
        }
    });
    let low_drone_res = ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdMineRepeatedly low skills");
    let low_mined_units = get_dict_val(&low_drone_res, "minedUnits").unwrap().as_i64().unwrap();

    // Restore Mining Drone Operation V (3438), Drone Interfacing V (3436), Mining Drone Spec V (22541),
    // and fit Large Drone Mining Augmentor I rig (25918, group 778, +10% yield)
    let rig_id = 800_000_122i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(3438, TrainedSkillRecord { type_id: 3438, level: 5, skill_points: 500_000.0 });
        skills.insert(3436, TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 500_000.0 });
        skills.insert(22541, TrainedSkillRecord { type_id: 22541, level: 5, skill_points: 500_000.0 });
        snap.items.insert(
            rig_id,
            InventoryItemRecord {
                item_id: rig_id,
                type_id: 25918, // Large Drone Mining Augmentor I (group 778)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let drone_res = ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdMineRepeatedly high skills + rig");
    let mined_units = get_dict_val(&drone_res, "minedUnits").unwrap().as_i64().unwrap();
    let is_drone = get_dict_val(&drone_res, "isDrone").unwrap().as_bool().unwrap();
    assert!(is_drone, "Must be flagged as drone mining");
    assert!(
        mined_units > low_mined_units,
        "Mining Drone Operation V + Drone Interfacing V + Drone Mining Augmentor rig must boost drone yield ({mined_units} > {low_mined_units})"
    );
    assert!(mined_units >= 300, "Mining Drone II with V/V skills must yield >= 300 units");

    // Return Mining Drone II to bay via entity.CmdReturnBay
    ctx.dispatch_rpc(
        &mut session,
        "entity",
        None,
        "CmdReturnBay",
        &[PyRep::List(vec![PyRep::Integer(drone_id)])],
        &kw,
        None,
    )
    .await
    .expect("entity.CmdReturnBay");

    // 2. 'Excavator' Mining Drone (type 41030, `feat:mining.excavator-drones`):
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

    ctx.dispatch_rpc(
        &mut session,
        "ship",
        None,
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(exc_drone_id)])],
        &kw,
        None,
    )
    .await
    .expect("Launch Excavator drone");

    // Attempting to mine with Excavator on a Retriever (non-Rorqual) must fail with InvalidShipForExcavator
    let err_non_rorq = ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(exc_drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Excavator must require a Rorqual hull");
    assert_eq!(err_non_rorq.code, "InvalidShipForExcavator");

    // Switch ship to Rorqual (28352) and fit Capital Industrial Core I (28583, group 515)
    let ind_core_id = 800_000_123i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 28352; // Rorqual
        }
        snap.items.retain(|_, item| item.location_id != ship_id || item.flag == InventoryFlag::DroneBay);
        snap.items.insert(
            ind_core_id,
            InventoryItemRecord {
                item_id: ind_core_id,
                type_id: 28583, // Capital Industrial Core I (group 515)
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

    // Without active Industrial Core, Excavator must fail with IndustrialCoreRequired
    let err_no_core = ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(exc_drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Excavator must require active Industrial Core on Rorqual");
    assert_eq!(err_no_core.code, "IndustrialCoreRequired");

    // Activate Capital Industrial Core I via dogmaIM.Activate
    ctx.dispatch_rpc(
        &mut session,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(ind_core_id), PyRep::String("industrialCore".into())],
        &kw,
        None,
    )
    .await
    .expect("Activate Capital Industrial Core I");

    let exc_res = ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(exc_drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdMineRepeatedly with Excavator on active-core Rorqual");
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

    let activate_scan_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(scanner_id),
                PyRep::String("surveyScan".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("dogmaIM.Activate Survey Scanner");
    let act_count = get_dict_val(&activate_scan_res, "asteroidCount")
        .unwrap()
        .as_i64()
        .unwrap();
    assert!(
        act_count >= 2,
        "dogmaIM.Activate on Survey Scanner must detect nearby asteroids in range"
    );

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

    // 1. In-space compression (`feat:mining.compression-space`):
    // 1,234 Veldspar in ore hold (portionSize = 100)
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

    // Attempting in-space compression without an active Orca/Rorqual/Porpoise compressor in range must fail
    let err_no_comp = ctx
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
        .expect_err("In-space compression without active compressor must fail");
    assert_eq!(err_no_comp.code, "NoActiveCompressorInRange");

    // Switch ship to Orca (28606), fit Large Asteroid Ore Compressor I (62625, group 4174) and Large Industrial Core I (58945, group 515)
    let comp_mod_id = 800_000_142i64;
    let core_mod_id = 800_000_145i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 28606; // Orca
        }
        snap.items.retain(|_, item| item.location_id != ship_id || item.flag == InventoryFlag::SpecializedOreHold);
        snap.items.insert(
            comp_mod_id,
            InventoryItemRecord {
                item_id: comp_mod_id,
                type_id: 62625, // Large Asteroid Ore Compressor I (group 4174)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            core_mod_id,
            InventoryItemRecord {
                item_id: core_mod_id,
                type_id: 58945, // Large Industrial Core I (group 515)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // Activating compressor module (attr 3265 activationRequiresActiveIndustrialCore = 1.0) without active Industrial Core must fail
    let no_core_act_err = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(comp_mod_id), PyRep::String("compressor".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Activating compressor without active Industrial Core must fail");
    assert_eq!(no_core_act_err.code, "IndustrialCoreNotActive");

    // Activate Large Industrial Core I first, then Large Asteroid Ore Compressor I
    ctx.dispatch_rpc(
        &mut session,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(core_mod_id), PyRep::String("industrialCore".into())],
        &kw,
        None,
    )
    .await
    .expect("Activate Large Industrial Core I");

    ctx.dispatch_rpc(
        &mut session,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(comp_mod_id), PyRep::String("compressor".into())],
        &kw,
        None,
    )
    .await
    .expect("Activate Large Asteroid Ore Compressor I");

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
        .expect("CompressItemInSpace with active Orca compressor");
    let comp_units = get_dict_val(&comp_space_res, "compressedUnits").unwrap().as_i64().unwrap();
    let rem_units = get_dict_val(&comp_space_res, "remainderUnits").unwrap().as_i64().unwrap();
    assert_eq!(comp_units, 1_234, "V24 1:1 compression converts all 1,234 Veldspar units");
    assert_eq!(rem_units, 0, "V24 1:1 compression leaves 0 remainder units");

    // Verify fleetmate in-space compression uses SDE module maxRange (83 km on Large Asteroid Ore Compressor I):
    // Seed a fleetmate in a Venture (32880) with 500 Scordite (1228) in its mining hold at 120 km (out of 83 km range), then move to 50 km (in range).
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
            InventoryItemRecord {
                item_id: fleetmate_ship_id,
                type_id: 32880, // Venture
                owner_id: fleetmate_char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            fleetmate_ore_id,
            InventoryItemRecord {
                item_id: fleetmate_ore_id,
                type_id: 1228, // Scordite
                owner_id: fleetmate_char_id,
                location_id: fleetmate_ship_id,
                flag: InventoryFlag::SpecializedOreHold,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let fleet_init = ctx
        .dispatch_rpc(&mut session, "fleetSvc", None, "InitFleet", &[], &kw, None)
        .await
        .expect("fleetSvc.InitFleet");
    let fleet_id = get_dict_val(&fleet_init, "fleetID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(fl) = snap.fleets.get_mut(&fleet_id) {
            if !fl.members.contains(&fleetmate_char_id) {
                fl.members.push(fleetmate_char_id);
            }
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let orca_pos = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .unwrap()
        .1
        .position;
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

    // Ensure Fleet Compression Logistics (62453) starts at level 0 so base range is 83 km
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sk) = snap.skills.get_mut(&char_id) {
            sk.remove(&62453);
        }
    });

    let out_of_range_err = ctx
        .dispatch_rpc(
            &mut fleetmate_sess,
            "inSpaceCompressionMgr",
            None,
            "CompressItemInSpace",
            &[PyRep::Integer(fleetmate_ore_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Fleetmate at 120 km must be out of 83 km Large Compressor range");
    assert_eq!(out_of_range_err.code, "NoActiveCompressorInRange");

    // Train Fleet Compression Logistics (62453) to level 5 (+50% range -> 83 km * 1.5 = 124.5 km) on the Orca pilot
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            62453,
            eve_db::TrainedSkillRecord {
                type_id: 62453,
                level: 5,
                skill_points: 512_000.0,
            },
        );
    });
    let in_range_res = ctx
        .dispatch_rpc(
            &mut fleetmate_sess,
            "inSpaceCompressionMgr",
            None,
            "CompressItemInSpace",
            &[PyRep::Integer(fleetmate_ore_id)],
            &kw,
            None,
        )
        .await
        .expect("Fleetmate at 120 km must succeed with Fleet Compression Logistics V (124.5 km range)");
    assert_eq!(
        get_dict_val(&in_range_res, "compressedUnits").unwrap().as_i64(),
        Some(500)
    );

    // 2. Structure compression (`feat:mining.compression-structure`) and gas decompression (`feat:mining.gas-decompression`):
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

    let struct_ore_id = 800_000_143i64;
    let gas_item_id = 800_000_141i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            struct_ore_id,
            InventoryItemRecord {
                item_id: struct_ore_id,
                type_id: 1228, // Scordite
                owner_id: char_id,
                location_id: structure_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            gas_item_id,
            InventoryItemRecord {
                item_id: gas_item_id,
                type_id: 62399, // Compressed Fullerite-C50 (raw type 30370, group 711)
                owner_id: char_id,
                location_id: structure_id,
                flag: InventoryFlag::Hangar,
                quantity: 200,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let comp_struct_res = ctx
        .dispatch_rpc(
            &mut session,
            "structureCompressionMgr",
            None,
            "CompressItemInStructure",
            &[PyRep::Integer(struct_ore_id)],
            &kw,
            None,
        )
        .await
        .expect("CompressItemInStructure");
    let struct_comp_units = get_dict_val(&comp_struct_res, "compressedUnits").unwrap().as_i64().unwrap();
    assert_eq!(struct_comp_units, 500, "V24 1:1 compression converts 500 Scordite to 500 Compressed Scordite in structure");

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
    assert!(
        (160..=200).contains(&decomp_units),
        "Decompressing 200 Compressed Fullerite-C50 must return 80%-100% raw gas units (got {decomp_units})"
    );
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
// Test 10: MN-13, MN-14, MN-16: Moon Probes, Extraction, Fracture, Decay, Drill & Ledger
// ====================================================================
#[tokio::test]
// parity: feat:scanning.moon-survey-probe, feat:mining.moon-extraction, feat:mining.moon-fracture, feat:mining.moon-field-lifetime, feat:mining.metenox, feat:mining.corp-ledger
async fn test_mn13_mn14_mn16_moon_mining_probes_jackpot_decay_and_ledger() {
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

    let structure_id = 1_000_000_001_003i64;
    let metenox_id = 1_000_000_001_004i64;
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
        snap.structures.insert(
            metenox_id,
            UpwellStructureRecord {
                structure_id: metenox_id,
                owner_corp_id: char_corp_id,
                type_id: 81826, // Metenox Moon Drill
                name: "Automated Metenox Drill (Asabona Low-Sec)".into(),
                solar_system_id: 30000012, // Asabona (Low-Sec 0.32)
                position: [1500.0, 2500.0, 3500.0],
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
        // Seed Magmatic Gas (type 81143) in Metenox Drill inventory
        snap.items.insert(
            800_000_305,
            InventoryItemRecord {
                item_id: 800_000_305,
                type_id: 81143, // Magmatic Gas
                owner_id: char_corp_id,
                location_id: metenox_id,
                flag: InventoryFlag::Hangar,
                quantity: 200,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 1. Moon Survey Probe (`feat:scanning.moon-survey-probe`):
    // Fit Survey Probe Launcher I (33270, group 1226) and Discovery Survey Probe I (18635, group 492)
    let launcher_id = 800_000_301i64;
    let probe_stack_id = 800_000_302i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 605; // Heron (has launcher hardpoints)
        }
        snap.items.retain(|_, item| item.location_id != ship_id);
        snap.items.insert(
            launcher_id,
            InventoryItemRecord {
                item_id: launcher_id,
                type_id: 33270, // Survey Probe Launcher I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            probe_stack_id,
            InventoryItemRecord {
                item_id: probe_stack_id,
                type_id: 18635, // Discovery Survey Probe I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let probe_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(launcher_id),
                PyRep::String("useMissiles".into()),
                PyRep::Integer(40009087),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate Survey Probe Launcher I");
    let probe_moon_id = get_dict_val(&probe_res, "moonID").unwrap().as_i64().unwrap();
    let rem_probes = get_dict_val(&probe_res, "remainingInLauncher").unwrap().as_i64().unwrap();
    assert_eq!(probe_moon_id, 40009087, "Survey probe launcher must scan target moon");
    assert_eq!(rem_probes, 4, "Survey probe launcher must consume 1 survey probe charge");
    let PyRep::List(probe_resources) = get_dict_val(&probe_res, "composition").unwrap() else {
        panic!("Expected composition list");
    };
    assert!(!probe_resources.is_empty(), "Moon scan must reveal moon ores");

    // 2. Moon Extraction lifecycle & fracture (`feat:mining.moon-extraction`, `feat:mining.moon-fracture`):
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
        .expect("StartNewExtraction #1");
    let status = get_dict_val(&start_res, "status").unwrap().as_str().unwrap();
    assert_eq!(status, "Started");
    let resolved_moon_id = get_dict_val(&start_res, "moonID").unwrap().as_i64().unwrap();
    assert!(
        ctx.sde
            .celestials_in_system(30000142)
            .iter()
            .any(|c| c.group_id == 8 && c.celestial_id == resolved_moon_id),
        "Resolved moonID {resolved_moon_id} must be a real SDE moon in Jita"
    );

    // Attempting to fracture before chunk arrival must be rejected
    let early_err = ctx
        .dispatch_rpc(
            &mut session,
            "moonExtractions",
            None,
            "FractureChunkWithStructure",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Fracture before chunk arrival must fail");
    assert_eq!(early_err.code, "ChunkNotReadyToFracture");

    // Advance virtual clock to chunk arrival (6d = 518,400s)
    ctx.advance_time_seconds(518_400);

    let frac1_res = ctx
        .dispatch_rpc(
            &mut session,
            "moonExtractions",
            None,
            "FractureChunkWithStructure",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect("FractureChunkWithStructure #1");
    let PyRep::List(ast_ids_1) = get_dict_val(&frac1_res, "asteroidIDs").unwrap() else {
        panic!("Expected asteroidIDs list");
    };
    let PyRep::List(ore_types_1) = get_dict_val(&frac1_res, "oreTypes").unwrap() else {
        panic!("Expected oreTypes list");
    };
    assert!(!ast_ids_1.is_empty(), "Fracturing moon chunk must spawn moon ore asteroids");
    let first_ast_id = ast_ids_1[0].as_i64().unwrap();
    let first_ast_type = ore_types_1[0].as_i64().unwrap() as u32;
    assert!(
        ctx.sde
            .types
            .get(&first_ast_type)
            .is_some_and(|t| matches!(t.group_id, 1884 | 1920)),
        "Fractured moon asteroid type {first_ast_type} must belong to SDE moon ore groups 1884/1920"
    );
    assert!(
        jita.get_ball_snapshot(first_ast_id).await.unwrap().is_some(),
        "Moon asteroid ball must exist in solar system ballpark"
    );

    // Repeating FractureChunkWithStructure on an already-fractured extraction must fail
    let dup_err = ctx
        .dispatch_rpc(
            &mut session,
            "moonExtractions",
            None,
            "FractureChunkWithStructure",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Repeated FractureChunkWithStructure must fail");
    assert_eq!(dup_err.code, "ExtractionAlreadyFractured");

    // 3. Moon field lifetime and natural decay (`feat:mining.moon-field-lifetime`):
    // Advance virtual clock past the 48-hour (172,800s) natural decay window
    ctx.advance_time_seconds(172_801);
    let ext_info = ctx
        .dispatch_rpc(
            &mut session,
            "moonExtractions",
            None,
            "GetExtractionAndEventIDFromStructureID",
            &[PyRep::Integer(structure_id)],
            &kw,
            None,
        )
        .await
        .expect("GetExtractionAndEventIDFromStructureID after decay");
    let decayed_status = get_dict_val(&ext_info, "status").unwrap().as_str().unwrap();
    assert_eq!(decayed_status, "Decayed", "Moon field must transition to Decayed after natural_decay_unix");
    assert!(
        jita.get_ball_snapshot(first_ast_id).await.unwrap().is_none(),
        "Decayed moon field asteroids must be despawned from ballpark"
    );

    // 5. Automated Metenox Moon Drill (`feat:mining.metenox`):
    // Verify non-Metenox structure (Athanor 35835) is rejected with InvalidStructureType
    let err_athanor = ctx
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
        .expect_err("Athanor must be rejected by autoMoonMining.RequestMiningDetails");
    assert_eq!(err_athanor.code, "InvalidStructureType");

    let details = ctx
        .dispatch_rpc(
            &mut session,
            "autoMoonMining",
            None,
            "RequestMiningDetails",
            &[PyRep::Integer(metenox_id)],
            &kw,
            None,
        )
        .await
        .expect("RequestMiningDetails on Metenox Moon Drill");
    let eff = get_dict_val(&details, "efficiency").unwrap().as_f64().unwrap();
    assert_eq!(eff, 0.40, "Metenox drill baseline efficiency must be 40%");

    // Advance virtual clock by 2 hours (7,200s) so 2 hourly Metenox cycles complete
    ctx.advance_time_seconds(7_200);

    let output = ctx
        .dispatch_rpc(
            &mut session,
            "autoMoonMining",
            None,
            "RequestMiningCycleOutput",
            &[PyRep::Integer(metenox_id), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("RequestMiningCycleOutput on Metenox Moon Drill");
    let produced = get_dict_val(&output, "unitsProduced").unwrap().as_i64().unwrap();
    let fuel_consumed = get_dict_val(&output, "fuelConsumed").unwrap().as_i64().unwrap();
    let gas_consumed = get_dict_val(&output, "magmaticGasConsumed").unwrap().as_i64().unwrap();
    assert_eq!(fuel_consumed, 10, "2 cycles * 5 fuel blocks = 10 fuel blocks");
    assert_eq!(gas_consumed, 20, "2 cycles * 10 Magmatic Gas = 20 units");
    assert!(produced > 0, "Metenox must produce positive moon material units");

    // 6. Corporate Mining Ledger (`feat:mining.corp-ledger`):
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
            &[PyRep::Integer(metenox_id)],
            &kw,
            None,
        )
        .await
        .expect("GetObserverLedger");
    assert!(entries.is_crowset(), "GetObserverLedger must return CRowset");

    let ledger_entries = ctx.db.with_snapshot(|snap| {
        snap.observer_mining_ledger
            .get(&metenox_id)
            .cloned()
            .unwrap_or_default()
    });
    assert!(!ledger_entries.is_empty(), "Observer ledger must have entries in DB");
    let ledger_sum: i64 = ledger_entries.iter().map(|e| e.quantity).sum();
    assert_eq!(ledger_sum, produced);
}
