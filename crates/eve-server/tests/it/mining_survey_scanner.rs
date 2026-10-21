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
    // parity: feat:modules.survey-scanner, feat:mining.survey-scan, rpc:miningScanMgr.perform_scan, notify:OnAsteroidRevealed, rpc:stationSvc.Undock
    eve_testkit::attest(&["feat:modules.survey-scanner", "feat:mining.survey-scan", "rpc:miningScanMgr.perform_scan", "notify:OnAsteroidRevealed"]);
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

    // 1. Miner I on Ice fails: the ice type's requiredSkill1 (Ice Harvesting) is not a Miner I skill
    let ice_err = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(ore_mod_id), PyRep::String("miningLaser".into()), PyRep::Integer(ice_id)],
        &kw, None,
    ).await.expect_err("Miner I on Ice must fail");
    assert_eq!(ice_err.code, "TargetNotMinable");

    // 2. Gas scoop (miningClouds effect) on Ore fails
    let gas_err = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(gas_mod_id), PyRep::String("gasHarvesting".into()), PyRep::Integer(veld_id)],
        &kw, None,
    ).await.expect_err("Gas scoop on Ore must fail");
    assert_eq!(gas_err.code, "TargetNotMinable");
    // The remaining checks need both turret-fitted miners online: use a Venture (2 turret
    // hardpoints) and take the gas scoop off.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&gas_mod_id);
        snap.items.remove(&100_000_005); // starter weapon sharing HiSlot0 with Miner I
        snap.items.get_mut(&ship_id).unwrap().type_id = 32_880;
    });
    ctx.invalidate_ship_fitting(ship_id);

    // 3. A rock 50 km away was never locked, so activation fails before the range check
    let range_err = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(ore_mod_id), PyRep::String("miningLaser".into()), PyRep::Integer(far_id)],
        &kw, None,
    ).await.expect_err("Unlocked far target must fail");
    assert_eq!(range_err.code, "TargetNotLocked");

    // 4. Ordinary mining laser on Mercoxit fails: Mercoxit requires Deep Core Mining, which Miner I does not
    let merc_err = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "Activate",
        &[PyRep::Integer(ore_mod_id), PyRep::String("miningLaser".into()), PyRep::Integer(merc_id)],
        &kw, None,
    ).await.expect_err("Miner I on Mercoxit must fail");
    assert_eq!(merc_err.code, "TargetNotMinable");

    // 5. Deep Core Mining Laser I cannot run without the Deep Core Mining skill it requires
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sk) = snap.skills.get_mut(&char_id) {
            sk.remove(&11395);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let deep_no_sk = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, deep_mod_id, merc_id).await;
    assert!(deep_no_sk.is_err(), "Deep Core without skill must fail: {deep_no_sk:?}");

    // 6. Deep Core mining laser with Deep Core Mining V succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            11395,
            TrainedSkillRecord { type_id: 11395, level: 5, skill_points: 256_000.0 },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    let deep_res = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, deep_mod_id, merc_id)
        .await
        .expect("Deep Core with skill must succeed on Mercoxit");
    assert_eq!(deep_res.ore_type_id, 11396);
    // Whole Mercoxit units (40 m3 each) only: floor(effective miningAmount / 40).
    assert!(deep_res.mining_amount_m3 > 0.0);
    assert_eq!(deep_res.units_removed, (deep_res.mining_amount_m3 / 40.0).floor() as i64);
}

