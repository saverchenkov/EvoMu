//! End-to-End Integration Tests closing Nosferatu, Grappler, Mutadaptive RAR, and Streaming Nanite Paste (Part 4):
//! `feat:modules.energy-nosferatu-relative-capacitor`, `feat:modules.stasis-grappler-mechanics`,
//! `feat:modules.mutadaptive-remote-armor-repairer`, and `feat:modules.nanite-repair-paste-in-space`.

use crate::modules_advanced_tactical::*;
use eve_db::{ContractRecord, InventoryFlag, IskCents};
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

    // 1. Energy Nosferatu Absolute GJ Rule & Blood Raider Override (`feat:modules.energy-nosferatu-relative-capacitor`)
    let nos_id: i64 = 77_001;
    give_item(&ctx, nos_id, 530, char_src, ship_src, InventoryFlag::HiSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(nos_id, true); });
    ctx.invalidate_ship_fitting(ship_src);

    let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_src, ship_src, 640, jita_sys, origin).await;
    spawn_in_space(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, origin + DVec3::new(1_500.0, 0.0, 0.0)).await;
    sys.add_target_lock(ship_src, eve_destiny::TargetLock {
        target_id: ship_tgt, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();

    // 1a. Lower % (30% < 50%) but higher absolute GJ (Source 1500 / 5000 GJ >= Target 150 / 300 GJ): 0 GJ drained!
    let mut s_cs = sys.get_combat_state(ship_src).await.unwrap().unwrap();
    s_cs.capacitor_recharge_time_ms = 0.0;
    s_cs.max_capacitor = 5_000.0;
    s_cs.capacitor = 1_500.0;
    sys.set_combat_state(ship_src, s_cs.clone()).await.unwrap();
    let mut t_cs = sys.get_combat_state(ship_tgt).await.unwrap().unwrap();
    t_cs.capacitor_recharge_time_ms = 0.0;
    t_cs.max_capacitor = 300.0;
    t_cs.capacitor = 150.0;
    sys.set_combat_state(ship_tgt, t_cs.clone()).await.unwrap();

    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
    ]).await.expect("Activate Nosferatu");
    for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
    assert_eq!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor, 150.0);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(nos_id)]).await.unwrap();

    // 1b. Higher % (80% > 50%) but lower absolute GJ (Source 120 / 150 GJ < Target 2000 / 4000 GJ): drains!
    let mut s_cs2 = sys.get_combat_state(ship_src).await.unwrap().unwrap();
    s_cs2.capacitor_recharge_time_ms = 0.0;
    s_cs2.max_capacitor = 150.0;
    s_cs2.capacitor = 120.0;
    sys.set_combat_state(ship_src, s_cs2).await.unwrap();
    let mut t_cs2 = sys.get_combat_state(ship_tgt).await.unwrap().unwrap();
    t_cs2.capacitor_recharge_time_ms = 0.0;
    t_cs2.max_capacitor = 4_000.0;
    t_cs2.capacitor = 2_000.0;
    sys.set_combat_state(ship_tgt, t_cs2).await.unwrap();
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
    ]).await.unwrap();
    for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
    assert!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor < 2_000.0);
    assert!(sys.get_combat_state(ship_src).await.unwrap().unwrap().capacitor > 120.0);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(nos_id)]).await.unwrap();

    // 1c. Blood Raider Bhaalgorn (17920) overrides absolute GJ limit (drains Target 150 GJ even with Source 1500 GJ)
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_src).unwrap().type_id = 17_920; });
    ctx.invalidate_ship_fitting(ship_src);
    sys.set_combat_state(ship_src, s_cs).await.unwrap();
    sys.set_combat_state(ship_tgt, t_cs).await.unwrap();
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
    ]).await.unwrap();
    for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
    assert!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor < 150.0);
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

    // Overloaded Grappler (`attr 1222` = +300% optimal -> 4,000m) MUST retain full -80% web across ticks at 3,500m!
    sys.set_ball_position(ship_tgt, origin + DVec3::new(3_500.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_011), PyRep::String("useMissiles".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.unwrap();
    sys.step_manual(1.0).await.unwrap();
    let (_, b_3500_norm) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    assert!(b_3500_norm.max_velocity > base_t_vel * 0.23, "Cold grappler at 3.5km (>1km optimal) decays below full 80%");
    rpc(&ctx, &mut sess_src, "dogmaIM", "Overload", vec![PyRep::Integer(77_011)]).await.unwrap();
    sys.step_manual(1.0).await.unwrap();
    let (_, b_3500_ovl) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    assert!(
        (b_3500_ovl.max_velocity - base_t_vel * 0.20).abs() < 1.0,
        "Overloaded Grappler (+300% optimal = 4km) must retain full 80% slow across ticks at 3.5km: got {}",
        b_3500_ovl.max_velocity
    );
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

#[tokio::test]
async fn test_nanite_repair_paste_streaming_and_cargo_refund_capacity() {
    // parity: feat:modules.nanite-repair-paste-in-space
    eve_testkit::attest(&["feat:modules.nanite-repair-paste-in-space"]);

    let ctx = build_ctx();
    let char_a: i64 = 94_161;
    let ship_a: i64 = 84_161;
    let mut sess_a = seed_pilot(&ctx, char_a, ship_a, 22_456, NULL_SYS, false); // Sabre (Interdictor, 180 m3 cargo)
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_a, ship_a, 22_456, NULL_SYS, origin).await;

    set_skill_level(&ctx, char_a, 28_164, 0); // Thermodynamics L0
    set_skill_level(&ctx, char_a, 28_879, 0); // Nanite Operation L0
    set_skill_level(&ctx, char_a, 28_880, 0); // Nanite Interfacing L0

    // 1. Streaming per-tick paste deduction + clean mid-repair halt when cargo paste is exhausted!
    // Mega Pulse Laser II (`3057`, 20 m3, 40 HP) at 20.0 HP damage requires 8 paste over ~7.46s (2.68 HP/s).
    // Seed only 3 paste in cargo -> 1 consumed at t=0, 2 streamed over ticks, then repair halts cleanly at 3/8 (12.5 HP remaining)!
    let mod_stream: i64 = 77_201;
    let paste_stack: i64 = 77_202;
    give_item(&ctx, mod_stream, 3_057, char_a, ship_a, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, paste_stack, 28_668, char_a, ship_a, InventoryFlag::Cargo, 3);
    set_item_dyn_attr(&ctx, mod_stream, 3, 20.0);

    let rep_start = rpc(&ctx, &mut sess_a, "dogmaIM", "InitiateModuleRepair", vec![PyRep::Integer(mod_stream)]).await.unwrap();
    assert_eq!(rep_start.dict_get("pasteConsumed"), Some(&PyRep::Integer(1)), "Only 1 paste unit deducted at t=0");
    assert_eq!(rep_start.dict_get("totalPasteRequired"), Some(&PyRep::Integer(8)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&paste_stack).unwrap().quantity), 2, "2 paste units remain in cargo at t=0");

    for _ in 0..8 {
        sys.step_manual(1.0).await.unwrap();
    }
    let paste_left = ctx.db.with_snapshot(|s| s.items.get(&paste_stack).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(paste_left, 0, "All 3 available paste units must be streamed and consumed");
    assert!(!sys.is_module_being_nanite_repaired_sync(ship_a, mod_stream), "Repair must halt cleanly when cargo paste is exhausted");
    let dmg_after_exhaust = ctx.db.with_snapshot(|s| s.items.get(&mod_stream).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0));
    assert!(
        (dmg_after_exhaust - 12.5).abs() < 0.05,
        "Exhausting 3 of 8 required paste must repair exactly 3/8 of 20 HP (12.5 HP left), got {dmg_after_exhaust}"
    );

    // 2. Cargo capacity guard on cancellation refund: filling cargo to 100% hull capacity before t=0 StopModuleRepair
    // must NOT overflow hull cargo volume!
    give_item(&ctx, 77_203, 28_668, char_a, ship_a, InventoryFlag::Cargo, 2);
    rpc(&ctx, &mut sess_a, "dogmaIM", "InitiateModuleRepair", vec![PyRep::Integer(mod_stream)]).await.unwrap();
    // Fill Sabre cargo (400 m3) completely: 1 unit of 28668 (0.01 m3) + 39,999 Tritanium (`34`, 0.01 m3) = 400.00 m3 (0.00 m3 free!)
    give_item(&ctx, 77_204, 34, char_a, ship_a, InventoryFlag::Cargo, 39_999);
    let stop_full = rpc(&ctx, &mut sess_a, "dogmaIM", "StopModuleRepair", vec![PyRep::Integer(mod_stream)]).await.unwrap();
    assert_eq!(stop_full.dict_get("pasteRefunded"), Some(&PyRep::Integer(0)), "Refund must cap to 0 when ship cargo has 0.00 m3 free");
    let cargo_used_m3: f64 = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.location_id == ship_a && i.flag == InventoryFlag::Cargo).map(|i| {
            ctx.sde.types.get(&i.type_id).unwrap().volume * (i.quantity as f64)
        }).sum()
    });
    assert!(cargo_used_m3 <= 400.0 + 1e-6, "Cargo volume must never exceed Sabre 400 m3 capacity: got {cargo_used_m3}");
    ctx.db.with_mut_snapshot(|s| { s.items.remove(&77_204); });

    // 3. Interdiction Sphere Launcher (`BF-02`): excess probes above launcher capacity return to Cargo (never destroyed),
    // and contract-locked probes in Cargo are NOT auto-reloaded!
    let launcher_id: i64 = 77_210;
    let over_stack_id: i64 = 77_211;
    let locked_probe_id: i64 = 77_212;
    give_item(&ctx, launcher_id, 22_782, char_a, ship_a, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, over_stack_id, 22_778, char_a, ship_a, InventoryFlag::HiSlot0, 5); // 5 loaded > max 3 (15 m3 / 5 m3)
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(launcher_id, true); });
    ctx.invalidate_ship_fitting(ship_a);

    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![
        PyRep::Integer(launcher_id), PyRep::String("useMissiles".into()), PyRep::None, PyRep::Integer(0),
    ]).await.expect("Launch 1st probe with 5 loaded");
    let (rem_slot, returned_cargo) = ctx.db.with_snapshot(|s| {
        let slot_q = s.items.get(&over_stack_id).map(|i| i.quantity).unwrap_or(0);
        let cargo_q: i32 = s.items.values().filter(|i| i.location_id == ship_a && i.flag == InventoryFlag::Cargo && i.type_id == 22_778).map(|i| i.quantity).sum();
        (slot_q, cargo_q)
    });
    assert_eq!((rem_slot, returned_cargo), (2, 2), "Excess 2 probes above capacity 3 must return to Cargo (5 - 1 = 4 total preserved)");

    // Clear unlocked cargo probes, leave 1 in launcher, and lock a cargo stack in an Outstanding contract -> auto-reload must skip locked stack!
    ctx.db.with_mut_snapshot(|s| {
        s.items.retain(|_, i| !(i.location_id == ship_a && i.flag == InventoryFlag::Cargo && i.type_id == 22_778));
        s.items.get_mut(&over_stack_id).unwrap().quantity = 1;
    });
    give_item(&ctx, locked_probe_id, 22_778, char_a, ship_a, InventoryFlag::Cargo, 3);
    ctx.db.with_mut_snapshot(|s| {
        s.contracts.insert(990_001, ContractRecord {
            contract_id: 990_001, issuer_id: char_a, issuer_corp_id: 1_000_009, assignee_id: 0, acceptor_id: None,
            start_station_id: 60_003_760, end_station_id: 60_003_760, contract_type: "ItemExchange".into(),
            status: "Outstanding".into(), title: "Locked Probes".into(), for_corp: false, availability: "Public".into(),
            date_issued_unix: 1, expiration_unix: u64::MAX, num_days: 1,
            price: IskCents(100), reward: IskCents(0), collateral: IskCents(0), buyout: IskCents(0),
            current_bid: IskCents(0), current_bidder_id: None,
            item_ids: vec![locked_probe_id], requested_items: Vec::new(), bid_history: Vec::new(),
        });
    });
    for _ in 0..6 { sys.step_manual(1.0).await.unwrap(); }
    let launch_last = rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![
        PyRep::Integer(launcher_id), PyRep::String("useMissiles".into()), PyRep::None, PyRep::Integer(0),
    ]).await.unwrap();
    assert_eq!(launch_last.dict_get("reloading"), Some(&PyRep::Bool(false)), "Contract-locked cargo probes must not be auto-reloaded");
    assert_eq!(launch_last.dict_get("remainingInLauncher"), Some(&PyRep::Integer(0)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&locked_probe_id).unwrap().quantity), 3);

    // 4. Scram-aborted MWD (`BF-04`): Warp Scrambler cleans up `snap.module_active_states` and emits `OnGodmaShipEffect` deactivation!
    let char_v: i64 = 94_162;
    let ship_v: i64 = 84_162;
    let mut sess_v = seed_pilot(&ctx, char_v, ship_v, 621, NULL_SYS, false);
    let mwd_id: i64 = 77_220;
    let scram_id: i64 = 77_221;
    give_item(&ctx, mwd_id, 5_973, char_v, ship_v, InventoryFlag::MedSlot0, 1); // 50MN MWD II
    give_item(&ctx, scram_id, 448, char_a, ship_a, InventoryFlag::MedSlot0, 1); // Warp Scrambler II
    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(mwd_id, true);
        s.module_online_states.insert(scram_id, true);
    });
    ctx.invalidate_ship_fitting(ship_v);
    ctx.invalidate_ship_fitting(ship_a);
    spawn_in_space(&ctx, char_v, ship_v, 621, NULL_SYS, origin + DVec3::new(3_000.0, 0.0, 0.0)).await;
    sys.add_target_lock(ship_a, eve_destiny::TargetLock {
        target_id: ship_v, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
    }).await.unwrap();
    rpc(&ctx, &mut sess_v, "dogmaIM", "Activate", vec![
        PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1),
    ]).await.unwrap();
    assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&mwd_id)));
    let _ = sess_v.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_v);

    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![
        PyRep::Integer(scram_id), PyRep::String("warpScramblerMWD".into()), PyRep::Integer(ship_v), PyRep::Integer(1),
    ]).await.unwrap();
    assert!(
        !ctx.db.with_snapshot(|s| s.module_active_states.contains(&mwd_id)),
        "Warp Scrambler aborting MWD must remove MWD from snap.module_active_states"
    );
    let notifs_scram = ctx.drain_notifications_for_char(char_v);
    assert!(
        notifs_scram.iter().any(|n| n.event == "OnGodmaShipEffect" && n.args.first() == Some(&PyRep::Integer(mwd_id))),
        "Warp Scrambler aborting MWD must emit OnGodmaShipEffect deactivation notification to victim"
    );
}

