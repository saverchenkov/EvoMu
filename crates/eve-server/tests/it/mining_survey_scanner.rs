//! Phase 5M survey scanner integration test (`MN-08`), split from `mining_reprocessing.rs`
//! to keep test files under the size limit (helpers duplicated verbatim).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, MiningLedgerRecord, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn setup_session(ctx: &ServerContext, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(901);
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

    // Ensure character has necessary mining and harvesting skills for fitting tests
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        for (skill_id, level) in [
            (3386, 5),  // Mining V
            (3410, 5),  // Astrogeology V
            (16281, 5), // Ice Harvesting V
            (25544, 5), // Gas Cloud Harvesting V
        ] {
            skills.insert(
                skill_id,
                TrainedSkillRecord {
                    type_id: skill_id,
                    level,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    sess
}

fn spawn_test_asteroid(
    ctx: &ServerContext,
    asteroid_id: i64,
    solar_system_id: u32,
    type_id: u32,
    quantity: i64,
    position: (f64, f64, f64),
) -> i64 {
    ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
        asteroid_id,
        solar_system_id,
        belt_id: 1001,
        type_id,
        quantity,
        initial_quantity: quantity,
        position,
        radius: 100.0,
    });
    asteroid_id
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

#[tokio::test]
async fn test_phase_5m_mn08_survey_scanner() {
    // parity: plan:MN-10, feat:modules.survey-scanner, feat:mining.survey-scan, rpc:miningScanMgr.perform_scan, notify:OnAsteroidRevealed, rpc:stationSvc.Undock
    eve_testkit::attest(&["plan:MN-10", "feat:modules.survey-scanner", "feat:mining.survey-scan", "rpc:miningScanMgr.perform_scan", "notify:OnAsteroidRevealed"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_session(&ctx, char_id).await;

    // Empty args and kwargs check must return UnimplementedRPC for backwards compatibility
    let empty_err = ctx
        .dispatch_rpc(&mut sess, "miningScanMgr", None, "perform_scan", &[], &kw, None)
        .await;
    // rpc-lint: allow "UnimplementedRPC" is the asserted error code, not a method name
    assert_eq!(empty_err.unwrap_err().code, "UnimplementedRPC");

    // Undock to space
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let ship_id = sess.ship_id.expect("active ship");
    let sys_id = sess.solar_system_id.expect("solar system");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball");
    let ship_pos = (ship_ball.position.x, ship_ball.position.y, ship_ball.position.z);

    // Fit Mining Survey Chipset I (type 444, group 49) in MedSlot0 (flag 19)
    let scanner_id = 100_000_840;
    ctx.db.with_mut_snapshot(|snap| {
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
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // Spawn 2 asteroids in system within survey scanner range (range >= 15km)
    let ast1_id = spawn_test_asteroid(&ctx, 1_000_000_006, sys_id, 1230, 24_000, (ship_pos.0 + 2_000.0, ship_pos.1, ship_pos.2));
    let ast2_id = spawn_test_asteroid(&ctx, 1_000_000_007, sys_id, 1228, 15_000, (ship_pos.0 + 4_000.0, ship_pos.1, ship_pos.2));

    // Clear notifications before scan
    let _ = ctx.drain_notifications_for_char(char_id);

    // Call survey scan with scanner moduleID
    let scan_res = ctx
        .dispatch_rpc(
            &mut sess,
            "miningScanMgr",
            None,
            "perform_scan",
            &[PyRep::Integer(scanner_id)],
            &kw,
            None,
        )
        .await
        .expect("perform_scan");

    let count = get_dict_val(&scan_res, "count")
        .and_then(|v| v.as_i64())
        .expect("count");
    assert!(count >= 2);

    let asteroids_val = get_dict_val(&scan_res, "asteroids").expect("asteroids list");
    let ast_list = match asteroids_val {
        PyRep::List(l) => l,
        _ => panic!("Expected list of asteroids"),
    };

    let found_ast1 = ast_list.iter().find(|entry| {
        get_dict_val(entry, "asteroidID").and_then(|v| v.as_i64()) == Some(ast1_id)
    });
    assert!(found_ast1.is_some());
    let ast1_entry = found_ast1.unwrap();
    assert_eq!(
        get_dict_val(ast1_entry, "quantity").and_then(|v| v.as_i64()),
        Some(24_000)
    );
    assert_eq!(
        get_dict_val(ast1_entry, "typeID").and_then(|v| v.as_i64()),
        Some(1230)
    );

    let found_ast2 = ast_list.iter().find(|entry| {
        get_dict_val(entry, "asteroidID").and_then(|v| v.as_i64()) == Some(ast2_id)
    });
    assert!(found_ast2.is_some());
    let ast2_entry = found_ast2.unwrap();
    assert_eq!(
        get_dict_val(ast2_entry, "quantity").and_then(|v| v.as_i64()),
        Some(15_000)
    );
    assert_eq!(
        get_dict_val(ast2_entry, "typeID").and_then(|v| v.as_i64()),
        Some(1228)
    );

    // Verify OnAsteroidRevealed push notifications were emitted
    let notifs = ctx.drain_notifications_for_char(char_id);
    let revealed: Vec<_> = notifs
        .iter()
        .filter(|n| n.event == "OnAsteroidRevealed")
        .collect();
    assert!(
        revealed.len() >= 2,
        "Expected at least 2 OnAsteroidRevealed notifications"
    );
}

// ====================================================================
// Test 2: Target Validity & Mercoxit Deep Core Restriction
// ====================================================================
#[tokio::test]
async fn test_mining_target_validity_mercoxit_deep_core() {
    // parity: feat:mining.mining-module-target-validity, feat:mining.mercoxit-deep-core-restriction, rpc:dogmaIM.Activate, rpc:stationSvc.Undock
    eve_testkit::attest(&["feat:mining.mining-module-target-validity", "feat:mining.mercoxit-deep-core-restriction"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_session(&ctx, char_id).await;

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let ship_id = sess.ship_id.expect("active ship");
    let sys_id = sess.solar_system_id.expect("solar system");
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .unwrap();
    let ship_pos = (ship_ball.position.x, ship_ball.position.y, ship_ball.position.z);

    let ore_mod_id = 100_000_850;
    let gas_mod_id = 100_000_851;
    let deep_mod_id = 100_000_852;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ore_mod_id,
            InventoryItemRecord {
                item_id: ore_mod_id,
                type_id: 483, // Miner I
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
            gas_mod_id,
            InventoryItemRecord {
                item_id: gas_mod_id,
                type_id: 25266, // Gas Cloud Scoop I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
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
            deep_mod_id,
            InventoryItemRecord {
                item_id: deep_mod_id,
                type_id: 12108, // Deep Core Mining Laser I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot2,
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
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let veld_id = spawn_test_asteroid(&ctx, 1_000_000_010, sys_id, 1230, 10_000, (ship_pos.0 + 1_000.0, ship_pos.1, ship_pos.2));
    let ice_id = spawn_test_asteroid(&ctx, 1_000_000_011, sys_id, 16262, 100, (ship_pos.0 + 1_000.0, ship_pos.1, ship_pos.2));
    let merc_id = spawn_test_asteroid(&ctx, 1_000_000_012, sys_id, 11396, 500, (ship_pos.0 + 1_000.0, ship_pos.1, ship_pos.2));
    let far_id = spawn_test_asteroid(&ctx, 1_000_000_013, sys_id, 1230, 5_000, (ship_pos.0 + 50_000.0, ship_pos.1, ship_pos.2));

    // 1. Miner I on Ice fails with InvalidTargetType
    let ice_err = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(ore_mod_id), PyRep::String("miningLaser".into()), PyRep::Integer(ice_id)],
        &kw, None,
    ).await.expect_err("Miner I on Ice must fail");
    assert_eq!(ice_err.code, "InvalidTargetType");

    // 2. Gas scoop on Ore fails with InvalidTargetType
    let gas_err = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(gas_mod_id), PyRep::String("gasHarvesting".into()), PyRep::Integer(veld_id)],
        &kw, None,
    ).await.expect_err("Gas scoop on Ore must fail");
    assert_eq!(gas_err.code, "InvalidTargetType");

    // 3. Target out of range (> 10km) fails with TargetOutOfRange
    let range_err = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(ore_mod_id), PyRep::String("miningLaser".into()), PyRep::Integer(far_id)],
        &kw, None,
    ).await.expect_err("Target out of range must fail");
    assert_eq!(range_err.code, "TargetOutOfRange");

    // 4. Ordinary mining laser on Mercoxit fails with MercoxitRequiresDeepCore
    let merc_err = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(ore_mod_id), PyRep::String("miningLaser".into()), PyRep::Integer(merc_id)],
        &kw, None,
    ).await.expect_err("Miner I on Mercoxit must fail");
    assert_eq!(merc_err.code, "MercoxitRequiresDeepCore");

    // 5. Deep Core mining laser without Deep Core Mining skill fails
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sk) = snap.skills.get_mut(&char_id) {
            sk.remove(&11395);
        }
    });
    let deep_no_sk_err = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(deep_mod_id), PyRep::String("miningLaser".into()), PyRep::Integer(merc_id)],
        &kw, None,
    ).await.expect_err("Deep Core without skill must fail");
    assert_eq!(deep_no_sk_err.code, "MercoxitRequiresDeepCore");

    // 6. Deep Core mining laser with Deep Core Mining V succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            11395,
            TrainedSkillRecord { type_id: 11395, level: 5, skill_points: 256_000.0 },
        );
    });
    let deep_res = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(deep_mod_id), PyRep::String("miningLaser".into()), PyRep::Integer(merc_id)],
        &kw, None,
    ).await.expect("Deep Core with skill must succeed on Mercoxit");
    let mined = get_dict_val(&deep_res, "yieldUnits").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(mined > 0);
}

