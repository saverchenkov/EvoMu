//! Dedicated integration tests for open `feat:modules.*` parity rows (Batch 35, Part 4: Nanite Repair Paste & Industrial Cyno).

use crate::modules_advanced_tactical::{build_ctx, seed_pilot, NULL_SYS};
use crate::open_feats_b35::{give_item, put_in_fleet, set_skill, spawn_ship_in_space, DEST_NULL_SYS};
use eve_db::{InventoryFlag, IskCents, UpwellStructureRecord};
use eve_destiny::DVec3;
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn test_b35_04_nanite_repair_paste_in_space() {
    // parity: feat:modules.nanite-repair-paste-in-space
    eve_testkit::attest(&["feat:modules.nanite-repair-paste-in-space"]);
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let kw = HashMap::new();

    // Negative/abuse case 1: unauthenticated InitiateModuleRepair & StopModuleRepair rejected
    let mut unauth = eve_machonet::SessionState::new(1);
    assert!(ctx.dispatch_rpc(&mut unauth, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(1)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut unauth, "dogmaIM", None, "StopModuleRepair", &[PyRep::Integer(1)], &kw, None).await.is_err());

    let char_id = 951_301;
    let ship_id = 961_301;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, char_id, ship_id, 641, NULL_SYS, DVec3::ZERO).await;

    // Three modules of 5 m³, 10 m³, and 20 m³ volume, all using real SDE HP_STRUCT = 40.0 HP
    let mod_5m3 = 971_301;  // Small Focused Pulse Laser II (3041, vol = 5 m³, hp = 40)
    let mod_10m3 = 971_302; // Heavy Pulse Laser II (3520, vol = 10 m³, hp = 40)
    let mod_20m3 = 971_303; // Mega Pulse Laser II (3057, vol = 20 m³, hp = 40)
    let paste_id = 971_310;
    give_item(&ctx, mod_5m3, 3041, char_id, ship_id, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, mod_10m3, 3520, char_id, ship_id, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, mod_20m3, 3057, char_id, ship_id, InventoryFlag::HiSlot2, 1);
    give_item(&ctx, paste_id, 28668, char_id, ship_id, InventoryFlag::Cargo, 500);

    // 1. Burned-out module (40.0 damage on real 40.0 HP_STRUCT) cannot be repaired in space with paste
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_5m3).unwrap().dynamic_attributes.insert(3, 40.0);
    });
    assert!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None).await.is_err(),
        "Fully burned-out module (40.0 / 40.0 HP) must not be repairable in space with Nanite Repair Paste"
    );

    // 2. Active module cannot be repaired until deactivated
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_5m3).unwrap().dynamic_attributes.insert(3, 20.0);
        snap.module_active_states.insert(mod_5m3);
    });
    assert!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None).await.is_err(),
        "Active module must be rejected from InitiateModuleRepair"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&mod_5m3);
    });

    // 3. Volume scaling at 20.0 HP (50% of 40 HP) with Thermo 0, Nanite Op 0, Nanite Interfacing 0:
    // 5 m³ -> 2 paste, 10 m³ -> 4 paste, 20 m³ -> 8 paste
    set_skill(&ctx, char_id, 28164, 0); // Thermodynamics 0
    set_skill(&ctx, char_id, 28879, 0); // Nanite Operation 0
    set_skill(&ctx, char_id, 28880, 0); // Nanite Interfacing 0
    for &mid in &[mod_5m3, mod_10m3, mod_20m3] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mid).unwrap().dynamic_attributes.insert(3, 20.0);
        });
    }
    let rep_5 = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None).await.expect("Repair 5m3");
    let rep_10 = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_10m3)], &kw, None).await.expect("Repair 10m3");
    let rep_20 = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_20m3)], &kw, None).await.expect("Repair 20m3");
    let c5 = rep_5.dict_get("pasteConsumed").and_then(|v| v.as_i64()).unwrap();
    let c10 = rep_10.dict_get("pasteConsumed").and_then(|v| v.as_i64()).unwrap();
    let c20 = rep_20.dict_get("pasteConsumed").and_then(|v| v.as_i64()).unwrap();
    let rate_5 = rep_5.dict_get("repairRateHpPerSec").and_then(|v| v.as_f64()).unwrap();
    let rate_10 = rep_10.dict_get("repairRateHpPerSec").and_then(|v| v.as_f64()).unwrap();
    let rate_20 = rep_20.dict_get("repairRateHpPerSec").and_then(|v| v.as_f64()).unwrap();
    assert_eq!((c5, c10, c20), (2, 4, 8), "Paste cost must scale linearly with module volume (5m3 / 10m3 / 20m3)");
    assert!((rate_5 - 0.67).abs() < 1e-3 && (rate_10 - 1.34).abs() < 1e-3 && (rate_20 - 2.68).abs() < 1e-3, "Repair rate must scale with module volume ({rate_5}, {rate_10}, {rate_20})");

    // Shield booster (type 10838, carries SDE attr 1231 overloadShieldBonus) must also scale paste cost by volume, not attr 1231
    let mod_sb = 971_304;
    give_item(&ctx, mod_sb, 10838, char_id, ship_id, InventoryFlag::MedSlot0, 1);
    let sb_type = ctx.sde.types.get(&10838).unwrap();
    assert!(sb_type.default_attributes.contains_key(&1231), "Shield booster 10838 must carry attr 1231");
    let sb_hp = sb_type.default_attributes.get(&9).copied().unwrap_or(40.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_sb).unwrap().dynamic_attributes.insert(3, sb_hp * 0.5);
    });
    let rep_sb = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_sb)], &kw, None).await.expect("Repair shield booster");
    let c_sb = rep_sb.dict_get("pasteConsumed").and_then(|v| v.as_i64()).unwrap();
    let expected_sb = ((0.5 * 4.0 * (sb_type.volume / 5.0).clamp(1.0, 250.0)) - 1e-9).ceil().max(1.0) as i64;
    assert_eq!(c_sb, expected_sb, "Shield booster paste cost must derive from volume, not attr 1231");
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopModuleRepair", &[PyRep::Integer(mod_sb)], &kw, None).await;

    // Negative: duplicate InitiateModuleRepair on an already-repairing module rejected & preserves paste;
    // activating, overloading, or unfitting a module while undergoing nanite repair is also rejected!
    let paste_after_start = ctx.db.with_snapshot(|s| s.items.get(&paste_id).unwrap().quantity);
    assert!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None).await.is_err(), "Duplicate InitiateModuleRepair on repairing module must fail");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&paste_id).unwrap().quantity), paste_after_start, "Rejected duplicate repair must not consume extra paste");
    assert!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mod_5m3)], &kw, None).await.is_err(), "Cannot Activate module while undergoing nanite repair");
    assert!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Overload", &[PyRep::Integer(mod_5m3)], &kw, None).await.is_err(), "Cannot Overload module while undergoing nanite repair");
    assert!(ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(mod_5m3), PyRep::Integer(ship_id), PyRep::Integer(5)], &kw, None).await.is_err(), "Cannot unfit module while undergoing nanite repair");

    // 4. Step 3s mid-cycle and call StopModuleRepair on mod_20m3: partial repair committed & unused paste refunded!
    let paste_before_stop = ctx.db.with_snapshot(|s| s.items.get(&paste_id).unwrap().quantity);
    let _ = sys.step_manual(3.0).await;
    let stop_res = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopModuleRepair", &[PyRep::Integer(mod_20m3)], &kw, None).await.expect("StopModuleRepair");
    let rem_dmg = stop_res.dict_get("remainingDamage").and_then(|v| v.as_f64()).unwrap();
    let refunded = stop_res.dict_get("pasteRefunded").and_then(|v| v.as_i64()).unwrap();
    let paste_after_stop = ctx.db.with_snapshot(|s| s.items.get(&paste_id).unwrap().quantity);
    let db_dmg_mid = ctx.db.with_snapshot(|s| s.items.get(&mod_20m3).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0));
    assert!(rem_dmg > 10.0 && rem_dmg < 14.0, "3s at 2.68 HP/s must repair ~8.04 HP of 20 HP (remaining {rem_dmg})");
    assert!((db_dmg_mid - rem_dmg).abs() < 1e-3, "Partial repair must be persisted to item DAMAGE attribute");
    assert!(refunded >= 4, "Stopping at ~40% progress must refund unused paste (refunded {refunded} of 8)");
    assert_eq!(paste_after_stop, paste_before_stop + refunded as i32);

    // Step remaining 32s -> mod_5m3 and mod_10m3 complete repair all the way to 0.0 DAMAGE!
    let _ = sys.step_manual(32.0).await;
    let dmg_5_done = ctx.db.with_snapshot(|s| s.items.get(&mod_5m3).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0));
    let dmg_10_done = ctx.db.with_snapshot(|s| s.items.get(&mod_10m3).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0));
    assert!(dmg_5_done <= 1e-6 && dmg_10_done <= 1e-6, "Stepped modules must finish repairing to 0.0 DAMAGE");

    // 5. Thermodynamics V (28164), Nanite Operation V (28879) (-5%/lvl), and Nanite Interfacing V (28880) (+20%/lvl)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_20m3).unwrap().dynamic_attributes.insert(3, 20.0);
    });
    set_skill(&ctx, char_id, 28164, 5); // Thermodynamics V -> -25%
    set_skill(&ctx, char_id, 28879, 0);
    let rep_thermo = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_20m3)], &kw, None).await.expect("Repair with Thermo V");
    let c_thermo = rep_thermo.dict_get("pasteConsumed").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(c_thermo, 6, "Thermodynamics V (-25%) must reduce 8 paste to 6 paste");
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "StopModuleRepair", &[PyRep::Integer(mod_20m3)], &kw, None).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_20m3).unwrap().dynamic_attributes.insert(3, 20.0);
    });
    set_skill(&ctx, char_id, 28879, 5); // Nanite Operation V -> additional -25% (8 * 0.75 * 0.75 = 4.5 -> 5)
    set_skill(&ctx, char_id, 28880, 5); // Nanite Interfacing V -> +100% rate (5.36 HP/s on 20 m³)
    let rep_all_v = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_20m3)], &kw, None).await.expect("Repair with all V");
    let c_all_v = rep_all_v.dict_get("pasteConsumed").and_then(|v| v.as_i64()).unwrap();
    let rate_v = rep_all_v.dict_get("repairRateHpPerSec").and_then(|v| v.as_f64()).unwrap();
    assert!(c_all_v < c_thermo, "Nanite Operation V must further reduce paste cost ({c_all_v} < {c_thermo})");
    assert!((rate_v - 5.36).abs() < 1e-2, "Nanite Interfacing V must double 20m³ repair rate to 5.36 HP/s (got {rate_v})");
    let _ = sys.step_manual(20.0).await;

    // 6. Burned-out module (40.0 / 40.0 HP) IS repaired when docked at station OR tethered/at Upwell structure!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_5m3).unwrap().dynamic_attributes.insert(3, 40.0);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents(1_000_000_000);
            ch.station_id = Some(60003760);
        }
    });
    sess.station_id = Some(60003760);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_10m3).unwrap().dynamic_attributes.insert(3, 15.0);
    });
    assert!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_10m3)], &kw, None).await.is_err(), "In-space InitiateModuleRepair must be rejected while docked");
    ctx.dispatch_rpc(&mut sess, "repairSvc", None, "RepairItemsInStation", &[PyRep::List(vec![PyRep::Integer(mod_5m3)]), PyRep::Real(5_000_000.0)], &kw, None).await.expect("Repair burned-out module in station");
    let dmg_after_station = ctx.db.with_snapshot(|s| s.items.get(&mod_5m3).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0));
    assert!(dmg_after_station <= 1e-6, "Station repair service must clear burned-out module damage");

    // Burn out again in space and repair via real in-space Upwell structure tethering (leaving burned-out module offline until manually onlined)
    let astra_id = 981_301;
    let corp_id = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(1000009));
    give_item(&ctx, 981_302, 35892, corp_id, astra_id, InventoryFlag::ServiceSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(981_302, true);
        snap.module_online_states.insert(mod_5m3, false);
        snap.items.get_mut(&mod_5m3).unwrap().dynamic_attributes.insert(3, 40.0);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = NULL_SYS;
        }
        snap.structures.insert(
            astra_id,
            UpwellStructureRecord {
                structure_id: astra_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: NULL_SYS,
                name: "Repair Astrahus".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                hull_hp: 5_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(NULL_SYS);
    let _ = sys.set_ball_position(ship_id, DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None).await.unwrap_err().code,
        "ModuleBurnedOut",
        "100% burned-out module must be rejected by InitiateModuleRepair in space"
    );
    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, char_id, NULL_SYS, ship_id).await;
    let _ = sys.step_manual(1.0).await;
    assert!(sys.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered, "Ship must be tethered in space to the operational Astrahus");
    let dmg_after_1s = ctx.db.with_snapshot(|s| s.items.get(&mod_5m3).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0));
    assert!(dmg_after_1s > 0.0 && dmg_after_1s < 40.0, "Tether heat repair must be gradual, not instant (after 1s: {dmg_after_1s})");
    let _ = sys.step_manual(25.0).await;
    let (dmg_after_tether, is_online_after_tether) = ctx.db.with_snapshot(|s| {
        let dmg = s.items.get(&mod_5m3).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0);
        let onl = s.module_online_states.get(&mod_5m3).copied().unwrap_or(true);
        (dmg, onl)
    });
    assert!(dmg_after_tether <= 1e-6, "In-space Upwell structure tethering must gradually clear burned-out module heat damage");
    assert!(!is_online_after_tether, "Tether-repaired burned-out module must remain offline until manually onlined by pilot");
    // BF-B35-03: InitiateModuleRepairMany must also reject docked sessions
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760));
    sess.station_id = Some(60_003_760);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepairMany", &[PyRep::List(vec![PyRep::Integer(mod_5m3)])], &kw, None).await.unwrap_err().code,
        "CannotRepairNaniteWhileDocked"
    );

    // Regression B35R-P1-01: try_remove_ball while holding DB write lock must not deadlock when ship has active nanite repair
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().station_id = None;
        s.items.get_mut(&mod_5m3).unwrap().dynamic_attributes.insert(3, 10.0);
    });
    sess.station_id = None;
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "InitiateModuleRepair", &[PyRep::Integer(mod_5m3)], &kw, None)
        .await
        .expect("Start nanite repair before try_remove_ball");
    ctx.db.with_mut_snapshot(|_| {
        sys.try_remove_ball(ship_id);
    });
    assert!(sys.get_ball_snapshot(ship_id).await.unwrap().is_none(), "Ship ball must be removed without deadlocking");
}