#[tokio::test]
async fn test_wubble_destroyed_during_arming_cancels_detonation() {
    let ctx = build_ctx();
    let null_sys: u32 = 30_004_608; // 6VDT-H (-0.70 null-sec)
    let sabre_char: i64 = 94_061;
    let sabre_id: i64 = 84_061;
    let sb_char: i64 = 94_062;
    let sb_ship_id: i64 = 84_062;

    let mut sabre_sess = seed_pilot(&ctx, sabre_char, sabre_id, 22_456, null_sys, false);
    let mut sb_sess = seed_pilot(&ctx, sb_char, sb_ship_id, 640, null_sys, false);
    give_item(&ctx, 77_201, 22_782, sabre_char, sabre_id, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 77_202, 56_748, sabre_char, sabre_id, InventoryFlag::HiSlot0, 2);
    for (slot_idx, flag) in [
        (0, InventoryFlag::HiSlot0),
        (1, InventoryFlag::HiSlot1),
        (2, InventoryFlag::HiSlot2),
        (3, InventoryFlag::HiSlot3),
    ] {
        give_item(&ctx, 77_210 + slot_idx, 3_995, sb_char, sb_ship_id, flag, 1); // Large EMP Smartbomb II
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(77_201, true);
        for slot_idx in 0..4 {
            snap.module_online_states.insert(77_210 + slot_idx, true);
        }
    });
    ctx.invalidate_ship_fitting(sabre_id);
    ctx.invalidate_ship_fitting(sb_ship_id);

    let sys = ctx.systems.get_or_spawn(null_sys);
    let arena = DVec3::new(50_060_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, sabre_char, sabre_id, 22_456, null_sys, arena).await;
    spawn_in_space(&ctx, sb_char, sb_ship_id, 640, null_sys, arena + DVec3::new(1_000.0, 0.0, 0.0)).await;
    let (_, pre_sabre) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
    let base_vel = pre_sabre.max_velocity;

    let pw = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(77_201)])
        .await
        .expect("Launch Wubble 56748");
    let pw_id = pw.dict_get("probeID").and_then(|v| v.as_i64()).unwrap();

    // Advance 1.0s (< 2.0s arming window): probe is alive on grid
    sys.step_manual(1.0).await.unwrap();
    assert!(sys.get_ball_snapshot(pw_id).await.unwrap().is_some(), "Probe must still exist at t=1.0s while arming");

    // Fire 4x Large EMP Smartbomb II before 2.0s detonation -> destroys the 1,000 HP arming probe
    for slot_idx in 0..4 {
        rpc(
            &ctx,
            &mut sb_sess,
            "dogmaIM",
            "Activate",
            vec![PyRep::Integer(77_210 + slot_idx), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(1)],
        )
        .await
        .unwrap();
    }
    assert!(sys.get_ball_snapshot(pw_id).await.unwrap().is_none(), "Smartbomb AoE must destroy arming wubble probe");

    // Step well past the 2.0s detonation window: destroyed probe must NOT trigger a ghost web debuff
    for _ in 0..4 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, post_sabre) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
    assert!(
        (post_sabre.max_velocity - base_vel).abs() < 1e-2,
        "Destroyed arming wubble must NOT apply ghost velocity debuff: got {} vs {}",
        post_sabre.max_velocity,
        base_vel
    );
}

