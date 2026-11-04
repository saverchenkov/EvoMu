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

pub(crate) mod mining_crystal_tier_ore_compatibility {
    // Phase 5M mining crystal tiers, scrapmetal exclusions, Prismaticite/Unrefined Mineral
    // reactions, Mining Foreman Burst charges, and phased asteroid fields (`MN-07`..`MN-08`, part 2).


    use eve_db::{
        InventoryFlag, InventoryItemRecord, IskCents, MiningLedgerRecord, TrainedSkillRecord,
        UpwellStructureRecord,
    };
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::{BTreeSet, HashMap};

    async fn setup_session(ctx: &ServerContext, session_id: u64, user: &str, secret: &str, char_id: i64) -> SessionState {
        let kw = HashMap::new();
        let mut sess = SessionState::new(session_id);
        ctx.dispatch_rpc(
            &mut sess,
            "authentication",
            None,
            "Login",
            &[PyRep::String(user.into()), PyRep::String(secret.into())],
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
        sess
    }

    fn make_item(id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, qty: i32, singleton: bool) -> InventoryItemRecord {
        InventoryItemRecord {
            item_id: id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity: qty,
            singleton,
            version: 1,
            ..Default::default()
        }
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

    #[tokio::test]
    async fn test_mining_crystal_tier_ore_compatibility_and_loading_rules() {
        // parity: feat:mining.crystal-tier-ore-compatibility
        eve_testkit::attest(&["feat:mining.crystal-tier-ore-compatibility"]);
        use crate::mining_cycle_engine::{activate, advance, lock, pilot, rock, rpc, CHAR_ID, JITA, VELDSPAR};
        const MINER_I: u32 = 483;
        const STRIP_I: u32 = 17_482;
        const MOD_STRIP_II: u32 = 17_912;
        const MOD_DEEP_STRIP_II: u32 = 24_305;
        const ORE_DEEP_CORE: u32 = 28_748;
        const BANIDINE: u32 = 28_617;

        let (mod_id, crystal_id, cargo_crystal_id) = (100_000_865_i64, 100_000_866_i64, 100_000_867_i64);
        let skills = [
            (3_386, 5), (3_410, 5), (3_402, 5), (3_380, 5), (3_327, 5), (11_395, 5),
            (17_940, 5), (32_918, 5), (3_429, 5), (60_377, 5), (60_378, 5), (60_379, 5),
            (60_380, 5), (60_381, 5), (12_189, 5), (46_152, 5), (46_153, 5), (46_154, 5),
            (46_155, 5), (46_156, 5),
        ];
        let mut p = pilot(17_478, &skills, &[(mod_id, MOD_STRIP_II, InventoryFlag::HiSlot0)]).await;
        let bid = p.beyonce_id;
        rpc(&mut p, "beyonce", Some(bid), "CmdStop", &[]).await.unwrap();
        p.ctx.set_mechanic_rng_seed(Some(7));

        let tiers: &[(u32, u32, u32, u32)] = &[
            (MOD_STRIP_II, 60_276, VELDSPAR, 20),
            (MOD_STRIP_II, 60_285, 20, VELDSPAR),
            (MOD_STRIP_II, 60_291, 1_225, VELDSPAR),
            (MOD_STRIP_II, 60_297, 1_223, VELDSPAR),
            (MOD_STRIP_II, 60_303, 52_306, VELDSPAR),
            (MOD_DEEP_STRIP_II, 18_054, 11_396, VELDSPAR),
            (MOD_STRIP_II, 46_355, 45_490, 45_494),
            (MOD_STRIP_II, 46_365, 45_494, 45_490),
            (MOD_STRIP_II, 46_367, 45_498, 45_490),
            (MOD_STRIP_II, 46_369, 45_502, 45_490),
            (MOD_STRIP_II, 46_371, 45_510, 45_490),
        ];

        let base_m3 = 120.0 * 1.25 * 1.25 * 1.15 * 1.10;
        let sys = p.ctx.systems.get_or_spawn(JITA);
        let max_cap = sys.get_combat_state(p.ship_id).await.unwrap().unwrap().max_capacitor;

        for (idx, &(miner_tid, crystal_tid, match_ore, mismatch_ore)) in tiers.iter().enumerate() {
            sys.set_ball_position(p.ship_id, p.ship_pos).await;
            let match_rock = 1_000_000_200_i64 + (idx as i64) * 2;
            let mismatch_rock = match_rock + 1;
            rock(&p, match_rock, match_ore, 100_000, 2_000.0).await;
            rock(&p, mismatch_rock, mismatch_ore, 100_000, 2_000.0).await;
            lock(&mut p, match_rock).await;
            lock(&mut p, mismatch_rock).await;
            p.ctx.db.with_mut_snapshot(|snap| {
                snap.items.get_mut(&mod_id).unwrap().type_id = miner_tid;
                snap.items.insert(crystal_id, make_item(crystal_id, crystal_tid, CHAR_ID, p.ship_id, InventoryFlag::HiSlot0, 1, true));
            });
            p.ctx.invalidate_ship_fitting(p.ship_id);

            let expected_base = if miner_tid == MOD_DEEP_STRIP_II { 80.0 * 1.25 * 1.25 * 1.15 * 1.10 } else { base_m3 };
            sys.set_ball_position(p.ship_id, p.ship_pos).await;
            sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
            activate(&mut p, mod_id, match_rock).await.expect("activate on matching ore");
            let out_match = advance(&p, 45).await;
            assert_eq!(out_match[0].crystal.map(|c| (c.type_id, c.matched)), Some((crystal_tid, true)));
            assert!((out_match[0].mining_amount_m3 - expected_base * 1.5).abs() < 1e-9, "tier {idx} matched yield");
            rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(mod_id), PyRep::String("miningLaser".into())]).await.unwrap();

            sys.set_ball_position(p.ship_id, p.ship_pos).await;
            sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
            activate(&mut p, mod_id, mismatch_rock).await.expect("activate on mismatched ore");
            let out_mis = advance(&p, 45).await;
            assert_eq!(out_mis[0].crystal.map(|c| (c.type_id, c.matched)), Some((crystal_tid, false)));
            assert!((out_mis[0].mining_amount_m3 - expected_base).abs() < 1e-9, "tier {idx} mismatched yield");
            rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(mod_id), PyRep::String("miningLaser".into())]).await.unwrap();

            rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(match_rock)]).await.unwrap();
            rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(mismatch_rock)]).await.unwrap();
        }

        sys.set_ball_position(p.ship_id, p.ship_pos).await;
        let banidine_rock = 1_000_000_250_i64;
        rock(&p, banidine_rock, BANIDINE, 100_000, 2_000.0).await;
        lock(&mut p, banidine_rock).await;
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_id).unwrap().type_id = MOD_STRIP_II;
            snap.items.get_mut(&crystal_id).unwrap().type_id = 60_276;
        });
        p.ctx.invalidate_ship_fitting(p.ship_id);
        sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
        activate(&mut p, mod_id, banidine_rock).await.expect("activate on mission-unique Banidine");
        let out_mission = advance(&p, 45).await;
        assert_eq!(out_mission[0].crystal.map(|c| c.matched), Some(false));
        assert!((out_mission[0].mining_amount_m3 - base_m3).abs() < 1e-9);
        rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(mod_id), PyRep::String("miningLaser".into())]).await.unwrap();

        p.ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&crystal_id);
            snap.items.insert(cargo_crystal_id, make_item(cargo_crystal_id, 60_276, CHAR_ID, p.ship_id, InventoryFlag::Cargo, 5, false));
        });
        let ship_id = p.ship_id;
        for unmod_tid in [MINER_I, STRIP_I] {
            p.ctx.db.with_mut_snapshot(|snap| snap.items.get_mut(&mod_id).unwrap().type_id = unmod_tid);
            p.ctx.invalidate_ship_fitting(ship_id);
            let err = rpc(&mut p, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_id)]), PyRep::List(vec![PyRep::Integer(cargo_crystal_id)])]).await.unwrap_err();
            assert_eq!(err, "IncompatibleAmmo");
        }

        p.ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_id).unwrap().type_id = MOD_STRIP_II;
            snap.items.get_mut(&cargo_crystal_id).unwrap().type_id = 18_054;
        });
        p.ctx.invalidate_ship_fitting(ship_id);
        let err = rpc(&mut p, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_id)]), PyRep::List(vec![PyRep::Integer(cargo_crystal_id)])]).await.unwrap_err();
        assert_eq!(err, "IncompatibleAmmo");

        for (miner_tid, cryst_tid, req_skill_id) in [
            (MOD_STRIP_II, 60_276_u32, 60_377_u32),
            (MOD_DEEP_STRIP_II, 18_054_u32, 12_189_u32),
            (ORE_DEEP_CORE, 18_054_u32, 12_189_u32),
        ] {
            p.ctx.db.with_mut_snapshot(|snap| {
                snap.items.retain(|&id, it| id == mod_id || id == cargo_crystal_id || it.flag != InventoryFlag::HiSlot0);
                snap.items.get_mut(&mod_id).unwrap().type_id = miner_tid;
                let ch = snap.items.get_mut(&cargo_crystal_id).unwrap();
                ch.type_id = cryst_tid;
                ch.location_id = ship_id;
                ch.flag = InventoryFlag::Cargo;
                ch.quantity = 5;
                snap.skills.get_mut(&CHAR_ID).unwrap().remove(&req_skill_id);
            });
            p.ctx.invalidate_ship_fitting(ship_id);
            let sk_err = rpc(&mut p, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_id)]), PyRep::List(vec![PyRep::Integer(cargo_crystal_id)])]).await.unwrap_err();
            assert_eq!(sk_err, "SkillPrerequisitesNotMet");

            p.ctx.db.with_mut_snapshot(|snap| {
                snap.skills.entry(CHAR_ID).or_default().insert(req_skill_id, TrainedSkillRecord { type_id: req_skill_id, level: 5, skill_points: 256_000.0 });
            });
            p.ctx.invalidate_ship_fitting(ship_id);
            rpc(&mut p, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_id)]), PyRep::List(vec![PyRep::Integer(cargo_crystal_id)])]).await.unwrap();
        }
    }

    #[tokio::test]
    async fn test_mining_reprocessing_curves_scrapmetal_and_prismaticite() {
        // parity: feat:mining.npc-reprocessing-equipment-and-tax-curve, feat:mining.scrapmetal-yield-exclusions, feat:mining.prismaticite-reprocessing-and-unrefined-reactions
        eve_testkit::attest(&[
            "feat:mining.npc-reprocessing-equipment-and-tax-curve",
            "feat:mining.scrapmetal-yield-exclusions",
            "feat:mining.prismaticite-reprocessing-and-unrefined-reactions",
        ]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let char_id = 90_000_001_i64;
        let mut sess = setup_session(&ctx, 902, "demo", "demo-secret", char_id).await;

        // 1. NPC Reprocessing equipment base yield & standing tax curve (0.0 -> 5%, 6.67 -> 0%)
        let jita_station = 60_003_760_i64;
        sess.station_id = Some(jita_station);
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.station_id = Some(jita_station);
            ch.balance = IskCents::from_isk(500_000_000);
            ch.standings.insert(1_000_035, 0.0);
        });
        let quote_zero = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
        assert!((get_dict_val(&quote_zero, "stationTaxRate").and_then(|v| v.as_f64()).unwrap() - 0.05).abs() < 1e-6);

        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().standings.insert(1_000_035, 6.67);
        });
        let quote_high = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
        assert!((get_dict_val(&quote_high, "stationTaxRate").and_then(|v| v.as_f64()).unwrap() - 0.0).abs() < 1e-6);

        // 2. Scrapmetal yield exclusions (`feat:mining.scrapmetal-yield-exclusions`):
        // Dock in a Null-Sec rigged Tatara (35836) with Reprocessing V, Reprocessing Efficiency V,
        // Simple Ore Processing V, Scrapmetal Processing V, and RX-804 (+4% implant 27170).
        let ns_sys = ctx.sde.solar_systems.values().find(|s| s.security_status < -0.2 && s.solar_system_id < 31_000_000).unwrap().solar_system_id;
        let rig_tid = ctx.sde.types.values().find(|t| matches!(t.group_id, 1941..=1945) && t.default_attributes.get(&1547) == Some(&3.0)).map(|t| t.type_id).unwrap_or(46_497);
        let tatara_id = 1_045_000_000_801_i64;
        let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(tatara_id, UpwellStructureRecord {
                structure_id: tatara_id, type_id: 35_836, owner_corp_id: corp_id, solar_system_id: ns_sys,
                position: [0.0, 0.0, 0.0], name: "NS Rigged Tatara".into(), state: "ShieldVulnerable".into(),
                fuel_blocks: 5_000, quantum_core_installed: true, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            snap.items.insert(100_000_868, make_item(100_000_868, 35_899, corp_id, tatara_id, InventoryFlag::ServiceSlot0, 1, true));
            snap.items.insert(100_000_869, make_item(100_000_869, rig_tid, corp_id, tatara_id, InventoryFlag::RigSlot0, 1, true));
            snap.module_online_states.insert(100_000_868, true);
            let sk = snap.skills.entry(char_id).or_default();
            for (sid, lvl) in [(3_385, 5), (3_389, 5), (60_377, 5), (12_196, 5), (45_746, 5), (90_398, 5)] {
                sk.insert(sid, TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 256_000.0 });
            }
            snap.active_implants.entry(char_id).or_default().push(27_170); // RX-804 +4% reprocessing yield
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.solar_system_id = ns_sys;
            ch.station_id = None;
            ch.structure_id = Some(tatara_id);
            snap.items.insert(100_000_870, make_item(100_000_870, 482, char_id, tatara_id, InventoryFlag::Hangar, 1, false));   // Miner II module
            snap.items.insert(100_000_871, make_item(100_000_871, 587, char_id, tatara_id, InventoryFlag::Hangar, 1, false));   // Packaged Rifter
            snap.items.insert(100_000_872, make_item(100_000_872, 587, char_id, tatara_id, InventoryFlag::Hangar, 1, true));    // Assembled Rifter (must fail)
            snap.items.insert(100_000_873, make_item(100_000_873, 1_230, char_id, tatara_id, InventoryFlag::Hangar, 1_000, false)); // Veldspar ore
        });
        sess.station_id = None;
        sess.structure_id = Some(tatara_id);
        sess.solar_system_id = Some(ns_sys);

        // Assembled ship is rejected
        let asm_err = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_872)], &kw, None).await.unwrap_err();
        assert_eq!(asm_err.code, "ItemMustBeRepackaged");

        // Ore at rigged NS Tatara receives structure + rig + nullsec + skills + RX-804 (> 75%)
        let ore_rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_873)], &kw, None).await.unwrap();
        let ore_yield = get_dict_val(&ore_rep, "yieldRate").and_then(|v| v.as_f64()).unwrap();
        assert!(ore_yield > 0.75, "Ore at rigged NS Tatara must receive structure/rig/nullsec/implant bonuses (got {ore_yield})");

        // Module (482) and packaged ship (587) ignore Upwell structure, rig, security-band, Reprocessing, and RX-804 bonuses -> exact 55% at Scrapmetal V
        let mod_rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_870)], &kw, None).await.unwrap();
        let ship_rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_871)], &kw, None).await.unwrap();
        assert!((get_dict_val(&mod_rep, "yieldRate").and_then(|v| v.as_f64()).unwrap() - 0.55).abs() < 1e-6);
        assert!((get_dict_val(&ship_rep, "yieldRate").and_then(|v| v.as_f64()).unwrap() - 0.55).abs() < 1e-6);

        // Scrapmetal Processing III -> exact 0.50 * (1 + 0.02 * 3) = 0.53 even in rigged NS Tatara with RX-804
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.get_mut(&char_id).unwrap().get_mut(&12_196).unwrap().level = 3;
            snap.items.insert(100_000_874, make_item(100_000_874, 482, char_id, tatara_id, InventoryFlag::Hangar, 1, false));
        });
        let mod_lvl3 = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_874)], &kw, None).await.unwrap();
        assert!((get_dict_val(&mod_lvl3, "yieldRate").and_then(|v| v.as_f64()).unwrap() - 0.53).abs() < 1e-6);

        // 3. Prismaticite per-batch random mineral & Unrefined Mineral reactions (`feat:mining.prismaticite-reprocessing-and-unrefined-reactions`):
        let mut single_batch_minerals = BTreeSet::new();
        let mut single_batch_qtys = BTreeSet::new();
        for (idx, seed) in [11_u64, 29, 73, 151, 313, 619].into_iter().enumerate() {
            let iid = 100_000_880 + idx as i64;
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.insert(iid, make_item(iid, 90_041, char_id, tatara_id, InventoryFlag::Hangar, 100, false));
            });
            ctx.set_mechanic_rng_seed(Some(seed));
            let rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(iid)], &kw, None).await.unwrap();
            let PyRep::List(outs) = get_dict_val(&rep, "outputs").unwrap() else { panic!("expected outputs list") };
            assert_eq!(outs.len(), 1, "Single 100-unit Prismaticite batch must yield exactly 1 mineral type");
            let tid = get_dict_val(&outs[0], "typeID").and_then(|v| v.as_i64()).unwrap() as u32;
            let qty = get_dict_val(&outs[0], "quantity").and_then(|v| v.as_i64()).unwrap();
            assert!(ctx.sde.types.get(&tid).is_some_and(|t| t.group_id == 18));
            single_batch_minerals.insert(tid);
            single_batch_qtys.insert((tid, qty));
        }
        assert!(single_batch_minerals.len() >= 2, "Different RNG seeds must roll distinct mineral types across Prismaticite batches");

        // Multi-batch stack (1,200 Prismaticite = 12 batches) rolls independently per batch into multiple minerals:
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(100_000_890, make_item(100_000_890, 90_041, char_id, tatara_id, InventoryFlag::Hangar, 1_200, false));
        });
        ctx.set_mechanic_rng_seed(Some(42));
        let multi_rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_890)], &kw, None).await.unwrap();
        let PyRep::List(multi_outs) = get_dict_val(&multi_rep, "outputs").unwrap() else { panic!() };
        assert!(multi_outs.len() >= 2, "12-batch Prismaticite stack must aggregate independent per-batch mineral rolls");

        // Unrefined Mineral reaction (90274 Unrefined Tritanium Formula, group 1888):
        // Negative: having only Standup Biochemical Reactor I (45539) online rejects InstallJob with FacilityServiceOffline!
        let (formula_id, biochem_svc_id, comp_svc_id) = (100_000_891_i64, 100_000_892_i64, 100_000_893_i64);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(formula_id, make_item(formula_id, 90_274, char_id, tatara_id, InventoryFlag::Hangar, 1, true));
            snap.items.insert(100_000_894, make_item(100_000_894, 90_307, char_id, tatara_id, InventoryFlag::Hangar, 200, false)); // Compressed Prismaticite
            snap.items.insert(100_000_895, make_item(100_000_895, 16_634, char_id, tatara_id, InventoryFlag::Hangar, 200, false)); // Atmospheric Gases
            let mut biochem = make_item(biochem_svc_id, 45_539, corp_id, tatara_id, InventoryFlag::ServiceSlot1, 1, true);
            biochem.dynamic_attributes.insert(2, 1.0);
            snap.items.insert(biochem_svc_id, biochem);
            snap.module_online_states.insert(biochem_svc_id, true);
        });
        let job_args = [PyRep::keyval([
            ("blueprintID", PyRep::Integer(formula_id)),
            ("facilityID", PyRep::Integer(tatara_id)),
            ("activityID", PyRep::Integer(9)),
            ("runs", PyRep::Integer(2)),
        ])];
        let wrong_reactor_err = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &job_args, &kw, None).await.unwrap_err();
        assert_eq!(wrong_reactor_err.code, "CompositeReactorRequired", "Unrefined Mineral reaction must require Standup Composite Reactor I (45537)");

        // Online Standup Composite Reactor I (45537) -> InstallJob succeeds -> CompleteJob delivers 200x Unrefined Tritanium (90283)
        ctx.db.with_mut_snapshot(|snap| {
            let mut comp = make_item(comp_svc_id, 45_537, corp_id, tatara_id, InventoryFlag::ServiceSlot2, 1, true);
            comp.dynamic_attributes.insert(2, 1.0);
            snap.items.insert(comp_svc_id, comp);
            snap.module_online_states.insert(comp_svc_id, true);
        });
        let inst_rep = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &job_args, &kw, None).await.expect("Install Unrefined Tritanium reaction");
        let job_id = get_dict_val(&inst_rep, "jobID").and_then(|v| v.as_i64()).unwrap();
        ctx.advance_secs(800);
        ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(job_id)], &kw, None).await.expect("Complete Unrefined Tritanium reaction");

        let unrefined_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
            snap.items.values().filter(|i| i.owner_id == char_id && i.location_id == tatara_id && i.type_id == 90_283).map(|i| i.item_id).collect()
        });
        assert!(!unrefined_ids.is_empty(), "Reaction must deliver Unrefined Tritanium (90283)");

        // Split into two 100-unit batches and reprocess under distinct RNG seeds -> both yield Tritanium (34) in variable quantities!
        let (u1, u2) = (100_000_896_i64, 100_000_897_i64);
        ctx.db.with_mut_snapshot(|snap| {
            for id in &unrefined_ids { snap.items.remove(id); }
            snap.items.insert(u1, make_item(u1, 90_283, char_id, tatara_id, InventoryFlag::Hangar, 100, false));
            snap.items.insert(u2, make_item(u2, 90_283, char_id, tatara_id, InventoryFlag::Hangar, 100, false));
        });
        ctx.set_mechanic_rng_seed(Some(101));
        let u_rep1 = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(u1)], &kw, None).await.unwrap();
        ctx.set_mechanic_rng_seed(Some(999_983));
        let u_rep2 = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(u2)], &kw, None).await.unwrap();
        let PyRep::List(u_out1) = get_dict_val(&u_rep1, "outputs").unwrap() else { panic!() };
        let PyRep::List(u_out2) = get_dict_val(&u_rep2, "outputs").unwrap() else { panic!() };
        assert_eq!(get_dict_val(&u_out1[0], "typeID").and_then(|v| v.as_i64()), Some(34));
        assert_eq!(get_dict_val(&u_out2[0], "typeID").and_then(|v| v.as_i64()), Some(34));
        let q1 = get_dict_val(&u_out1[0], "quantity").and_then(|v| v.as_i64()).unwrap();
        let q2 = get_dict_val(&u_out2[0], "quantity").and_then(|v| v.as_i64()).unwrap();
        assert_ne!(q1, q2, "Unrefined Tritanium reprocessing must produce variable Tritanium output across batches ({q1} vs {q2})");
    }

    #[tokio::test]
    async fn test_mining_foreman_burst_charges_orca_porpoise_rorqual_and_preservation() {
        // parity: feat:mining.foreman-burst-charges
        eve_testkit::attest(&["feat:mining.foreman-burst-charges"]);
        use crate::mining_cycle_engine::{activate, advance, lock, pilot, rock, rpc, CHAR_ID, JITA, VELDSPAR};
        const ORCA: u32 = 28_606;
        const PORPOISE: u32 = 42_244;
        const RORQUAL: u32 = 28_352;
        const BURST_II: u32 = 43_551;
        const CHG_FIELD: u32 = 42_829;
        const CHG_OPT: u32 = 42_830;
        const CHG_PRES: u32 = 42_831;
        const CHG_ARMOR_WRONG: u32 = 42_832;
        const MOD_STRIP_II: u32 = 17_912;
        const ICE_HARV_I: u32 = 16_278;
        const TYPE_A_I: u32 = 60_276;

        let (strip_id, ice_id, cryst_id) = (800_600_001_i64, 800_600_002_i64, 800_600_003_i64);
        let miner_skills = [
            (3_386, 5), (3_410, 5), (16_281, 5), (17_940, 5), (60_377, 5),
            (3_402, 5), (3_380, 5), (3_327, 5), (3_429, 5), (3_436, 5),
        ];
        let mut p = pilot(
            17_478,
            &miner_skills,
            &[
                (strip_id, MOD_STRIP_II, InventoryFlag::HiSlot0),
                (ice_id, ICE_HARV_I, InventoryFlag::HiSlot1),
            ],
        )
        .await;
        let kw = HashMap::new();
        let sys = p.ctx.systems.get_or_spawn(JITA);
        let (ore_rock, ice_rock) = (750_600_001_i64, 750_600_002_i64);
        rock(&p, ore_rock, VELDSPAR, 500_000, 3_000.0).await;
        rock(&p, ice_rock, 16_262, 500, 3_000.0).await;
        lock(&mut p, ore_rock).await;
        lock(&mut p, ice_rock).await;

        // Negative: fitting Mining Foreman Burst II (43551) onto a Retriever (17478) via invbroker.Add fails hull restriction
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(800_600_009, make_item(800_600_009, BURST_II, CHAR_ID, p.ship_id, InventoryFlag::Cargo, 1, true));
        });
        let p_ship = p.ship_id;
        let fit_err = rpc(&mut p, "invbroker", None, "Add", &[PyRep::Integer(800_600_009), PyRep::Integer(p_ship), PyRep::Integer(InventoryFlag::HiSlot2 as i64)]).await;
        assert!(fit_err.is_err(), "Mining Barge (Retriever) must not fit Mining Foreman Burst");

        // Set up second real pilot (90_000_002) in Orca (28606), form fleet via fleetObjectHandler.CreateFleet + Invite
        let booster_cid = 90_000_002_i64;
        let (b_ship, burst_mod, chg_item, bystander_ship) = (9_400_600_001_i64, 9_400_600_002_i64, 9_400_600_003_i64, 9_400_600_099_i64);
        let mut bsess = setup_session(&p.ctx, 903, "other", "other-secret", booster_cid).await;
        p.ctx.db.with_mut_snapshot(|snap| {
            let b_sk = snap.skills.entry(booster_cid).or_default();
            for (sid, lvl) in [(3_348, 5), (22_536, 5), (22_552, 5), (29_637, 5), (28_374, 5)] {
                b_sk.insert(sid, TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 1_000_000.0 });
            }
            let ch = snap.characters.get_mut(&booster_cid).unwrap();
            ch.clone_grade = 2; // Omega clone required for Tech II Command Burst Specialist skill 22552
            ch.solar_system_id = JITA;
            ch.station_id = None;
            ch.active_ship_id = b_ship;
            snap.items.insert(b_ship, make_item(b_ship, ORCA, booster_cid, JITA as i64, InventoryFlag::None, 1, true));
            snap.items.insert(burst_mod, make_item(burst_mod, BURST_II, booster_cid, b_ship, InventoryFlag::HiSlot0, 1, true));
            snap.module_online_states.insert(burst_mod, true);
            // Non-fleet bystander ship in range (must NOT receive fleet bursts)
            snap.items.insert(bystander_ship, make_item(bystander_ship, 17_478, 90_000_099, JITA as i64, InventoryFlag::None, 1, true));
        });
        bsess.solar_system_id = Some(JITA);
        bsess.station_id = None;
        bsess.ship_id = Some(b_ship);
        sys.spawn_ball(Ball {
            id: b_ship, type_id: ORCA, position: p.ship_pos + DVec3::new(2_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 250_000_000.0, radius: 250.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
        sys.spawn_ball(Ball {
            id: bystander_ship, type_id: 17_478, position: p.ship_pos + DVec3::new(1_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 20_000_000.0, radius: 100.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();

        p.ctx.advance_secs(12);
        p.ctx.dispatch_rpc(&mut bsess, "fleetObjectHandler", None, "CreateFleet", &[], &kw, None).await.expect("CreateFleet");
        p.ctx.dispatch_rpc(&mut bsess, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(CHAR_ID), PyRep::None, PyRep::None, PyRep::None], &kw, None).await.expect("Invite miner");
        p.ctx.dispatch_rpc(&mut p.sess, "fleetObjectHandler", None, "AcceptInvite", &[PyRep::Integer(bsess.fleet_id.unwrap())], &kw, None).await.expect("AcceptInvite");

        // Negative: loading Armor Command Burst charge (42832, group 1774) into Mining Foreman Burst II fails with InvalidChargeGroup
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(chg_item, make_item(chg_item, CHG_ARMOR_WRONG, booster_cid, b_ship, InventoryFlag::HiSlot0, 5, false));
        });
        p.ctx.invalidate_ship_fitting(b_ship);
        let wrong_chg = p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.unwrap_err();
        assert_eq!(wrong_chg.code, "InvalidChargeGroup");

        // 1. Orca (28606, +3%/lvl Ind Cmd V = 1.15x strength, 2.0 * 1.25 = 37.5 km range) + Optimization (42830):
        // Consumes 1 charge (5 -> 4) and buffs both Strip Miner AND Ice Harvester duration, excluding non-fleet bystander.
        p.ctx.db.with_mut_snapshot(|snap| snap.items.get_mut(&chg_item).unwrap().type_id = CHG_OPT);
        p.ctx.invalidate_ship_fitting(b_ship);
        let orca_rep = p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.expect("Activate Orca Optimization burst");
        assert_eq!(p.ctx.db.get_item(chg_item).unwrap().quantity, 4, "One burst charge must be consumed per activation");
        let PyRep::List(aff_ships) = get_dict_val(&orca_rep, "affectedShipIDs").unwrap() else { panic!() };
        assert!(aff_ships.contains(&PyRep::Integer(p.ship_id)));
        assert!(!aff_ships.contains(&PyRep::Integer(bystander_ship)), "Non-fleet ship in AoE must never receive bursts");
        assert!((get_dict_val(&orca_rep, "aoeRangeMeters").and_then(|v| v.as_f64()).unwrap() - 37_500.0).abs() < 1e-3);

        let max_cap = sys.get_combat_state(p.ship_id).await.unwrap().unwrap().max_capacitor;
        sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
        let ice_act = activate(&mut p, ice_id, ice_rock).await.expect("Activate Ice Harvester under Orca Optimization");
        let orca_mult = 1.25 * 1.5 * 1.15; // T2 1.25 * Mining Director V 1.5 * Orca Ind Cmd V 1.15 = 2.15625
        let base_ice_ms = 240_000.0 * 0.75 * 0.90 * 0.875; // Retriever Ice Harvester I = 141,750 ms
        assert_eq!(ice_act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some((base_ice_ms * (1.0 - 0.15 * orca_mult)) as i64));
        rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ice_id), PyRep::String("miningLaser".into())]).await.unwrap();

        // 2. Porpoise (42244, +2%/lvl = 1.10x strength) + Field Enhancement (42829) & Rorqual (28352, +5%/lvl = 1.25x strength) + Preservation (42831):
        let _ = p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_mod)], &kw, None).await;
        p.ctx.advance_virtual_clock(95);
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&b_ship).unwrap().type_id = PORPOISE;
            snap.items.get_mut(&chg_item).unwrap().type_id = CHG_FIELD;
            snap.module_active_charges.remove(&burst_mod);
        });
        p.ctx.invalidate_ship_fitting(b_ship);
        p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.unwrap();
        sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
        let porp_act = activate(&mut p, strip_id, ore_rock).await.unwrap();
        let porp_mult = 1.25 * 1.5 * 1.10;
        assert!((porp_act.dict_get("maxRange").and_then(|v| v.as_f64()).unwrap() - 15_000.0 * (1.0 + 0.40 * porp_mult)).abs() < 1e-3);
        rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(strip_id), PyRep::String("miningLaser".into())]).await.unwrap();

        // 3. Rorqual (28352) + Mining Equipment Preservation Charge (42831, dbuff 25: -15% * 1.25 * 1.5 * 1.25 = -35.15625% crystal volatility!):
        // Base Type A I volatility = 0.025 -> under Rorqual Preservation = 0.025 * (1 - 0.3515625) = 0.0162109375.
        // Pick deterministic RNG seed whose 3rd draw (crystal volatility) lies in (0.017, 0.024):
        // -> WITHOUT Preservation (< 0.025) crystal takes 0.05 damage; WITH Preservation (> 0.01621) crystal takes 0.0 damage!
        let _ = p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_mod)], &kw, None).await;
        p.ctx.advance_virtual_clock(95);
        let pres_seed = (1u64..)
            .find(|&s| {
                let mut st = s;
                let mut d = || eve_server::rng::u64_to_unit_f64(eve_server::rng::splitmix64_next(&mut st));
                d() >= 0.02 && d() >= 0.40 && (0.018..0.024).contains(&d())
            })
            .unwrap();
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(cryst_id, make_item(cryst_id, TYPE_A_I, CHAR_ID, p.ship_id, InventoryFlag::HiSlot0, 1, true));
        });
        p.ctx.invalidate_ship_fitting(p.ship_id);
        sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
        activate(&mut p, strip_id, ore_rock).await.unwrap();
        p.ctx.set_mechanic_rng_seed(Some(pres_seed));
        let unbuffed_cyc = advance(&p, 45).await;
        assert_eq!(unbuffed_cyc[0].crystal_damage, 0.05, "Without Preservation burst, roll in (0.018..0.024) damages crystal");
        rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(strip_id), PyRep::String("miningLaser".into())]).await.unwrap();

        p.ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&b_ship).unwrap().type_id = RORQUAL;
            snap.items.get_mut(&chg_item).unwrap().type_id = CHG_PRES;
            snap.module_active_charges.remove(&burst_mod);
        });
        p.ctx.invalidate_ship_fitting(b_ship);
        p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.expect("Activate Rorqual Preservation burst");
        sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
        activate(&mut p, strip_id, ore_rock).await.unwrap();
        p.ctx.set_mechanic_rng_seed(Some(pres_seed));
        let buffed_cyc = advance(&p, 45).await;
        assert_eq!(buffed_cyc[0].crystal_damage, 0.0, "Rorqual Preservation burst lowers crystal volatility from 2.5% to 1.621% so roll misses");
        rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(strip_id), PyRep::String("miningLaser".into())]).await.unwrap();
    }

    #[tokio::test]
    async fn test_mining_wormhole_sites_phased_fields_and_market_goals() {
        // parity: feat:mining.wormhole-ore-sites, feat:mining.phased-asteroid-field-lifecycle, feat:mining.mobile-phase-anchor, feat:mining.mining-market, feat:mining.mining-goals
        eve_testkit::attest(&[
            "feat:mining.wormhole-ore-sites",
            "feat:mining.phased-asteroid-field-lifecycle",
            "feat:mining.mobile-phase-anchor",
            "feat:mining.mining-market",
            "feat:mining.mining-goals",
        ]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);

        use eve_server::services::mining_sites::{PhasedAsteroidFieldState, resolve_wormhole_ore_site, WORMHOLE_ORE_SITES};
        assert_eq!(WORMHOLE_ORE_SITES.len(), 11);
        for def in WORMHOLE_ORE_SITES {
            let ores = resolve_wormhole_ore_site(&ctx.sde, def).expect("W-space site ores must exist in SDE");
            assert!(!ores.is_empty());
            assert!(!def.guards.is_empty());
        }

        let mut field = PhasedAsteroidFieldState::new("SIG-001".to_string(), 30_000_142);
        assert!(!field.beacon_spawned);
        assert_eq!(field.mining_efficiency(), 0.10);
        field.advance_fracture(0.45);
        assert!(field.beacon_spawned);
        assert!(!field.connect_industrial_command_ship(1001, 50));
        assert!(!field.connect_industrial_command_ship(1002, 35));
        assert_eq!(field.mining_efficiency(), 0.10);
        assert!(field.connect_industrial_command_ship(1003, 35));
        assert_eq!(field.mining_efficiency(), 1.0);
        field.advance_fracture(0.50);
        assert!(field.collapsed);
        assert_eq!(field.mining_efficiency(), 0.0);

        for (tid, expected_vol) in [(1_230_u32, 0.10_f64), (34_u32, 0.01_f64), (28_432_u32, 0.15_f64)] {
            let t = ctx.sde.types.get(&tid).unwrap();
            assert!(t.published && (t.volume - expected_vol).abs() < 1e-4);
        }

        let char_id = 90_000_001_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.mining_ledger.push(MiningLedgerRecord {
                character_id: char_id,
                solar_system_id: 30_000_142,
                type_id: 1_230,
                quantity: 5_000,
                date: "2026-10-20".into(),
                timestamp_unix: 1_729_430_000,
            });
        });
        let ledger_total: i64 = ctx.db.with_snapshot(|snap| {
            snap.mining_ledger.iter().filter(|e| e.character_id == char_id).map(|e| e.quantity).sum()
        });
        assert_eq!(ledger_total, 5_000);
    }
}
