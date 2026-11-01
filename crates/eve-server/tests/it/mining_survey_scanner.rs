//! Phase 5M survey scanner integration test (`MN-08`), split from `mining_reprocessing.rs`
//! to keep test files under the size limit (helpers duplicated verbatim).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
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
    // parity: feat:modules.survey-scanner, feat:mining.survey-scan, rpc:miningScanMgr.perform_scan, notify:OnAsteroidRevealed
    eve_testkit::attest(&["feat:modules.survey-scanner", "feat:mining.survey-scan", "rpc:miningScanMgr.perform_scan", "notify:OnAsteroidRevealed"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
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

    // Spawn 2 asteroids in system within survey scanner range (15km) and 1 out-of-range asteroid at 25km
    let ast1_id = spawn_test_asteroid(&ctx, 1_000_000_006, sys_id, 1230, 24_000, (ship_pos.0 + 2_000.0, ship_pos.1, ship_pos.2));
    let ast2_id = spawn_test_asteroid(&ctx, 1_000_000_007, sys_id, 1228, 15_000, (ship_pos.0 + 4_000.0, ship_pos.1, ship_pos.2));
    let ast_far_id = spawn_test_asteroid(&ctx, 1_000_000_008, sys_id, 1230, 50_000, (ship_pos.0 + 25_000.0, ship_pos.1, ship_pos.2));

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
    assert!(
        !ast_list.iter().any(|entry| get_dict_val(entry, "asteroidID").and_then(|v| v.as_i64()) == Some(ast_far_id)),
        "Out-of-range asteroid at 25km must be excluded by 15km survey scanner"
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
    // parity: feat:mining.mining-module-target-validity, feat:mining.mercoxit-deep-core-restriction
    eve_testkit::attest(&[
        "feat:mining.mining-module-target-validity",
        "feat:mining.mercoxit-deep-core-restriction",
    ]);
    use crate::mining_cycle_engine::{activate, advance, lock, pilot, rock, rpc, CHAR_ID, VELDSPAR};
    const MINER_I: u32 = 483;
    const STRIP_I: u32 = 17_482;
    const ICE_HARV_I: u32 = 16_278;
    const GAS_SCOOP_I: u32 = 25_266;
    const DEEP_CORE_I: u32 = 12_108;
    const MOD_DEEP_STRIP_II: u32 = 24_305;
    const ZEOLITES: u32 = 45_490;
    const CLEAR_ICICLE: u32 = 16_262;
    const GOLDEN_CYTO: u32 = 25_278;
    const MERCOXIT: u32 = 11_396;
    const MAGMA_MERCOXIT: u32 = 17_869;
    const VITREOUS_MERCOXIT: u32 = 17_870;

    let skills = [
        (3_386, 5),  // Mining
        (3_410, 5),  // Astrogeology
        (3_402, 5),  // Science
        (3_380, 5),  // Industry
        (16_281, 5), // Ice Harvesting
        (25_544, 5), // Gas Cloud Harvesting
        (11_395, 5), // Deep Core Mining
        (17_940, 5), // Mining Barge
        (22_551, 5), // Exhumers
        (22_578, 5), // Mining Upgrades
        (3_429, 5),  // Target Management
        (3_426, 5),  // CPU Management
        (3_413, 5),  // Power Grid Management
        (32_918, 5), // Mining Frigate
    ];
    let (m0, mlu) = (100_000_850_i64, 100_000_851_i64);
    let mut p = pilot(
        32_880,
        &skills,
        &[
            (m0, MINER_I, InventoryFlag::HiSlot0),
            (mlu, 28_576, InventoryFlag::LoSlot0), // Mining Laser Upgrade II (+9% yield)
        ],
    )
    .await;
    let sys = p.ctx.systems.get_or_spawn(crate::mining_cycle_engine::JITA);
    let swap_mod = |p: &crate::mining_cycle_engine::Pilot, ship_tid: u32, mod_tid: u32| {
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&p.ship_id).unwrap().type_id = ship_tid;
            snap.items.get_mut(&m0).unwrap().type_id = mod_tid;
            snap.module_online_states.insert(m0, true);
        });
        p.ctx.invalidate_ship_fitting(p.ship_id);
    };

    for m_tid in [MERCOXIT, MAGMA_MERCOXIT, VITREOUS_MERCOXIT] {
        let t = p.ctx.sde.types.get(&m_tid).unwrap();
        assert_eq!((t.category_id, t.group_id, t.volume), (25, 468, 40.0));
    }

    let (veld, moon, ice, gas, merc, magma, vitr, far_veld) = (
        1_000_000_010_i64,
        1_000_000_011_i64,
        1_000_000_012_i64,
        1_000_000_013_i64,
        1_000_000_014_i64,
        1_000_000_015_i64,
        1_000_000_016_i64,
        1_000_000_017_i64,
    );
    rock(&p, veld, VELDSPAR, 10_000, 2_000.0).await;
    rock(&p, moon, ZEOLITES, 10_000, 2_000.0).await;
    rock(&p, ice, CLEAR_ICICLE, 100, 2_000.0).await;
    rock(&p, gas, GOLDEN_CYTO, 1_000, 1_000.0).await;
    rock(&p, merc, MERCOXIT, 500, 2_000.0).await;
    rock(&p, magma, MAGMA_MERCOXIT, 500, 2_000.0).await;
    rock(&p, vitr, VITREOUS_MERCOXIT, 500, 2_000.0).await;
    rock(&p, far_veld, VELDSPAR, 10_000, 14_000.0).await;

    // 1. Miner I (on Venture) & Strip Miner I (on Retriever) succeed on asteroid and moon ores,
    //    and refuse on ice, gas, and Mercoxit + variants.
    for (ship_tid, mod_tid) in [(32_880_u32, MINER_I), (17_478_u32, STRIP_I)] {
        swap_mod(&p, ship_tid, mod_tid);
        for ok_target in [veld, moon] {
            sys.set_ball_position(p.ship_id, p.ship_pos).await;
            lock(&mut p, ok_target).await;
            sys.set_ball_position(p.ship_id, p.ship_pos).await;
            activate(&mut p, m0, ok_target).await.expect("ore/moon activation must succeed");
            assert!(p.ctx.mining_cycles.read().unwrap().contains_key(&m0));
            rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(m0), PyRep::String("miningLaser".into())]).await.unwrap();
            rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(ok_target)]).await.unwrap();
        }
        for bad_target in [ice, gas, merc, magma, vitr] {
            sys.set_ball_position(p.ship_id, p.ship_pos).await;
            lock(&mut p, bad_target).await;
            sys.set_ball_position(p.ship_id, p.ship_pos).await;
            let err = activate(&mut p, m0, bad_target).await.expect_err("must reject invalid target");
            assert_eq!(err, "TargetNotMinable");
            assert!(!p.ctx.mining_cycles.read().unwrap().contains_key(&m0));
            rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(bad_target)]).await.unwrap();
        }
    }

    // 2. Locked target beyond optimal range fails with TargetOutOfRange and does not start a cycle.
    swap_mod(&p, 32_880, MINER_I);
    sys.set_ball_position(p.ship_id, p.ship_pos).await;
    lock(&mut p, far_veld).await;
    sys.set_ball_position(p.ship_id, p.ship_pos).await;
    let range_err = activate(&mut p, m0, far_veld).await.expect_err("locked target beyond maxRange must fail");
    assert_eq!(range_err, "TargetOutOfRange");
    assert!(!p.ctx.mining_cycles.read().unwrap().contains_key(&m0));
    rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(far_veld)]).await.unwrap();

    // 3. Ice Harvester I activates only on ice types; refuses on asteroid ore, moon ore, gas, and Mercoxit.
    swap_mod(&p, 17_478, ICE_HARV_I);
    sys.set_ball_position(p.ship_id, p.ship_pos).await;
    lock(&mut p, ice).await;
    sys.set_ball_position(p.ship_id, p.ship_pos).await;
    activate(&mut p, m0, ice).await.expect("Ice Harvester on ice must succeed");
    assert!(p.ctx.mining_cycles.read().unwrap().contains_key(&m0));
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(m0), PyRep::String("miningLaser".into())]).await.unwrap();
    rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(ice)]).await.unwrap();
    for bad_target in [veld, moon, gas, merc] {
        sys.set_ball_position(p.ship_id, p.ship_pos).await;
        lock(&mut p, bad_target).await;
        sys.set_ball_position(p.ship_id, p.ship_pos).await;
        let err = activate(&mut p, m0, bad_target).await.expect_err("Ice Harvester on non-ice must fail");
        assert_eq!(err, "TargetNotMinable");
        assert!(!p.ctx.mining_cycles.read().unwrap().contains_key(&m0));
        rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(bad_target)]).await.unwrap();
    }

    // 4. Gas Cloud Scoop I activates only on gas cloud types; refuses on asteroid ore, moon ore, ice, and Mercoxit.
    swap_mod(&p, 32_880, GAS_SCOOP_I);
    sys.set_ball_position(p.ship_id, p.ship_pos).await;
    lock(&mut p, gas).await;
    sys.set_ball_position(p.ship_id, p.ship_pos).await;
    activate(&mut p, m0, gas).await.expect("Gas Cloud Scoop on gas must succeed");
    assert!(p.ctx.mining_cycles.read().unwrap().contains_key(&m0));
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(m0), PyRep::String("miningClouds".into())]).await.unwrap();
    rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(gas)]).await.unwrap();
    for bad_target in [veld, moon, ice, merc] {
        sys.set_ball_position(p.ship_id, p.ship_pos).await;
        lock(&mut p, bad_target).await;
        sys.set_ball_position(p.ship_id, p.ship_pos).await;
        let err = activate(&mut p, m0, bad_target).await.expect_err("Gas scoop on non-gas must fail");
        assert_eq!(err, "TargetNotMinable");
        assert!(!p.ctx.mining_cycles.read().unwrap().contains_key(&m0));
        rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(bad_target)]).await.unwrap();
    }

    // 5. Deep Core Mining Laser I (12108) & Modulated Deep Core Strip Miner II (24305) require Deep Core Mining (11395)
    //    and mine Mercoxit and its variants in whole 40 m3 units.
    sys.set_ball_position(p.ship_id, p.ship_pos).await;
    lock(&mut p, merc).await;
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.skills.get_mut(&CHAR_ID).unwrap().remove(&11_395);
    });
    for (ship_tid, mod_tid) in [(32_880_u32, DEEP_CORE_I), (17_478_u32, MOD_DEEP_STRIP_II)] {
        swap_mod(&p, ship_tid, mod_tid);
        sys.set_ball_position(p.ship_id, p.ship_pos).await;
        let err = activate(&mut p, m0, merc).await.expect_err("Deep core without skill 11395 must fail");
        assert!(matches!(err.as_str(), "ModuleOffline" | "SkillPrerequisitesNotMet"), "got {err}");
        assert!(!p.ctx.mining_cycles.read().unwrap().contains_key(&m0));
    }
    rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(merc)]).await.unwrap();

    p.ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(CHAR_ID).or_default().insert(
            11_395,
            TrainedSkillRecord { type_id: 11_395, level: 5, skill_points: 256_000.0 },
        );
    });
    p.ctx.set_mechanic_rng_seed(Some(7));

    for (ship_tid, mod_tid, target_id, expected_tid) in [
        (32_880_u32, DEEP_CORE_I, merc, MERCOXIT),
        (17_478_u32, MOD_DEEP_STRIP_II, magma, MAGMA_MERCOXIT),
        (17_478_u32, MOD_DEEP_STRIP_II, vitr, VITREOUS_MERCOXIT),
    ] {
        swap_mod(&p, ship_tid, mod_tid);
        sys.set_ball_position(p.ship_id, p.ship_pos).await;
        let max_cap = sys.get_combat_state(p.ship_id).await.unwrap().unwrap().max_capacitor;
        sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
        lock(&mut p, target_id).await;
        sys.set_ball_position(p.ship_id, p.ship_pos).await;
        let act = activate(&mut p, m0, target_id).await.expect("Deep core with skill must activate on Mercoxit variant");
        let dur_s = (act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()).unwrap() as f64 / 1000.0).ceil() as u64;
        let out = advance(&p, dur_s).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ore_type_id, expected_tid);
        assert!(out[0].mining_amount_m3 >= 40.0);
        assert_eq!(out[0].units_removed, (out[0].mining_amount_m3 / 40.0).floor() as i64);
        rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(m0), PyRep::String("miningLaser".into())]).await.unwrap();
        rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(target_id)]).await.unwrap();
    }
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
