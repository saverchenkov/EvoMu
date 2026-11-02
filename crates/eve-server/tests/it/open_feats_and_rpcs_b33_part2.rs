//! End-to-end integration tests for Batch 33 open `feat:mining.*` and `feat:modules.*` rows (part 2:
//! moon extraction duration/yield, auto-fracture, moon ore class by security, Upwell refinery
//! base yield, NPC reprocessing tax curve, Scrapmetal exclusions, ARSB/ARAR, and Auto Targeting System).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::mining_cycle_engine::{lock, pilot, rpc, CHAR_ID, JITA, MINER_I, VELDSPAR};
use crate::open_feats_and_rpcs_b33::{dict_get, insert_item};
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;

const PORPOISE: u32 = 42_244;
const RORQUAL: u32 = 28_352;
const ATHANOR: u32 = 35_835;
const TATARA: u32 = 35_836;
const MOON_DRILL_I: u32 = 35_894;
const ARAR_MEDIUM: u32 = 41_477;
const ARSB_MEDIUM: u32 = 41_481;
const AUTO_TARGETING_SYS_I: u32 = 1_182;
const NULL_SYS: u32 = 30_004_759; // 1DQ1-A

#[tokio::test]
async fn test_b33_moon_mining_and_reprocessing_curves() {
    // parity: feat:mining.moon-extraction-duration-yield, feat:mining.moon-chunk-auto-fracture, feat:mining.moon-ore-class-by-security, feat:mining.npc-reprocessing-equipment-and-tax-curve, feat:mining.scrapmetal-yield-exclusions, feat:mining.upwell-refinery-base-yield
    eve_testkit::attest(&[
        "feat:mining.moon-extraction-duration-yield",
        "feat:mining.moon-chunk-auto-fracture",
        "feat:mining.moon-ore-class-by-security",
        "feat:mining.npc-reprocessing-equipment-and-tax-curve",
        "feat:mining.scrapmetal-yield-exclusions",
        "feat:mining.upwell-refinery-base-yield",
    ]);

    let p = pilot(PORPOISE, &[(12_196, 5)], &[]).await;
    let kw = HashMap::new();
    let mut sess = p.sess.clone();

    let corp_id = 9_800_001_i64;
    let athanor_id = 1_000_000_001_001_i64;
    let tatara_id = 1_000_000_001_002_i64;
    let null_athanor_id = 1_000_000_001_003_i64;
    let now = p.ctx.now_unix_secs();
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "B33 Mining Corp".into(),
                ticker: "B33M".into(),
                ceo_id: CHAR_ID,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.corporation_id = corp_id;
        }
        for (sid, tid, sys_id, pos) in [
            (athanor_id, ATHANOR, JITA, [100_000.0, 0.0, 0.0]),
            (tatara_id, TATARA, JITA, [200_000.0, 0.0, 0.0]),
            (null_athanor_id, ATHANOR, NULL_SYS, [300_000.0, 0.0, 0.0]),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: tid,
                    owner_corp_id: corp_id,
                    solar_system_id: sys_id,
                    name: format!("B33 Refinery {sid}"),
                    position: pos,
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 1_000,
                    shield_hp: 1.0,
                    armor_hp: 1.0,
                    hull_hp: 1.0,
                    ..Default::default()
                },
            );
        }
        for (iid, tid, loc, flag) in [
            (9_400_001, MOON_DRILL_I, athanor_id, InventoryFlag::ServiceSlot0),
            (9_400_002, 35_893, athanor_id, InventoryFlag::ServiceSlot1),
            (9_400_003, 35_893, tatara_id, InventoryFlag::ServiceSlot0),
            (9_400_004, MOON_DRILL_I, null_athanor_id, InventoryFlag::ServiceSlot0),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: corp_id,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(iid, true);
        }
    });
    sess.corporation_id = Some(corp_id);
    sess.corp_role = u64::MAX;

    // 1. Duration bounds (< 6d and > 56d refused), CancelExtraction produces no chunk
    assert!(
        p.ctx
            .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(athanor_id), PyRep::Integer(518_399)], &kw, None)
            .await
            .is_err(),
        "Extraction < 6 days must be refused"
    );
    assert!(
        p.ctx
            .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(athanor_id), PyRep::Integer(4_838_401)], &kw, None)
            .await
            .is_err(),
        "Extraction > 56 days must be refused"
    );

    let start_rep = p
        .ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(athanor_id), PyRep::Integer(864_000)], &kw, None)
        .await
        .expect("StartNewExtraction 10d");
    assert_eq!(dict_get(&start_rep, "chunkArrivalTime"), Some(&PyRep::Integer((now + 864_000) as i64)));
    let _ = p
        .ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "CancelExtraction", &[PyRep::Integer(athanor_id)], &kw, None)
        .await
        .expect("CancelExtraction");
    p.ctx.clock().advance_secs(864_000 + 10_801);
    eve_server::services::mining_advanced::tick_all_moon_extractions(&p.ctx).await;
    let cancelled_spawned = p.ctx.db.with_snapshot(|s| {
        s.moon_extractions.get(&athanor_id).map(|e| e.spawned_asteroid_ids.is_empty()).unwrap_or(true)
    });
    assert!(cancelled_spawned, "CancelExtraction must stop the extraction and produce no chunk");

    // 2. 6-day extraction (144h * 30,000 m³/h = 4,320,000 m³), early fracture refused, 3h auto-fracture, high-sec R4 vs null-sec R4-R64 + quality mix
    let _ = p
        .ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(athanor_id), PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction 6d");
    let _ = p
        .ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(null_athanor_id), PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction null 6d");
    assert!(
        p.ctx
            .dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(athanor_id)], &kw, None)
            .await
            .is_err(),
        "FractureChunkWithStructure before chunk arrival must be refused"
    );
    p.ctx.clock().advance_secs(518_400 + 10_801);
    eve_server::services::mining_advanced::tick_all_moon_extractions(&p.ctx).await;
    let (auto_fractured, spawned_ids, chunk_vol_m3, null_ids) = p.ctx.db.with_snapshot(|s| {
        let ext = s.moon_extractions.get(&athanor_id).unwrap();
        let vol: f64 = ext
            .spawned_asteroid_ids
            .iter()
            .filter_map(|id| s.asteroids.get(id))
            .map(|a| (a.quantity as f64) * p.ctx.sde.types.get(&a.type_id).map(|t| t.volume).unwrap_or(0.0))
            .sum();
        let n_ids = s.moon_extractions.get(&null_athanor_id).map(|e| e.spawned_asteroid_ids.clone()).unwrap_or_default();
        (ext.auto_fracture, ext.spawned_asteroid_ids.clone(), vol, n_ids)
    });
    assert!(auto_fractured && !spawned_ids.is_empty());
    assert!(
        (chunk_vol_m3 - 4_320_000.0).abs() < 1_000.0,
        "6-day (144h) chunk volume must be 4,320,000 m³ (30,000 m³/h), got {chunk_vol_m3}"
    );
    let all_r4 = p.ctx.db.with_snapshot(|s| {
        spawned_ids.iter().all(|id| {
            s.asteroids.get(id).and_then(|a| p.ctx.sde.types.get(&a.type_id)).is_some_and(|t| t.group_id == 1884)
        })
    });
    assert!(all_r4, "High-sec moon fracture must contain only R4 ores (group 1884)");

    let (null_valid_groups, has_improved) = p.ctx.db.with_snapshot(|s| {
        let valid = null_ids.iter().all(|id| {
            s.asteroids
                .get(id)
                .and_then(|a| p.ctx.sde.types.get(&a.type_id))
                .is_some_and(|t| matches!(t.group_id, 1884 | 1920 | 1921 | 1922 | 1923))
        });
        let imp = null_ids.iter().any(|id| {
            s.asteroids
                .get(id)
                .and_then(|a| p.ctx.sde.types.get(&a.type_id))
                .is_some_and(|t| t.default_attributes.contains_key(&2699) || t.name.starts_with("Bountiful ") || t.name.starts_with("Glowing ") || t.name.starts_with("Twinkling ") || t.name.starts_with("Scintillating ") || t.name.starts_with("Sparkling "))
        });
        (valid, imp)
    });
    assert!(null_valid_groups && !null_ids.is_empty(), "Null-sec moon fracture must contain R4-R64 moon ores");
    assert!(has_improved, "Fractured moon field must mix basic and improved (+15%) quality ores");

    // 3. Upwell refinery base yield (Athanor 51% -> 211 Tritanium, Tatara 52% -> 215 Tritanium)
    insert_item(&p.ctx, 9_400_010, VELDSPAR, CHAR_ID, athanor_id, InventoryFlag::Hangar, 100);
    insert_item(&p.ctx, 9_400_011, VELDSPAR, CHAR_ID, tatara_id, InventoryFlag::Hangar, 100);
    insert_item(&p.ctx, 9_400_012, MINER_I, CHAR_ID, 60_003_760, InventoryFlag::Hangar, 2);

    sess.station_id = None;
    sess.structure_id = Some(athanor_id);
    p.ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&CHAR_ID) {
            ch.station_id = None;
            ch.structure_id = Some(athanor_id);
        }
    });
    let q_ath = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(9_400_010)], &kw, None).await.unwrap();
    let ath_yield = dict_get(&q_ath, "yieldRate").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((ath_yield - 0.51).abs() < 1e-4, "Athanor base yield must be 51%, got {ath_yield}");
    let ath_trit: i32 = p.ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.location_id == athanor_id && i.type_id == 34).map(|i| i.quantity).sum()
    });
    assert_eq!(ath_trit, 204, "100 Veldspar (400 base Trit) at 51% Athanor must yield 204 Tritanium");

    sess.structure_id = Some(tatara_id);
    p.ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&CHAR_ID) { ch.structure_id = Some(tatara_id); }
    });
    let q_tat = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(9_400_011)], &kw, None).await.unwrap();
    let tat_yield = dict_get(&q_tat, "yieldRate").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((tat_yield - 0.5275).abs() < 1e-4, "Tatara base yield (52% + 1.5% hull bonus) must be 52.75%, got {tat_yield}");
    let tat_trit: i32 = p.ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.location_id == tatara_id && i.type_id == 34).map(|i| i.quantity).sum()
    });
    assert_eq!(tat_trit, 211, "100 Veldspar (400 base Trit) at 52.75% Tatara must yield 211 Tritanium");

    // 4. NPC station tax curve (5% at 0.0, linear at 3.335, 0% at 6.67, max(personal, corp)), per-station equipment yield, and Scrapmetal 55% cap
    let veld_type = p.ctx.sde.types.get(&VELDSPAR).unwrap();
    let (tax_zero, tax_mid, tax_max_corp, equip_yield) = p.ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&CHAR_ID).unwrap().standings.insert(1_000_035, 0.0);
        let (_, _, t0) = eve_server::services::mining_cycle::compute_reprocessing_rates_for_item_at(&p.ctx.sde, s, CHAR_ID, 60_003_760, veld_type, Some(now));
        s.characters.get_mut(&CHAR_ID).unwrap().standings.insert(1_000_035, 3.335);
        let (_, _, t_mid) = eve_server::services::mining_cycle::compute_reprocessing_rates_for_item_at(&p.ctx.sde, s, CHAR_ID, 60_003_760, veld_type, Some(now));
        s.characters.get_mut(&CHAR_ID).unwrap().standings.insert(1_000_035, 0.0);
        let mut corp_char = s.characters.get(&CHAR_ID).unwrap().clone();
        corp_char.character_id = corp_id;
        corp_char.standings.insert(1_000_035, 6.67);
        s.characters.insert(corp_id, corp_char);
        let (base_eq, _, t_corp) = eve_server::services::mining_cycle::compute_reprocessing_rates_for_item_at(&p.ctx.sde, s, CHAR_ID, 60_003_760, veld_type, Some(now));
        (t0, t_mid, t_corp, base_eq)
    });
    assert!((tax_zero - 0.05).abs() < 1e-4, "NPC tax at 0.0 standing must be 5%, got {tax_zero}");
    assert!((tax_mid - 0.025).abs() < 1e-3, "NPC tax at 3.335 standing must be ~2.5%, got {tax_mid}");
    assert!(tax_max_corp.abs() < 1e-4, "NPC tax with corp standing 6.67 (max(personal, corp)) must be 0%, got {tax_max_corp}");
    assert!((0.30..=0.50).contains(&equip_yield), "NPC station equipment base yield must be in 30..=50%, got {equip_yield}");

    sess.structure_id = None;
    sess.station_id = Some(60_003_760);
    p.ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&CHAR_ID) {
            ch.structure_id = None;
            ch.station_id = Some(60_003_760);
        }
    });
    let q_npc = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(9_400_012)], &kw, None).await.unwrap();
    let scrap_yield = dict_get(&q_npc, "yieldRate").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((scrap_yield - 0.55).abs() < 1e-4, "Scrapmetal V at 50% facility must yield 55%, got {scrap_yield}");
}

