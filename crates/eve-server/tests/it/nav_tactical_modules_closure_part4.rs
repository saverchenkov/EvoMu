//! End-to-End Integration Tests closing Nosferatu, Grappler, and Mutadaptive RAR parity rows (Part 4):
//! `feat:modules.energy-nosferatu-relative-capacitor`, `feat:modules.stasis-grappler-mechanics`,
//! and `feat:modules.mutadaptive-remote-armor-repairer`.

use crate::modules_advanced_tactical::*;
use eve_db::InventoryFlag;
use eve_destiny::DVec3;
use eve_proto::PyRep;
use eve_server::services::combat_npc_police::scan_and_enforce_highsec_contraband;

#[tokio::test]
async fn test_nosferatu_relative_cap_grappler_mutadaptive_rar_and_blockade_runner() {
    // parity: feat:modules.energy-nosferatu-relative-capacitor, feat:modules.stasis-grappler-mechanics, feat:modules.mutadaptive-remote-armor-repairer
    eve_testkit::attest(&[
        "feat:modules.energy-nosferatu-relative-capacitor",
        "feat:modules.stasis-grappler-mechanics",
        "feat:modules.mutadaptive-remote-armor-repairer",
    ]);

    let ctx = build_ctx();
    let jita_sys: u32 = 30_000_142;
    let char_src: i64 = 94_051;
    let ship_src: i64 = 84_051;
    let char_tgt: i64 = 94_052;
    let ship_tgt: i64 = 84_052;
    let char_tgt2: i64 = 94_053;
    let ship_tgt2: i64 = 84_053;

    let mut sess_src = seed_pilot(&ctx, char_src, ship_src, 640, jita_sys, false);
    let _sess_tgt = seed_pilot(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, false);
    let _sess_tgt2 = seed_pilot(&ctx, char_tgt2, ship_tgt2, 12_729, jita_sys, false); // Crane (Blockade Runner)
    let sys = ctx.systems.get_or_spawn(jita_sys);

    // 1. Energy Nosferatu Relative Capacitor % & Blood Raider Override (`feat:modules.energy-nosferatu-relative-capacitor`)
    let nos_id: i64 = 77_001;
    give_item(&ctx, nos_id, 530, char_src, ship_src, InventoryFlag::HiSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(nos_id, true); });
    ctx.invalidate_ship_fitting(ship_src);

    let origin = DVec3::new(50_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_src, ship_src, 640, jita_sys, origin).await;
    spawn_in_space(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, origin + DVec3::new(1_500.0, 0.0, 0.0)).await;
    sys.add_target_lock(ship_src, eve_destiny::TargetLock {
        target_id: ship_tgt, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();

    // Target 1200/3000 (40%) < Source 800/1000 (80%): no drain on standard hull despite 1200 GJ > 800 GJ
    let mut s_cs = sys.get_combat_state(ship_src).await.unwrap().unwrap();
    s_cs.capacitor_recharge_time_ms = 0.0;
    s_cs.max_capacitor = 1_000.0;
    s_cs.capacitor = 800.0;
    sys.set_combat_state(ship_src, s_cs.clone()).await.unwrap();
    let mut t_cs = sys.get_combat_state(ship_tgt).await.unwrap().unwrap();
    t_cs.capacitor_recharge_time_ms = 0.0;
    t_cs.max_capacitor = 3_000.0;
    t_cs.capacitor = 1_200.0;
    sys.set_combat_state(ship_tgt, t_cs.clone()).await.unwrap();

    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
    ]).await.expect("Activate Nosferatu");
    for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
    assert_eq!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor, 1_200.0);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(nos_id)]).await.unwrap();

    // Target 400/500 (80%) > Source 500/2000 (25%): drains despite 400 GJ < 500 GJ
    let mut s_cs2 = sys.get_combat_state(ship_src).await.unwrap().unwrap();
    s_cs2.capacitor_recharge_time_ms = 0.0;
    s_cs2.max_capacitor = 2_000.0;
    s_cs2.capacitor = 500.0;
    sys.set_combat_state(ship_src, s_cs2).await.unwrap();
    let mut t_cs2 = sys.get_combat_state(ship_tgt).await.unwrap().unwrap();
    t_cs2.capacitor_recharge_time_ms = 0.0;
    t_cs2.max_capacitor = 500.0;
    t_cs2.capacitor = 400.0;
    sys.set_combat_state(ship_tgt, t_cs2).await.unwrap();
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
    ]).await.unwrap();
    for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
    assert!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor < 400.0);
    assert!(sys.get_combat_state(ship_src).await.unwrap().unwrap().capacitor > 500.0);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(nos_id)]).await.unwrap();

    // Blood Raider Bhaalgorn (17920) overrides relative % limit
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_src).unwrap().type_id = 17_920; });
    ctx.invalidate_ship_fitting(ship_src);
    sys.set_combat_state(ship_src, s_cs).await.unwrap();
    sys.set_combat_state(ship_tgt, t_cs).await.unwrap();
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
    ]).await.unwrap();
    for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
    assert!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor < 1_200.0);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(nos_id)]).await.unwrap();

    // 2. Heavy Stasis Grappler Mechanics (`feat:modules.stasis-grappler-mechanics`)
    let mut cruiser_sess = seed_pilot(&ctx, 94_054, 84_054, 621, jita_sys, true);
    assert!(fit_mod(&ctx, &mut cruiser_sess, 84_054, 77_010, 41_040, 19).await.is_err());

    let mut bs_fit_sess = seed_pilot(&ctx, char_src, ship_src, 640, jita_sys, true);
    fit_mod(&ctx, &mut bs_fit_sess, ship_src, 77_011, 41_040, 19).await.expect("Battleship fits 1st Grappler");
    assert!(fit_mod(&ctx, &mut bs_fit_sess, ship_src, 77_012, 41_040, 20).await.is_err());
    fit_mod(&ctx, &mut bs_fit_sess, ship_src, 77_013, 527, 20).await.expect("Fit Stasis Webifier II");

    sess_src.station_id = None;
    sess_src.solar_system_id = Some(jita_sys);
    spawn_in_space(&ctx, char_src, ship_src, 640, jita_sys, origin).await;
    spawn_in_space(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, origin + DVec3::new(500.0, 0.0, 0.0)).await;
    let (_, base_t_ball) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    let base_t_vel = base_t_ball.max_velocity;
    sys.add_target_lock(ship_src, eve_destiny::TargetLock {
        target_id: ship_tgt, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();

    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_011), PyRep::String("useMissiles".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.unwrap();
    let (_, b_500) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    assert!((b_500.max_velocity - base_t_vel * 0.20).abs() < 1.0);

    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_013), PyRep::String("useMissiles".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.unwrap();
    let (_, b_stacked) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    assert!(b_stacked.max_velocity > base_t_vel * 0.085 && b_stacked.max_velocity < base_t_vel * 0.11);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_013)]).await.unwrap();
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_011)]).await.unwrap();

    sys.set_ball_position(ship_tgt, origin + DVec3::new(16_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_011), PyRep::String("useMissiles".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.unwrap();
    let (_, b_16k) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    assert!(b_16k.max_velocity > base_t_vel * 0.90);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_011)]).await.unwrap();

    // 3. Mutadaptive Remote Armor Repairer (`feat:modules.mutadaptive-remote-armor-repairer`): spool, +150% cap (2.5x), unlock & switch reset
    assert!(fit_mod(&ctx, &mut cruiser_sess, 84_054, 77_020, 49_770, 27).await.is_err());
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_src);
        snap.module_online_states.clear();
        snap.module_active_states.clear();
    });
    let mut zarm_fit_sess = seed_pilot(&ctx, char_src, ship_src, 49_713, jita_sys, true);
    ctx.invalidate_ship_fitting(ship_src);
    fit_mod(&ctx, &mut zarm_fit_sess, ship_src, 77_021, 49_770, 27).await.expect("Zarmazd fits 1st Mutadaptive RAR");
    assert!(fit_mod(&ctx, &mut zarm_fit_sess, ship_src, 77_022, 49_770, 28).await.is_err());

    sess_src.station_id = None;
    sess_src.solar_system_id = Some(jita_sys);
    spawn_in_space(&ctx, char_src, ship_src, 49_713, jita_sys, origin).await;
    spawn_in_space(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, origin + DVec3::new(3_000.0, 0.0, 0.0)).await;
    spawn_in_space(&ctx, char_tgt2, ship_tgt2, 12_729, jita_sys, origin + DVec3::new(4_000.0, 0.0, 0.0)).await;
    for &tid in &[ship_tgt, ship_tgt2] {
        let mut dmg_cs = sys.get_combat_state(tid).await.unwrap().unwrap();
        dmg_cs.max_armor_hp = 100_000.0;
        dmg_cs.armor_hp = 1_000.0;
        sys.set_combat_state(tid, dmg_cs).await.unwrap();
        sys.add_target_lock(ship_src, eve_destiny::TargetLock {
            target_id: tid, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
        }).await.unwrap();
    }

    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_021), PyRep::String("remoteArmorRepairFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..6 { sys.step_manual(1.0).await.unwrap(); }
    let a_c1 = sys.get_combat_state(ship_tgt).await.unwrap().unwrap().armor_hp;
    assert!((a_c1 - 1_784.0).abs() < 1.0);
    for _ in 0..6 { sys.step_manual(1.0).await.unwrap(); }
    let a_c2 = sys.get_combat_state(ship_tgt).await.unwrap().unwrap().armor_hp;
    assert!((a_c2 - (1_784.0 + 784.0 * 1.1)).abs() < 1.0);

    // Step 16 more cycles (total 18 > 15 maxspool) -> effective attr 84 caps at 784.0 * 2.5 = 1,960.0 HP (+150% cap)!
    for _ in 0..(16 * 6) {
        sys.set_capacitor(ship_src, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let capped_rep = attr_from_item_info(&rpc(&ctx, &mut sess_src, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(77_021)]).await.unwrap(), 84);
    assert!((capped_rep - 784.0 * 2.5).abs() < 1e-2, "Mutadaptive RAR spool must cap at 2.5x (+150% = 1960.0): got {capped_rep}");

    // Unlocking target resets spool immediately back to 1.0x (784.0)
    rpc(&ctx, &mut sess_src, "dogmaIM", "RemoveTargets", vec![PyRep::List(vec![PyRep::Integer(ship_tgt)])]).await.unwrap();
    let rep_after_unlock = attr_from_item_info(&rpc(&ctx, &mut sess_src, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(77_021)]).await.unwrap(), 84);
    assert!((rep_after_unlock - 784.0).abs() < 1e-2);

    // Target switch resets spool back to 1.0x on new target
    sys.add_target_lock(ship_src, eve_destiny::TargetLock {
        target_id: ship_tgt, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_021), PyRep::String("remoteArmorRepairFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..12 { sys.set_capacitor(ship_src, 10_000.0).await.unwrap(); sys.step_manual(1.0).await.unwrap(); }
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_021), PyRep::String("remoteArmorRepairFalloff".into()), PyRep::Integer(ship_tgt2), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..6 { sys.set_capacitor(ship_src, 10_000.0).await.unwrap(); sys.step_manual(1.0).await.unwrap(); }
    let a_tgt2_c1 = sys.get_combat_state(ship_tgt2).await.unwrap().unwrap().armor_hp;
    assert!((a_tgt2_c1 - 1_784.0).abs() < 1.0);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_021)]).await.unwrap();

    // 4. Blockade Runner Cargo Scan Immunity, Ship Scanner Visibility, & NPC Customs Contraband (pending V24 live message)
    let cargo_scanner_id: i64 = 77_031;
    let ship_scanner_id: i64 = 77_032;
    give_item(&ctx, cargo_scanner_id, 2_038, char_src, ship_src, InventoryFlag::MedSlot0, 1); // Cargo Scanner II
    give_item(&ctx, ship_scanner_id, 443, char_src, ship_src, InventoryFlag::MedSlot1, 1); // Ship Scanner I
    give_item(&ctx, 77_035, 1_319, char_tgt2, ship_tgt2, InventoryFlag::LoSlot0, 1); // Fitted module on Crane
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(cargo_scanner_id, true);
        snap.module_online_states.insert(ship_scanner_id, true);
        snap.module_online_states.insert(77_035, true);
    });
    ctx.invalidate_ship_fitting(ship_src);
    ctx.invalidate_ship_fitting(ship_tgt2);

    // Crane (12729, Blockade Runner) carrying Tritanium, PLEX, and contraband Standard Crash Booster (9947)
    give_item(&ctx, 77_041, 34, char_tgt2, ship_tgt2, InventoryFlag::Cargo, 500);
    give_item(&ctx, 77_042, 44_992, char_tgt2, ship_tgt2, InventoryFlag::Cargo, 1);
    give_item(&ctx, 77_044, 9_947, char_tgt2, ship_tgt2, InventoryFlag::Cargo, 2);

    // 4a. Player Cargo Scanner II returns empty scan (immune)
    let scan_crane = rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(cargo_scanner_id), PyRep::String("cargoScan".into()), PyRep::Integer(ship_tgt2), PyRep::Integer(1),
    ]).await.expect("Cargo Scan Crane Blockade Runner");
    assert_eq!(scan_crane.dict_get("cargoScanImmune"), Some(&PyRep::Bool(true)));
    assert_eq!(scan_crane.dict_get("totalScannedQuantity"), Some(&PyRep::Integer(0)));
    assert_eq!(scan_crane.dict_get("scannedItems"), Some(&PyRep::List(Vec::new())));

    // 4b. Player Ship Scanner I (group 48) is NOT blocked by cargoScanResistance -> reveals fitted modules!
    let ship_scan_crane = rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(ship_scanner_id), PyRep::String("shipScan".into()), PyRep::Integer(ship_tgt2), PyRep::Integer(1),
    ]).await.expect("Ship Scan Crane Blockade Runner");
    let fitted_list = match ship_scan_crane.dict_get("fittedModules") {
        Some(PyRep::List(v)) => v.len(),
        _ => 0,
    };
    assert!(fitted_list >= 1, "Ship Scanner I must reveal fitted modules on Blockade Runner");

    // 4c. High-sec NPC customs contraband enforcement still detects, confiscates, and fines contraband inside Blockade Runner!
    let found_contraband = scan_and_enforce_highsec_contraband(&ctx, jita_sys, ship_tgt2);
    assert_eq!(found_contraband.len(), 1, "High-sec NPC customs must detect contraband inside Blockade Runner");
    assert_eq!(found_contraband[0].0, 9_947);
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&77_044)), "Contraband booster must be confiscated");

    // 4d. Bustard (12731, Deep Space Transport, cargoScanResistance = 0) exposes cargo manifest to player Cargo Scanner II
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_tgt).unwrap().type_id = 12_731; });
    give_item(&ctx, 77_043, 34, char_tgt, ship_tgt, InventoryFlag::Cargo, 500);
    let scan_bustard = rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(cargo_scanner_id), PyRep::String("cargoScan".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.expect("Scan Bustard DST");
    assert_eq!(scan_bustard.dict_get("cargoScanImmune"), None);
    assert_eq!(scan_bustard.dict_get("totalScannedQuantity"), Some(&PyRep::Integer(500)));
}
