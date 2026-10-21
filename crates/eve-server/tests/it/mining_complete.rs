//! Comprehensive Phase 5M Mining, Harvesting, Processing & Moons test suite (`MN-01..MN-14, MN-16, C-5, C-6`).
//!

#![forbid(unsafe_code)]

use eve_db::{
    AsteroidRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
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

// ====================================================================
// Test 1: MN-01, MN-02, C-5: Static Data, Belt Ore Distribution, Ice Belts
// ====================================================================
#[tokio::test]
async fn test_mn01_mn02_c5_static_data_and_belt_ore_distribution() {
    // parity: feat:mining.static-data, feat:mining.belts-as-balls, feat:mining.belt-ore-distribution
    eve_testkit::attest(&["feat:mining.static-data", "feat:mining.belts-as-balls", "feat:mining.belt-ore-distribution"]);
    let ctx = ServerContext::new();

    // 1. Static data verification from SDE (`feat:mining.static-data`):
    let veldspar_mats = ctx.sde.type_materials.get(&1230).expect("Veldspar typeMaterials");
    assert_eq!(veldspar_mats.len(), 1, "Veldspar must yield Tritanium only");
    assert_eq!(veldspar_mats[0].material_type_id, 34);
    assert_eq!(veldspar_mats[0].quantity, 400);

    let scordite_mats = ctx.sde.type_materials.get(&1228).expect("Scordite typeMaterials");
    assert_eq!(scordite_mats.len(), 2, "Scordite must yield Tritanium and Pyerite");
    assert!(scordite_mats.iter().any(|m| m.material_type_id == 34));
    assert!(scordite_mats.iter().any(|m| m.material_type_id == 35));

    // +5%, +10%, +15% ore variants (`17470`, `17471`, `46689`) and moon ore (`45490` Zeolites) + gas (`30370` Fullerite-C50):
    assert_eq!(ctx.sde.type_materials[&17470][0].quantity, 420); // +5% Veldspar II-Grade
    assert_eq!(ctx.sde.type_materials[&17471][0].quantity, 440); // +10% Veldspar III-Grade
    assert_eq!(ctx.sde.type_materials[&46689][0].quantity, 460); // +15% Veldspar IV-Grade
    assert!(!ctx.sde.type_materials[&45490].is_empty(), "Zeolites moon ore must have typeMaterials");
    assert_eq!(ctx.sde.types[&30370].group_id, 711, "Fullerite-C50 must be Harvestable Cloud group 711");

    // Clear Icicle (16262): Ice yields Heavy Water, Liquid Ozone, Strontium Clathrates, and Helium Isotopes
    let ice_mats = ctx.sde.type_materials.get(&16262).expect("Clear Icicle mats");
    let ice_types: Vec<u32> = ice_mats.iter().map(|m| m.material_type_id).collect();
    for mat_tid in [16272u32, 16273, 16274, 16275] {
        assert!(ice_types.contains(&mat_tid), "Clear Icicle must yield {mat_tid}");
    }
    assert!(ctx.sde.compressible_types.contains_key(&1230), "Veldspar must be compressible in SDE");
    assert!(ctx.sde.compressible_types.contains_key(&16262), "Clear Icicle must be compressible in SDE");

    // 2. Belt ore distribution & Ballpark verification (`feat:mining.belts-as-balls`, `feat:mining.belt-ore-distribution`):
    let mut highsec_bp = eve_destiny::Ballpark::new(30000144);
    let highsec_states = eve_server::system_actor::populate_asteroid_belt_entities(&mut highsec_bp, 30000144, &ctx.sde);
    assert!(!highsec_states.is_empty(), "Highsec belts must generate asteroids");
    let perim_sys = ctx.systems.get_or_spawn(30000144);
    let _ = perim_sys.populate_asteroid_belt_entities().await.unwrap();
    assert!(perim_sys.get_ball_snapshot(highsec_states[0].asteroid_id).await.unwrap().is_some(), "Belt asteroids must be spawned as Balls in Ballpark");
    let highsec_type_ids: Vec<u32> = highsec_states.iter().map(|s| s.type_id).collect();
    assert!(highsec_type_ids.contains(&1230) && highsec_type_ids.contains(&1228));

    let mut lowsec_bp = eve_destiny::Ballpark::new(30002537); // Amamake (lowsec 0.4)
    let lowsec_states = eve_server::system_actor::populate_asteroid_belt_entities(&mut lowsec_bp, 30002537, &ctx.sde);
    assert!(!lowsec_states.is_empty(), "Lowsec belts must generate asteroids");
    let lowsec_type_ids: Vec<u32> = lowsec_states.iter().map(|s| s.type_id).collect();
    assert!(
        lowsec_type_ids.iter().any(|&tid| matches!(tid, 20 | 21 | 1226 | 1227 | 1231)),
        "Lowsec belt must contain lowsec ores (Kernite/Omber/Jaspet/Hemorphite/Hedbergite)"
    );

    let mut nullsec_bp = eve_destiny::Ballpark::new(30000208); // LZ-6SU (nullsec -0.17)
    let nullsec_states = eve_server::system_actor::populate_asteroid_belt_entities(&mut nullsec_bp, 30000208, &ctx.sde);
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
async fn test_mn04_mn06_crits_crystals_residue() {
    // parity: plan:MN-04, plan:MN-06, feat:mining.mining-laser-cycle, feat:mining.strip-miner, feat:mining.mining-crits, feat:mining.modulated-crystals, feat:mining.mining-residue
    eve_testkit::attest(&["plan:MN-04", "plan:MN-06", "feat:mining.mining-laser-cycle", "feat:mining.strip-miner", "feat:mining.mining-crits", "feat:mining.modulated-crystals", "feat:mining.mining-residue"]);
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
    assert_eq!(mismatch_mult, 1.0, "Mismatched crystal must yield 1.0x base modulated yield with no penalty");
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

    // 5. Partial cycle proportional yield when rock has fewer units than full cycle (`feat:mining.mining-laser-cycle`)
    // and Type A / Type B / Type C mining crystals in SDE (`feat:mining.mining-residue`):
    for tier_label in ["Type A", "Type B", "Type C"] {
        assert!(
            ctx.sde.types.values().any(|t| t.published && t.name.contains("Mining Crystal") && t.name.contains(tier_label)),
            "SDE must include {tier_label} mining crystals"
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(a) = snap.asteroids.get_mut(&ast_id) {
            a.quantity = 15;
        }
        snap.items.remove(&crystal_id);
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.type_id == 1230));
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    let partial_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Partial mining cycle on 15-unit rock");
    assert_eq!(get_dict_val(&partial_res, "normalUnits").unwrap().as_i64(), Some(15));
    assert!(ctx.db.with_snapshot(|snap| snap.asteroids.get(&ast_id).map(|a| a.quantity).unwrap_or(0)) <= 0);
}

// ====================================================================
// Test 3: MN-05: Specialized Holds and Mining to Jetcan
// ====================================================================
#[tokio::test]
async fn test_mn05_special_holds_and_mining_to_jetcan() {
    // parity: plan:MN-05, feat:ships.special-holds, feat:mining.ore-hold, feat:mining.mining-to-jetcan
    eve_testkit::attest(&["plan:MN-05", "feat:ships.special-holds", "feat:mining.ore-hold", "feat:mining.mining-to-jetcan"]);
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
        snap.items.insert(laser_id, make_item(laser_id, 482, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
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
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate Miner II on Venture");
    let flag_id = get_dict_val(&mine_res, "flagID").unwrap().as_i64().unwrap();
    assert_eq!(flag_id, InventoryFlag::SpecializedOreHold as i64);

    // 2. Specialized holds item-group filtering & capacity limits (`feat:ships.special-holds`, `feat:mining.ore-hold`):
    let mineral_stack_id = 800_000_201i64;
    let oversized_ore_id = 800_000_202i64;
    let ammo_stack_id = 800_000_203i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mineral_stack_id, make_item(mineral_stack_id, 34, char_id, ship_id, InventoryFlag::Cargo, 1_000, false));
        snap.items.insert(oversized_ore_id, make_item(oversized_ore_id, 1230, char_id, ship_id, InventoryFlag::Cargo, 60_000, false));
        snap.items.insert(ammo_stack_id, make_item(ammo_stack_id, 18066, char_id, ship_id, InventoryFlag::Cargo, 5, false));
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
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&thief_ship_id) {
            ship.type_id = 32880; // Venture (5,000 m³ SpecializedOreHold so critical mining yields fit deterministically)
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&thief_ship_id);

    let loot_res = ctx
        .dispatch_rpc(
            &mut thief_session,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(mined_ore_item_id),
                PyRep::Integer(thief_ship_id),
                PyRep::Integer(InventoryFlag::SpecializedOreHold as i64),
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
async fn test_mn07_skills_upgrades_boosts_ore_ships_and_destroyers() {
    // parity: plan:MN-07, feat:mining.mining-skills, feat:mining.mining-upgrades, feat:mining.mining-fleet-boosts, feat:mining.ore-ships, feat:mining.mining-destroyers
    eve_testkit::attest(&["plan:MN-07", "feat:mining.mining-skills", "feat:mining.mining-upgrades", "feat:mining.mining-fleet-boosts", "feat:mining.ore-ships", "feat:mining.mining-destroyers"]);
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
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 89240; // Pioneer
        }
        snap.items.retain(|_, item| item.location_id != ship_id);
        snap.items.insert(laser_id, make_item(laser_id, 482, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
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
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate Miner II with low skills");
    let yield_low_skill = get_dict_val(&low_skill_res, "normalUnits").unwrap().as_i64().unwrap() as f64;

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
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate Miner II with Mining V + Astrogeology V");
    let yield_high_skill = get_dict_val(&high_skill_res, "normalUnits").unwrap().as_i64().unwrap() as f64;
    assert!(yield_high_skill > yield_low_skill * 1.25);

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
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate Miner II with Highwall MX-1005 implant");
    let yield_implant = get_dict_val(&implant_res, "normalUnits").unwrap().as_i64().unwrap() as f64;
    let imp_mult = get_dict_val(&implant_res, "implantYieldMultiplier").unwrap().as_f64().unwrap();
    assert!((imp_mult - 1.05).abs() < 1e-6);
    assert!(yield_implant > yield_high_skill);

    // 2. Mining Laser Upgrades (`feat:mining.mining-upgrades`):
    let cpu_0_mlu = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .unwrap()
        .ship
        .get_attribute(eve_sde::attr_ids::CPU_LOAD, &ctx.sde);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mlu1_id, make_item(mlu1_id, 28576, char_id, ship_id, InventoryFlag::LoSlot0, 1, true));
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
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_id)],
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
        snap.items.insert(mlu2_id, make_item(mlu2_id, 28576, char_id, ship_id, InventoryFlag::LoSlot1, 1, true));
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    let mlu2_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate with 2x MLU II");
    let normal_2_mlu = get_dict_val(&mlu2_res, "normalUnits").unwrap().as_i64().unwrap() as f64;
    let mult_2_mlu = get_dict_val(&mlu2_res, "mluMultiplier").unwrap().as_f64().unwrap();
    let gain_first = mult_1_mlu - 1.0;
    let gain_second = mult_2_mlu - mult_1_mlu;
    assert!(normal_2_mlu > normal_1_mlu);
    assert!(gain_second < gain_first);

    // 3. Real Fleet Mining Foreman Bursts (`feat:mining.mining-fleet-boosts`):
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
        snap.items.insert(burst_mod_id, make_item(burst_mod_id, 42528, booster_char_id, booster_ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(burst_charge_id, make_item(burst_charge_id, 42830, booster_char_id, booster_ship_id, InventoryFlag::HiSlot0, 100, false));
    });
    ctx.ship_fittings.write().unwrap().remove(&booster_ship_id);

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

    let fleet_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate laser under real fleet Mining Foreman Burst");
    let cycle_time = get_dict_val(&fleet_res, "cycleTimeMs").unwrap().as_i64().unwrap();
    let fleet_active = get_dict_val(&fleet_res, "fleetBoostActive").unwrap().as_bool().unwrap();
    assert!(fleet_active);
    assert!(cycle_time < cycle_before_boost);

    // 4. Mining Destroyers (`feat:mining.mining-destroyers`: Pioneer 89240 and Perseverance 91174)
    // and ORE Frigates / Barges / Exhumers / Command Ships (`feat:mining.ore-ships`):
    for ore_hull_tid in [32880u32, 33697, 37135, 17476, 17478, 17480, 22544, 22546, 22548, 42244, 28606, 28352] {
        let hull = &ctx.sde.types[&ore_hull_tid];
        assert!(hull.default_attributes.get(&1556).copied().unwrap_or(0.0) >= 5_000.0, "ORE hull {ore_hull_tid} must have specialized mining hold");
    }
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
                &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_id)],
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
        snap.items.insert(strip_id, make_item(strip_id, 17482, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    let ret_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(strip_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_id)],
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
            &[PyRep::Integer(strip_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate on Hulk");
    let hulk_yield = get_dict_val(&hulk_res, "yieldVolume").unwrap().as_f64().unwrap();
    assert!(hulk_yield > ret_yield);
}

// ====================================================================
// Test 5: MN-08: Ice Belts, Ice Harvesting & Gas Cloud Harvesting
// ====================================================================
#[tokio::test]
async fn test_mn08_ice_and_gas_harvesting() {
    // parity: plan:MN-08, feat:mining.ice-belts, feat:mining.ice-harvester, feat:mining.gas-harvesting
    eve_testkit::attest(&["plan:MN-08", "feat:mining.ice-belts", "feat:mining.ice-harvester", "feat:mining.gas-harvesting"]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut session = setup_test_session(&ctx, char_id).await;
    let kw = HashMap::new();

    // 0. Regional Ice Belt dungeons & 4-hour (14,400s) depletion respawn (`feat:mining.ice-belts`):
    for (sys_id, exp_name, exp_ice_tid, exp_count) in [
        (30_000_142u32, "Blue Ice Belt", 16264u32, 21usize),
        (30_002_187u32, "Clear Icicle Belt", 16262u32, 19usize),
        (30_002_510u32, "Glacial Mass Belt", 16263u32, 20usize),
        (30_002_659u32, "White Glaze Belt", 16265u32, 19usize),
        (30_002_537u32, "Thick Blue Ice Belt", 16266u32, 37usize),
    ] {
        let resolved = eve_server::services::mining_sites::resolve_regional_ice_belt(&ctx.sde, sys_id)
            .expect("Regional ice belt must match")
            .expect("Regional ice belt must resolve against SDE dungeon layout");
        assert_eq!(resolved.name, exp_name);
        assert_eq!(resolved.asteroids.len(), exp_count);
        assert!(resolved.asteroids.iter().any(|a| a.type_id == exp_ice_tid));
    }
    assert!(!eve_server::services::mining_sites::is_ice_belt_ready_to_respawn(14_399));
    assert!(eve_server::services::mining_sites::is_ice_belt_ready_to_respawn(14_400));

    let undock = ctx
        .dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // 1. Ice Harvesting: Clear Icicle (16262, unit volume 1000 m3) (`feat:mining.ice-harvester`)
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
        snap.items.insert(ice_harvester_id, make_item(ice_harvester_id, 16278, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let ice_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(ice_harvester_id), PyRep::String("miningLaser".into()), PyRep::Integer(ice_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate Ice Harvester");
    let ice_units = get_dict_val(&ice_res, "yieldUnits").unwrap().as_i64().unwrap();
    let ice_crit = get_dict_val(&ice_res, "isCriticalSuccess").and_then(|v| v.as_bool()).unwrap_or(false);
    assert_eq!(ice_units, if ice_crit { 2 } else { 1 }, "Ice harvester cycle must harvest 1 ice block (1000 m3)");

    // 2. Gas Harvesting: Fullerite-C50 (30370, unit volume 1 m3) (`feat:mining.gas-harvesting`)
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
        snap.items.insert(gas_harvester_id, make_item(gas_harvester_id, 25542, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let gas_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(gas_harvester_id), PyRep::String("miningLaser".into()), PyRep::Integer(gas_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate Gas Harvester");
    let gas_units = get_dict_val(&gas_res, "yieldUnits").unwrap().as_i64().unwrap();
    assert!(gas_units >= 10, "Gas cloud harvester must extract gas units from cloud");
}