// ====================================================================
// Test 3: Mining crystals: ore match, volatility damage, shatter, duration multiplier
// ====================================================================
#[tokio::test]
async fn test_mining_crystals_match_ore_and_take_volatility_damage() {
    // parity: feat:mining.modulated-crystals
    eve_testkit::attest(&["feat:mining.modulated-crystals"]);
    use crate::mining_cycle_engine::{activate, advance, attr, lock, pilot, rock, rpc, CHAR_ID, VELDSPAR};
    const MOD_STRIP_II: u32 = 17_912;
    const TYPE_A_I: u32 = 60_276;
    const TYPE_B_I: u32 = 60_279;
    const KERNITE: u32 = 20;
    let (strip, crystal) = (100_000_860_i64, 100_000_862_i64);
    let skills = [(3_386, 5), (3_410, 5), (17_940, 5), (32_918, 5), (3_380, 5), (3_327, 5), (60_377, 3), (3_429, 5)];
    let mut p = pilot(17_478, &skills, &[(strip, MOD_STRIP_II, InventoryFlag::HiSlot0)]).await;
    let load = |p: &crate::mining_cycle_engine::Pilot, type_id: u32, damage: f64| {
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&crystal);
            let mut it = InventoryItemRecord {
                item_id: crystal,
                type_id,
                owner_id: CHAR_ID,
                location_id: p.ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            };
            if damage > 0.0 {
                it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, damage);
            }
            snap.items.insert(crystal, it);
        });
        p.ctx.invalidate_ship_fitting(p.ship_id);
    };
    // A seed whose first three draws are: no crit (>= 0.01), no residue (>= 0.34), then `hit`
    // against the crystal's 2.5% volatility.
    let seed = |hit: bool| {
        (1u64..)
            .find(|&s| {
                let mut st = s;
                let mut d = || eve_server::rng::u64_to_unit_f64(eve_server::rng::splitmix64_next(&mut st));
                d() >= 0.01 && d() >= 0.34 && (d() < 0.025) == hit
            })
            .unwrap()
    };
    let (veld, kern) = (1_000_000_020_i64, 1_000_000_021_i64);
    rock(&p, veld, VELDSPAR, 1_000_000, 3_000.0).await;
    rock(&p, kern, KERNITE, 1_000_000, 3_000.0).await;
    lock(&mut p, veld).await;
    lock(&mut p, kern).await;
    load(&p, TYPE_A_I, 0.0);
    assert_eq!((attr(&p.ctx, TYPE_A_I, 782), attr(&p.ctx, TYPE_A_I, 783), attr(&p.ctx, TYPE_A_I, 784)), (1.5, 0.025, 0.05));
    // Retriever, Mining V, Astrogeology V, Mining Barge V: 120 x 1.25 x 1.25 x 1.15 x 1.10 = 237.1875 m3.
    let base_m3 = 120.0 * 1.25 * 1.25 * 1.15 * 1.10;

    // 1) Matching ore (Veldspar is in the Simple crystal list): x1.5 yield, and a volatility hit
    //    adds 0.05 damage to the crystal.
    activate(&mut p, strip, veld).await.expect("Activate on Veldspar");
    p.ctx.set_mechanic_rng_seed(Some(seed(true)));
    let out = advance(&p, 45).await;
    assert_eq!(out.len(), 1);
    assert!((out[0].mining_amount_m3 - base_m3 * 1.5).abs() < 1e-9, "{}", out[0].mining_amount_m3);
    assert_eq!(out[0].units_removed, 3_557); // floor(355.78125 / 0.1)
    assert_eq!(out[0].crystal.map(|c| (c.type_id, c.matched)), Some((TYPE_A_I, true)));
    assert_eq!((out[0].crystal_damage, out[0].crystal_shattered), (0.05, false));
    let dmg = p.ctx.db.get_item(crystal).unwrap().dynamic_attributes[&eve_sde::attr_ids::DAMAGE];
    assert!((dmg - 0.05).abs() < 1e-12);
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(strip), PyRep::String("miningLaser".into())]).await.unwrap();

    // 2) Non-matching ore (Kernite is not a Simple ore): base modulated yield, no bonus, no penalty.
    activate(&mut p, strip, kern).await.expect("Activate on Kernite");
    p.ctx.set_mechanic_rng_seed(Some(seed(false)));
    let out = advance(&p, 45).await;
    assert!((out[0].mining_amount_m3 - base_m3).abs() < 1e-9, "{}", out[0].mining_amount_m3);
    assert_eq!(out[0].units_removed, 197); // floor(237.1875 / 1.2)
    assert_eq!(out[0].crystal.map(|c| c.matched), Some(false));
    assert_eq!(out[0].crystal_damage, 0.0);
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(strip), PyRep::String("miningLaser".into())]).await.unwrap();

    // 3) A crystal at 0.96 damage shatters on the next volatility hit (structure HP 1.0) and is removed.
    load(&p, TYPE_A_I, 0.96);
    activate(&mut p, strip, veld).await.expect("Activate with worn crystal");
    p.ctx.set_mechanic_rng_seed(Some(seed(true)));
    let out = advance(&p, 45).await;
    assert!(out[0].crystal_shattered);
    assert!(p.ctx.db.get_item(crystal).is_none(), "shattered crystal leaves the slot");
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(strip), PyRep::String("miningLaser".into())]).await.unwrap();

    // 4) Type B I crystal: 0.9x cycle duration (wiki:Mining_crystals) on matching ore only.
    let sys = p.ctx.systems.get_or_spawn(crate::mining_cycle_engine::JITA);
    let max_cap = sys.get_combat_state(p.ship_id).await.unwrap().unwrap().max_capacitor;
    sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
    load(&p, TYPE_B_I, 0.0);
    let act = activate(&mut p, strip, veld).await.expect("Activate with Type B I");
    assert_eq!(act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some(40_500));
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(strip), PyRep::String("miningLaser".into())]).await.unwrap();
    sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
    let act = activate(&mut p, strip, kern).await.expect("Activate Type B I on Kernite");
    assert_eq!(act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some(45_000));
}

// ====================================================================
// Test 4: Reprocessing Curves, Scrapmetal & Prismaticite
// ====================================================================
#[tokio::test]
async fn test_mining_reprocessing_curves_scrapmetal_and_prismaticite() {
    // parity: feat:mining.npc-reprocessing-equipment-and-tax-curve, feat:mining.scrapmetal-yield-exclusions, feat:mining.prismaticite-reprocessing-and-unrefined-reactions, rpc:dogmaIM.Activate, rpc:reprocessingSvc.GetQuotes, rpc:reprocessingSvc.Reprocess, rpc:stationSvc.Undock
    eve_testkit::attest(&["feat:mining.npc-reprocessing-equipment-and-tax-curve", "feat:mining.scrapmetal-yield-exclusions", "feat:mining.prismaticite-reprocessing-and-unrefined-reactions"]);
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