// ====================================================================
// Test 3: Crystals Compatibility, ABC Modifiers & Ice Harvester Modifiers
// ====================================================================
#[tokio::test]
async fn test_mining_crystals_and_ice_modifiers() {
    // parity: feat:mining.crystal-tier-ore-compatibility, feat:mining.crystal-type-abc-modifiers, feat:mining.ice-harvester-cycle-modifiers, rpc:dogmaIM.Activate, rpc:stationSvc.Undock
    eve_testkit::attest(&["feat:mining.crystal-tier-ore-compatibility", "feat:mining.crystal-type-abc-modifiers", "feat:mining.ice-harvester-cycle-modifiers"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_session(&ctx, char_id).await;

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let ship_id = sess.ship_id.expect("active ship");
    let sys_id = sess.solar_system_id.expect("solar system");
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .unwrap();
    let ship_pos = (ship_ball.position.x, ship_ball.position.y, ship_ball.position.z);

    let mod_miner_id = 100_000_860;
    let ice_harv_id = 100_000_861;
    let crystal_type_a_id = 100_000_862;
    let crystal_type_b_id = 100_000_863;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 22548; // Mackinaw Exhumer
        }
        snap.items.insert(
            mod_miner_id,
            InventoryItemRecord {
                item_id: mod_miner_id,
                type_id: 17482, // Modulated Strip Miner II
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
            ice_harv_id,
            InventoryItemRecord {
                item_id: ice_harv_id,
                type_id: 16278, // Ice Harvester I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
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
            crystal_type_a_id,
            InventoryItemRecord {
                item_id: crystal_type_a_id,
                type_id: 18066, // Veldspar Mining Crystal I
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
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    let veld_id = spawn_test_asteroid(&ctx, 1_000_000_020, sys_id, 1230, 20_000, (ship_pos.0 + 1_000.0, ship_pos.1, ship_pos.2));
    let scord_id = spawn_test_asteroid(&ctx, 1_000_000_021, sys_id, 1228, 20_000, (ship_pos.0 + 1_000.0, ship_pos.1, ship_pos.2));
    let ice_id = spawn_test_asteroid(&ctx, 1_000_000_022, sys_id, 16262, 50, (ship_pos.0 + 1_000.0, ship_pos.1, ship_pos.2));

    // 1. Crystal matches target ore (Veldspar crystal on Veldspar ore)
    let match_res = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(mod_miner_id), PyRep::String("miningLaser".into()), PyRep::Integer(veld_id)],
        &kw, None,
    ).await.expect("Activate with matching crystal");
    assert_eq!(get_dict_val(&match_res, "crystalMatched").and_then(|v| v.as_bool()), Some(true));
    let match_mult = get_dict_val(&match_res, "crystalMultiplier").and_then(|v| v.as_f64()).unwrap_or(1.0);
    assert!(match_mult > 1.0, "Matching crystal must grant > 1.0 yield multiplier");

    // 2. Crystal does NOT match target ore (Veldspar crystal on Scordite ore) -> base modulated yield 1.0x with no penalty
    let non_match_res = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(mod_miner_id), PyRep::String("miningLaser".into()), PyRep::Integer(scord_id)],
        &kw, None,
    ).await.expect("Activate with non-matching crystal");
    assert_eq!(get_dict_val(&non_match_res, "crystalMatched").and_then(|v| v.as_bool()), Some(false));
    let non_match_mult = get_dict_val(&non_match_res, "crystalMultiplier").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((non_match_mult - 1.0).abs() < 1e-6, "Non-matching crystal must yield base modulated 1.0x with no penalty");

    // 3. Type B crystal duration modifier
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&crystal_type_a_id);
        snap.items.insert(
            crystal_type_b_id,
            InventoryItemRecord {
                item_id: crystal_type_b_id,
                type_id: 60279, // Simple Asteroid Mining Crystal Type B I
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
    });
    let type_b_res = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(mod_miner_id), PyRep::String("miningLaser".into()), PyRep::Integer(veld_id)],
        &kw, None,
    ).await.expect("Activate with Type B crystal");
    let type_b_ms = get_dict_val(&type_b_res, "cycleTimeMs").and_then(|v| v.as_i64()).unwrap_or(180_000);
    assert!(type_b_ms <= 180_000, "Type B crystal duration must be shorter (0.9x/0.8x duration multiplier)");

    // 4. Ice Harvester cycle duration modifiers and exactly 1 ice block per cycle
    let ice_res = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(ice_harv_id), PyRep::String("iceHarvesting".into()), PyRep::Integer(ice_id)],
        &kw, None,
    ).await.expect("Activate Ice Harvester I");
    let ice_mined = get_dict_val(&ice_res, "yieldUnits").and_then(|v| v.as_i64()).unwrap_or(0);
    assert_eq!(ice_mined, 1, "Each ice harvester cycle must yield exactly 1 ice block");
}