#[tokio::test]
async fn test_b33_ancillary_remote_repairers_and_auto_targeting_system() {
    // parity: feat:modules.ancillary-remote-repairers, feat:modules.auto-targeting-system
    eve_testkit::attest(&[
        "feat:modules.ancillary-remote-repairers",
        "feat:modules.auto-targeting-system",
    ]);

    let arar_id = 9_500_001_i64;
    let arsb_id = 9_500_002_i64;
    let ats_id = 9_500_003_i64;
    let paste_id = 9_500_004_i64;
    let cap_charge_id = 9_500_005_i64;
    let reload_paste_id = 9_500_006_i64;

    let mut p = pilot(
        RORQUAL,
        &[
            (3_428, 4), // Target Management IV
            (3_392, 5), // Mechanics V
            (3_394, 5), // Hull Upgrades V
            (3_416, 5), // Shield Operation V
            (3_422, 5), // Shield Emission Systems V
            (16_069, 5), // Remote Armor Repair Systems V
            (28_374, 5), // Capital Industrial Ships V
        ],
        &[
            (arar_id, ARAR_MEDIUM, InventoryFlag::HiSlot0),
            (arsb_id, ARSB_MEDIUM, InventoryFlag::HiSlot1),
            (ats_id, AUTO_TARGETING_SYS_I, InventoryFlag::HiSlot2),
        ],
    )
    .await;

    insert_item(&p.ctx, paste_id, 28_668, CHAR_ID, p.ship_id, InventoryFlag::HiSlot0, 8);
    insert_item(&p.ctx, cap_charge_id, 3_200, CHAR_ID, p.ship_id, InventoryFlag::HiSlot1, 1);
    insert_item(&p.ctx, reload_paste_id, 28_668, CHAR_ID, p.ship_id, InventoryFlag::Cargo, 16);
    p.ctx.invalidate_ship_fitting(p.ship_id);

    // 1. Auto Targeting System I passively adds +2 maxLockedTargets (5 -> 7) and auto-locks nearby targets when activated
    let fit = p.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p.ship_id).unwrap();
    let max_targets = fit.ship.get_attribute(192, &p.ctx.sde);
    assert!((max_targets - 7.0).abs() < 1e-4, "Auto Targeting System I must passively add +2 maxLockedTargets (expected 7, got {max_targets})");

    let target_ship_id = 9_500_099_i64;
    let sys_handle = p.ctx.systems.get_or_spawn(JITA);
    sys_handle
        .spawn_ball(Ball {
            id: target_ship_id,
            type_id: PORPOISE,
            position: p.ship_pos + DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    let ats_rep = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(ats_id)]).await.unwrap();
    assert!(
        dict_get(&ats_rep, "lockingCount").and_then(|v| v.as_i64()).unwrap_or(0) >= 1,
        "Active Auto Targeting System I must automatically initiate target locks"
    );

    // 2. ARSB consumes cap booster charge instead of capacitor while loaded, and falls back to capacitor when unloaded
    lock(&mut p, target_ship_id).await;
    let _ = sys_handle.try_update_combat_state(p.ship_id, |c| {
        c.capacitor = 10_000.0;
        c.max_capacitor = 10_000.0;
    });
    let cap_before = sys_handle.get_combat_state(p.ship_id).await.unwrap().map(|c| c.capacitor).unwrap_or(10_000.0);
    let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(arsb_id), PyRep::Integer(target_ship_id)]).await.unwrap();
    let cap_after_loaded = sys_handle.get_combat_state(p.ship_id).await.unwrap().map(|c| c.capacitor).unwrap_or(0.0);
    assert!((cap_before - cap_after_loaded).abs() < 1e-3, "ARSB must consume 0 capacitor while loaded");
    assert!(
        p.ctx.db.with_snapshot(|s| s.items.get(&cap_charge_id).is_none()),
        "ARSB must consume 1 cap booster charge on activation"
    );
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(arsb_id)]).await;
    p.ctx.clock().advance_secs(60);
    p.ctx.invalidate_ship_fitting(p.ship_id);
    let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(arsb_id), PyRep::Integer(target_ship_id)]).await.unwrap();
    let cap_after_unloaded = sys_handle.get_combat_state(p.ship_id).await.unwrap().map(|c| c.capacitor).unwrap_or(0.0);
    assert!(cap_after_unloaded < cap_after_loaded - 1.0, "Unloaded ARSB must fall back to consuming capacitor");

    // 3. ARAR gives 3x repair bonus with Nanite Repair Paste vs unloaded fallback, and reloads in 60s
    let _ = sys_handle.try_update_combat_state(target_ship_id, |c| { c.armor_hp = 100.0; });
    let armor_0 = sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap().armor_hp;
    let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(arar_id), PyRep::Integer(target_ship_id)]).await.unwrap();
    sys_handle.step_manual(12.0).await.unwrap();
    let armor_1 = sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap().armor_hp;
    let repaired_loaded = armor_1 - armor_0;
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(arar_id)]).await;

    p.ctx.clock().advance_secs(60);
    p.ctx.db.with_mut_snapshot(|s| { s.items.remove(&paste_id); });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    let _ = sys_handle.try_update_combat_state(target_ship_id, |c| { c.armor_hp = 100.0; });
    let armor_2 = sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap().armor_hp;
    let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(arar_id), PyRep::Integer(target_ship_id)]).await.unwrap();
    sys_handle.step_manual(12.0).await.unwrap();
    let armor_3 = sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap().armor_hp;
    let repaired_unloaded = armor_3 - armor_2;
    assert!(repaired_unloaded > 0.0, "Unloaded ARAR must fall back to base remote armor repair");
    assert!(
        repaired_loaded > repaired_unloaded * 2.5,
        "Loaded ARAR ({repaired_loaded}) must apply 3x paste repair bonus over unloaded ({repaired_unloaded})"
    );
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(arar_id)]).await;

    let ship_id = p.ship_id;
    let load_rep = rpc(
        &mut p,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(arar_id)]), PyRep::List(vec![PyRep::Integer(reload_paste_id)])],
    )
    .await
    .unwrap();
    let reload_ms = dict_get(&load_rep, "reloadTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((reload_ms - 60_000.0).abs() < 1e-3, "Ancillary remote repairer reload must be 60s (60000 ms), got {reload_ms}");
}