#[tokio::test]
async fn test_b35_05_industrial_cyno_rules() {
    // parity: feat:modules.industrial-cyno-rules
    eve_testkit::attest(&["feat:modules.industrial-cyno-rules"]);
    let ctx = build_ctx();
    let kw = HashMap::new();

    // Negative/abuse cases (>= 2): Battleship hull rejected & High-sec (Jita 30000142) rejected
    let mega_char = 952_001;
    let mega_ship = 962_001;
    let mut mega_sess = seed_pilot(&ctx, mega_char, mega_ship, 641, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, mega_char, mega_ship, 641, DEST_NULL_SYS, DVec3::ZERO).await;
    set_skill(&ctx, mega_char, 3336, 5);
    set_skill(&ctx, mega_char, 21603, 5);
    give_item(&ctx, 972_001, 52694, mega_char, mega_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_002, 16273, mega_char, mega_ship, InventoryFlag::Cargo, 1_000);
    assert!(
        ctx.dispatch_rpc(&mut mega_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_001), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.is_err(),
        "Battleship must not be allowed to light Industrial Cyno"
    );

    let hs_char = 952_010;
    let hs_ship = 962_010;
    let mut hs_sess = seed_pilot(&ctx, hs_char, hs_ship, 32880, 30000142, false);
    spawn_ship_in_space(&ctx, hs_char, hs_ship, 32880, 30000142, DVec3::ZERO).await;
    set_skill(&ctx, hs_char, 32918, 5);
    set_skill(&ctx, hs_char, 21603, 5);
    give_item(&ctx, 972_010, 52694, hs_char, hs_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_011, 16273, hs_char, hs_ship, InventoryFlag::Cargo, 1_000);
    assert!(
        ctx.dispatch_rpc(&mut hs_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_010), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.is_err(),
        "Industrial Cyno must be rejected in high-security space"
    );

    // Legal Hull Matrix:
    // (a) T1 Hauler (Badger 648): CFT I -> 720 LO; check base 800 * (1 - 0.1*lvl) -> CFT V = 400 LO
    for (idx, hull_tid, cft_lvl, expected_lo, extra_skills) in [
        (0usize, 648u32, 1u8, 720i64, &[(3341u32, 5u8)][..]),
        (1, 648, 5, 400, &[(3341, 5)][..]),
        // (b) Blockade Runner (Crane 12729): CFT V -> 400 LO
        (2, 12729, 5, 400, &[(3341, 5), (19719, 5)][..]),
        // (c) Deep Space Transport (Bustard 12731): CFT III -> 560 LO
        (3, 12731, 3, 560, &[(3341, 5), (19719, 5)][..]),
        // (d) Venture (32880): -50% role bonus -> 400 LO at CFT I AND 400 LO minimum floor at CFT V!
        (4, 32880, 1, 400, &[(32918, 5)][..]),
    ] {
        let cid = 952_020 + idx as i64;
        let sid = 962_020 + idx as i64;
        let mid = 972_020 + idx as i64 * 2;
        let mut s = seed_pilot(&ctx, cid, sid, hull_tid, DEST_NULL_SYS, false);
        spawn_ship_in_space(&ctx, cid, sid, hull_tid, DEST_NULL_SYS, DVec3::new((idx as f64 + 1.0) * 5_000.0, 0.0, 0.0)).await;
        set_skill(&ctx, cid, 21603, cft_lvl);
        for &(sk, lv) in extra_skills {
            set_skill(&ctx, cid, sk, lv);
        }
        give_item(&ctx, mid, 52694, cid, sid, InventoryFlag::HiSlot0, 1);
        give_item(&ctx, mid + 1, 16273, cid, sid, InventoryFlag::Cargo, 2_000);
        ctx.invalidate_ship_fitting(sid);
        put_in_fleet(&ctx, 880_002, cid, &[(cid, true)]);
        let res = ctx.dispatch_rpc(&mut s, "dogmaIM", None, "Activate", &[PyRep::Integer(mid), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Activate Industrial Cyno");
        assert_eq!(res.dict_get("consumptionQuantity"), Some(&PyRep::Integer(expected_lo)), "Hull {hull_tid} CFT {cft_lvl} LO mismatch");
    }

    // Venture (32880) with CFT V: verify 400 LO minimum floor, overview ball presence, AND immobilization
    let ven_char = 952_101;
    let ven_ship = 962_101;
    let mut ven_sess = seed_pilot(&ctx, ven_char, ven_ship, 32880, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, ven_char, ven_ship, 32880, DEST_NULL_SYS, DVec3::new(50_000.0, 0.0, 0.0)).await;
    set_skill(&ctx, ven_char, 32918, 5);
    set_skill(&ctx, ven_char, 21603, 5); // CFT V on Venture: 800 * 0.5 * 0.5 = 200 -> clamped to 400 floor!
    give_item(&ctx, 972_101, 52694, ven_char, ven_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 972_102, 16273, ven_char, ven_ship, InventoryFlag::Cargo, 1_000);
    let ven_beyonce = ctx.bind_object(&mut ven_sess, "beyonce", DEST_NULL_SYS as i64).unwrap();
    put_in_fleet(&ctx, 880_001, ven_char, &[(ven_char, true)]);
    let cyno_res = ctx.dispatch_rpc(&mut ven_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(972_101), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &kw, None).await.expect("Venture CFT V industrial cyno");
    assert_eq!(cyno_res.dict_get("consumptionQuantity"), Some(&PyRep::Integer(400)), "Venture + CFT V must respect 400 LO minimum floor");
    let beacon_id = cyno_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    let dest_sys = ctx.systems.get_or_spawn(DEST_NULL_SYS);
    assert!(dest_sys.get_ball_snapshot(beacon_id).await.unwrap().is_some(), "Industrial cyno beacon must appear in space/overview");
    let (_, ven_ball) = dest_sys.get_ball_snapshot(ven_ship).await.unwrap().unwrap();
    assert!(ven_ball.max_velocity <= 1e-9, "Active Industrial Cyno must immobilize the cyno ship (max_velocity = 0)");
    let dest_gate = ctx.sde.stargates().values().find(|j| j.from_solar_system_id == DEST_NULL_SYS).map(|j| j.stargate_id).unwrap();
    assert!(ctx.dispatch_rpc(&mut ven_sess, "beyonce", Some(ven_beyonce), "CmdGotoDirection", &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut ven_sess, "beyonce", Some(ven_beyonce), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(dest_gate), PyRep::Real(0.0)], &kw, None).await.is_err());

    // Jump & Bridge eligibility matrix against Industrial Cyno:
    let car_char = 952_104;
    let car_ship = 962_104;
    let mut car_sess = seed_pilot(&ctx, car_char, car_ship, 23757, NULL_SYS, false); // Archon
    spawn_ship_in_space(&ctx, car_char, car_ship, 23757, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_104, 16274, car_char, car_ship, InventoryFlag::SpecializedFuelBay, 50_000);

    let jf_char = 952_102;
    let jf_ship = 962_102;
    let mut jf_sess = seed_pilot(&ctx, jf_char, jf_ship, 28844, NULL_SYS, false); // Rhea
    spawn_ship_in_space(&ctx, jf_char, jf_ship, 28844, NULL_SYS, DVec3::new(2_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_103, 17888, jf_char, jf_ship, InventoryFlag::SpecializedFuelBay, 50_000);

    let blops_char = 952_105;
    let blops_ship = 962_105;
    let mut blops_sess = seed_pilot(&ctx, blops_char, blops_ship, 22428, NULL_SYS, false); // Redeemer
    spawn_ship_in_space(&ctx, blops_char, blops_ship, 22428, NULL_SYS, DVec3::new(3_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_105, 16274, blops_char, blops_ship, InventoryFlag::SpecializedFuelBay, 50_000);
    give_item(&ctx, 972_106, 28652, blops_char, blops_ship, InventoryFlag::HiSlot0, 1);

    let rorq_char = 952_106;
    let rorq_ship = 962_106;
    let mut rorq_sess = seed_pilot(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, false); // Rorqual
    spawn_ship_in_space(&ctx, rorq_char, rorq_ship, 28352, NULL_SYS, DVec3::new(4_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 972_107, 17887, rorq_char, rorq_ship, InventoryFlag::SpecializedFuelBay, 50_000);
    give_item(&ctx, 972_108, 63140, rorq_char, rorq_ship, InventoryFlag::HiSlot0, 1);

    put_in_fleet(&ctx, 880_001, ven_char, &[(ven_char, true), (car_char, true), (jf_char, true), (blops_char, true), (rorq_char, true)]);

    assert!(ctx.dispatch_rpc(&mut blops_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_ok());
    assert!(ctx.dispatch_rpc(&mut rorq_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_ok());
    assert!(ctx.dispatch_rpc(&mut blops_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_err(), "Cannot jump while own portal is open");
    ctx.advance_time_seconds(65);
    assert!(ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut jf_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_ok());
    assert!(ctx.dispatch_rpc(&mut blops_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_ok());
    assert!(ctx.dispatch_rpc(&mut rorq_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(ven_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &kw, None).await.is_ok());
}

// parity: sde:res:/localizationfsd/localization_fsd_main.pickle, sde:res:/localizationfsd/localization_fsd_en-us.pickle, sde:res:/localizationfsd/localization_fsd_de.pickle, sde:res:/localizationfsd/localization_fsd_es.pickle, sde:res:/localizationfsd/localization_fsd_fr.pickle, sde:res:/localizationfsd/localization_fsd_ja.pickle, sde:res:/localizationfsd/localization_fsd_ko.pickle, sde:res:/localizationfsd/localization_fsd_ru.pickle, sde:res:/localizationfsd/localization_fsd_zh.pickle
#[test]
fn test_b35_esi_localization_fsd_accept_language() {
    eve_testkit::attest(&[
        "sde:res:/localizationfsd/localization_fsd_main.pickle",
        "sde:res:/localizationfsd/localization_fsd_en-us.pickle",
        "sde:res:/localizationfsd/localization_fsd_de.pickle",
        "sde:res:/localizationfsd/localization_fsd_es.pickle",
        "sde:res:/localizationfsd/localization_fsd_fr.pickle",
        "sde:res:/localizationfsd/localization_fsd_ja.pickle",
        "sde:res:/localizationfsd/localization_fsd_ko.pickle",
        "sde:res:/localizationfsd/localization_fsd_ru.pickle",
        "sde:res:/localizationfsd/localization_fsd_zh.pickle",
    ]);
    let ctx = std::sync::Arc::new(build_ctx());
    let main_bundle = ctx.sde.load_localization_main().expect("load_localization_main");
    assert_eq!(main_bundle.labels.len(), 31_872);
    assert_eq!(main_bundle.languages.len(), 9);
    let router = eve_server::esi::EsiRouter::new(std::sync::Arc::clone(&ctx));

    let req_en = eve_server::esi::EsiHttpRequest::new("GET", "/v1/universe/types/587/")
        .with_header("Accept-Language", "en-us");
    let resp_en = router.handle_request(&req_en);
    assert_eq!(resp_en.status_code, 200);
    assert_eq!(resp_en.headers.get("Content-Language").map(String::as_str), Some("en"));
    let en_json: serde_json::Value = serde_json::from_slice(&resp_en.body).unwrap();
    assert_eq!(en_json["name"].as_str(), Some("Rifter"));

    for lang in ["de", "es", "fr", "ja", "ko", "ru", "zh"] {
        let req_type = eve_server::esi::EsiHttpRequest::new(
            "GET",
            &format!("/v1/universe/types/587/?language={lang}"),
        );
        let resp_type = router.handle_request(&req_type);
        assert_eq!(resp_type.status_code, 200);
        assert_eq!(resp_type.headers.get("Content-Language").map(String::as_str), Some(lang));
        let type_json: serde_json::Value = serde_json::from_slice(&resp_type.body).unwrap();
        let expected_type = ctx.sde.localized_type_name(lang, 587).expect("localized type 587");
        let expected_desc = ctx.sde.localized_type_description(lang, 587).expect("localized desc 587");
        assert_eq!(type_json["name"].as_str(), Some(expected_type.as_str()));
        assert_eq!(type_json["description"].as_str(), Some(expected_desc.as_str()));

        let req_grp = eve_server::esi::EsiHttpRequest::new("GET", "/v1/universe/groups/25/")
            .with_header("Accept-Language", lang);
        let resp_grp = router.handle_request(&req_grp);
        assert_eq!(resp_grp.status_code, 200);
        let grp_json: serde_json::Value = serde_json::from_slice(&resp_grp.body).unwrap();
        let expected_grp = ctx.sde.localized_group_name(lang, 25).expect("localized group 25");
        assert_eq!(grp_json["name"].as_str(), Some(expected_grp.as_str()));

        let req_cat = eve_server::esi::EsiHttpRequest::new("GET", "/v1/universe/categories/6/")
            .with_header("Accept-Language", lang);
        let resp_cat = router.handle_request(&req_cat);
        assert_eq!(resp_cat.status_code, 200);
        assert_eq!(resp_cat.headers.get("Content-Language").map(String::as_str), Some(lang));
        let cat_json: serde_json::Value = serde_json::from_slice(&resp_cat.body).unwrap();
        let expected_cat = ctx.sde.localized_category_name(lang, 6).expect("localized category 6");
        assert_eq!(cat_json["name"].as_str(), Some(expected_cat.as_str()));

        let req_mg = eve_server::esi::EsiHttpRequest::new("GET", "/v1/markets/groups/4/")
            .with_header("Accept-Language", lang);
        let resp_mg = router.handle_request(&req_mg);
        assert_eq!(resp_mg.status_code, 200);
        let mg_json: serde_json::Value = serde_json::from_slice(&resp_mg.body).unwrap();
        let expected_mg = ctx.sde.localized_market_group_name(lang, 4).expect("localized market group 4");
        assert_eq!(mg_json["name"].as_str(), Some(expected_mg.as_str()));
    }
}