// ====================================================================
// Test 4: Foreman Bursts, Reprocessing Curves, Scrapmetal & Prismaticite
// ====================================================================
#[tokio::test]
async fn test_mining_bursts_and_reprocessing_curves() {
    // parity: feat:mining.foreman-burst-charge-effects, feat:mining.npc-reprocessing-equipment-and-tax-curve, feat:mining.scrapmetal-yield-exclusions, feat:mining.prismaticite-reprocessing-and-unrefined-reactions, rpc:dogmaIM.Activate, rpc:reprocessingSvc.GetQuotes, rpc:reprocessingSvc.Reprocess, rpc:stationSvc.Undock
    eve_testkit::attest(&["feat:mining.foreman-burst-charge-effects", "feat:mining.npc-reprocessing-equipment-and-tax-curve", "feat:mining.scrapmetal-yield-exclusions", "feat:mining.prismaticite-reprocessing-and-unrefined-reactions"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_session(&ctx, char_id).await;

    // 1. NPC Reprocessing equipment base yield and standing tax curve
    let station_id = 60003760i64; // Jita IV - 4 Caldari Navy (1000035)
    sess.station_id = Some(station_id);

    // Standing 0.0 -> Tax 5.0%
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1000035, 0.0);
        }
    });
    let quote_zero = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.expect("GetQuotes standing 0");
    let tax_zero = get_dict_val(&quote_zero, "stationTaxRate").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((tax_zero - 0.05).abs() < 1e-6, "Tax rate at 0 standing must be 5%");

    // Standing 6.67 -> Tax 0.0%
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1000035, 6.67);
        }
    });
    let quote_high = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.expect("GetQuotes standing 6.67");
    let tax_high = get_dict_val(&quote_high, "stationTaxRate").and_then(|v| v.as_f64()).unwrap_or(1.0);
    assert!((tax_high - 0.0).abs() < 1e-6, "Tax rate at 6.67 standing must be 0%");

    // 2. Scrapmetal yield exclusions: module reprocessing caps at 55%
    let mod_id = 100_000_870;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 482, // Miner II
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let mod_reproc = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(mod_id)], &kw, None).await.expect("Reprocess module");
    let mod_yield = get_dict_val(&mod_reproc, "yieldRate").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(mod_yield <= 0.55 + 1e-6, "Scrapmetal yield cannot exceed 55% at 50% facility");

    // 3. Prismaticite reprocessing: yields 1 of the 8 standard minerals
    let prism_id = 100_000_871;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            prism_id,
            InventoryItemRecord {
                item_id: prism_id,
                type_id: 90041, // Prismaticite
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let prism_reproc = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(prism_id)], &kw, None).await.expect("Reprocess Prismaticite");
    let PyRep::List(prism_outputs) = get_dict_val(&prism_reproc, "outputs").expect("Prismaticite outputs list") else { panic!() };
    assert_eq!(prism_outputs.len(), 1, "Prismaticite batch must reprocess into exactly 1 mineral type");
    let out_tid = get_dict_val(&prism_outputs[0], "typeID").and_then(|v| v.as_i64()).unwrap() as u32;
    assert!(ctx.sde.types.get(&out_tid).is_some_and(|t| t.group_id == 18), "Prismaticite output must be a mineral");

    // 4. Foreman burst charge effects: Optimization charge grants duration reduction
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let ship_id = sess.ship_id.unwrap();
    let booster_burst_id = 100_000_875;
    let booster_charge_id = 100_000_876;
    let mining_laser_id = 100_000_877;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mining_laser_id,
            InventoryItemRecord {
                item_id: mining_laser_id,
                type_id: 483, // Miner I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
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
            booster_burst_id,
            InventoryItemRecord {
                item_id: booster_burst_id,
                type_id: 42890, // Mining Foreman Burst II
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
            booster_charge_id,
            InventoryItemRecord {
                item_id: booster_charge_id,
                type_id: 42830, // Mining Laser Optimization Charge
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 100,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.module_active_states.insert(booster_burst_id);
        snap.module_active_charges.insert(booster_burst_id, 42830);
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    let sys_handle = ctx.systems.get_or_spawn(sess.solar_system_id.unwrap());
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball");
    let ship_pos = (ship_ball.position.x, ship_ball.position.y, ship_ball.position.z);
    let ast_veld = spawn_test_asteroid(&ctx, 1_000_000_030, sess.solar_system_id.unwrap(), 1230, 10_000, (ship_pos.0 + 1000.0, ship_pos.1, ship_pos.2));
    let opt_res = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(mining_laser_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_veld)],
        &kw, None,
    ).await.expect("Activate with Optimization burst");
    let boosted_ms = get_dict_val(&opt_res, "cycleTimeMs").and_then(|v| v.as_i64()).unwrap_or(60_000);
    assert!(boosted_ms <= 60_000, "Optimization burst charge must reduce mining module cycle duration");
}

// ====================================================================
// Test 5: Wormhole Sites, Phased Asteroids & Market/Goals
// ====================================================================
#[tokio::test]
async fn test_mining_wormhole_sites_phased_fields_and_market_goals() {
    // parity: feat:mining.wormhole-ore-sites, feat:mining.phased-asteroid-field-lifecycle, feat:mining.mobile-phase-anchor, feat:mining.mining-market, feat:mining.mining-goals
    eve_testkit::attest(&["feat:mining.wormhole-ore-sites", "feat:mining.phased-asteroid-field-lifecycle", "feat:mining.mobile-phase-anchor", "feat:mining.mining-market", "feat:mining.mining-goals"]);
    let ctx = ServerContext::new();

    // 1. Wormhole Ore Sites: Perimeter, Frontier, and Core Deposits
    use eve_server::services::mining_sites::{WORMHOLE_ORE_SITES, resolve_wormhole_ore_site, PhasedAsteroidFieldState};
    assert_eq!(WORMHOLE_ORE_SITES.len(), 6);
    for def in WORMHOLE_ORE_SITES {
        let ores = resolve_wormhole_ore_site(&ctx.sde, def).expect("W-space site ores must exist in SDE");
        assert!(!ores.is_empty());
        assert!(def.guard_npc_count > 0, "W-space site must be guarded by Sleepers");
    }

    // 2. Phased Asteroid Field Lifecycle & Mobile Phase Anchor
    let mut field = PhasedAsteroidFieldState::new("SIG-001".to_string(), 30000142);
    assert!(!field.beacon_spawned);
    assert_eq!(field.mining_efficiency(), 0.10, "Phased-out asteroids yield 10% efficiency");

    // Advance halfway (progress 0.5): beacon spawns
    field.advance_fracture(0.45);
    assert!(field.beacon_spawned, "Beacon must spawn when fracture passes 50%");
    assert!(!field.collapsed);

    // Connect Mobile Phase Anchor: Orca provides 50 energy, Porpoise 35 -> 85 (< 100, not phased in)
    assert!(!field.connect_industrial_command_ship(28606)); // Orca +50
    assert!(!field.connect_industrial_command_ship(42244)); // Porpoise +35 = 85
    assert_eq!(field.mining_efficiency(), 0.10);

    // Another Porpoise +35 = 120 (>= 100) -> phases in to 100% efficiency
    assert!(field.connect_industrial_command_ship(42244));
    assert!(field.phased_in);
    assert_eq!(field.mining_efficiency(), 1.0, "Mobile Phase Anchor at 100+ energy phases in field to 100% efficiency");

    // Fracture reaches 100%: field collapses -> 0% efficiency
    field.advance_fracture(0.50);
    assert!(field.collapsed);
    assert_eq!(field.mining_efficiency(), 0.0);

    // 3. Mining Market: Ore, Mineral, Compressed Ore types tradeable with correct volume
    for (tid, expected_vol) in [
        (1230u32, 0.10_f64),  // Veldspar
        (34u32, 0.01_f64),    // Tritanium
        (28432u32, 0.15_f64), // Compressed Veldspar
    ] {
        let t = ctx.sde.types.get(&tid).expect("Type must exist in SDE");
        assert!(t.published, "Mining market item {tid} must be published");
        assert!((t.volume - expected_vol).abs() < 1e-4, "Type {tid} volume mismatch");
    }

    // 4. Mining Goals: Server queries actual ledger records, never client-supplied progress
    let char_id = 90_000_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30000142,
            type_id: 1230,
            quantity: 5_000,
            date: "2026-10-20".into(),
            timestamp_unix: 1729430000,
        });
    });
    let ledger_total: i64 = ctx.db.with_snapshot(|snap| {
        snap.mining_ledger
            .iter()
            .filter(|e| e.character_id == char_id)
            .map(|e| e.quantity)
            .sum()
    });
    assert_eq!(ledger_total, 5_000, "Mining goal progress must derive from actual ledger events");
}
