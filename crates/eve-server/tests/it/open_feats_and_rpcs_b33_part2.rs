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
const ASTRAHUS: u32 = 35_832;
const RAITARU: u32 = 35_825;
const MOON_DRILL_I: u32 = 35_894;
const MOON_STABILITY_RIG_I: u32 = 46_325;
const REPROC_MONITOR_RIG_II: u32 = 46_640;
const RX_804_IMPLANT: u32 = 27_174;
const ARAR_MEDIUM: u32 = 41_477;
const ARSB_MEDIUM: u32 = 41_481;
const AUTO_TARGETING_SYS_I: u32 = 1_182;
const AUTO_TARGETING_SYS_II: u32 = 1_436;
const ANGEL_ROGUE_NPC: u32 = 2_372;
const NULL_SYS: u32 = 30_004_759; // 1DQ1-A
const WH_SYS: u32 = 31_000_005;

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

    let p = pilot(PORPOISE, &[(12_196, 5), (3_359, 5)], &[]).await;
    let kw = HashMap::new();
    let mut sess = p.sess.clone();

    let low_sys = p.ctx.sde.solar_systems.values()
        .find(|s| s.solar_system_id < 31_000_000 && s.security_status > 0.05 && s.security_status < 0.45
            && p.ctx.sde.celestials_in_system(s.solar_system_id).iter().any(|c| c.group_id == 8))
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_510);
    let wh_sys = p.ctx.sde.solar_systems.values()
        .find(|s| (31_000_000..=32_999_999).contains(&s.solar_system_id)
            && p.ctx.sde.celestials_in_system(s.solar_system_id).iter().any(|c| c.group_id == 8))
        .map(|s| s.solar_system_id)
        .unwrap_or(WH_SYS);
    let low_npc_station = p.ctx.sde.solar_systems.values()
        .flat_map(|s| s.stations.iter())
        .find(|st| st.reprocessing_efficiency >= 0.30 && st.reprocessing_efficiency < 0.49)
        .map(|st| (st.station_id, st.reprocessing_efficiency))
        .expect("NPC station with < 50% reprocessing efficiency");

    let corp_id = 9_800_001_i64;
    let (athanor_id, tatara_id, null_athanor_id, low_athanor_id, wh_athanor_id, raitaru_id, astrahus_id, null_tatara_id) = (
        1_000_000_001_001_i64, 1_000_000_001_002_i64, 1_000_000_001_003_i64, 1_000_000_001_004_i64,
        1_000_000_001_005_i64, 1_000_000_001_006_i64, 1_000_000_001_007_i64, 1_000_000_001_008_i64,
    );
    let now = p.ctx.now_unix_secs();
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(corp_id, CorporationRecord {
            corporation_id: corp_id, name: "B33 Mining Corp".into(), ticker: "B33M".into(), ceo_id: CHAR_ID, ..Default::default()
        });
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) { ch.corporation_id = corp_id; }
        for (sid, tid, sys_id, pos) in [
            (athanor_id, ATHANOR, JITA, [100_000.0, 0.0, 0.0]),
            (tatara_id, TATARA, JITA, [200_000.0, 0.0, 0.0]),
            (null_athanor_id, ATHANOR, NULL_SYS, [300_000.0, 0.0, 0.0]),
            (low_athanor_id, ATHANOR, low_sys, [400_000.0, 0.0, 0.0]),
            (wh_athanor_id, ATHANOR, wh_sys, [500_000.0, 0.0, 0.0]),
            (raitaru_id, RAITARU, JITA, [600_000.0, 0.0, 0.0]),
            (astrahus_id, ASTRAHUS, JITA, [700_000.0, 0.0, 0.0]),
            (null_tatara_id, TATARA, NULL_SYS, [800_000.0, 0.0, 0.0]),
        ] {
            snap.structures.insert(sid, UpwellStructureRecord {
                structure_id: sid, type_id: tid, owner_corp_id: corp_id, solar_system_id: sys_id,
                name: format!("B33 Refinery {sid}"), position: pos, state: "ShieldVulnerable".into(),
                quantum_core_installed: true, fuel_blocks: 1_000, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
                ..Default::default()
            });
        }
        for (iid, tid, loc, flag) in [
            (9_400_001, MOON_DRILL_I, athanor_id, InventoryFlag::ServiceSlot0),
            (9_400_002, 35_899, athanor_id, InventoryFlag::ServiceSlot1),
            (9_400_003, 35_899, tatara_id, InventoryFlag::ServiceSlot0),
            (9_400_004, MOON_DRILL_I, null_athanor_id, InventoryFlag::ServiceSlot0),
            (9_400_005, MOON_DRILL_I, low_athanor_id, InventoryFlag::ServiceSlot0),
            (9_400_006, MOON_DRILL_I, wh_athanor_id, InventoryFlag::ServiceSlot0),
            (9_400_007, MOON_DRILL_I, tatara_id, InventoryFlag::ServiceSlot1),
            (9_400_008, MOON_STABILITY_RIG_I, tatara_id, InventoryFlag::RigSlot0),
            (9_400_020, 35_899, raitaru_id, InventoryFlag::ServiceSlot0),
            (9_400_021, 35_899, astrahus_id, InventoryFlag::ServiceSlot0),
            (9_400_022, 35_899, null_tatara_id, InventoryFlag::ServiceSlot0),
            (9_400_023, REPROC_MONITOR_RIG_II, null_tatara_id, InventoryFlag::RigSlot0),
        ] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: corp_id, location_id: loc, flag, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
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

    // 2. 6-day extraction (4,320,000 m³), early fracture refused, 3h base vs 3.6h rigged auto-fracture, security-band & jackpot ores
    for sid in [athanor_id, tatara_id, null_athanor_id, low_athanor_id, wh_athanor_id] {
        p.ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(sid), PyRep::Integer(518_400)], &kw, None).await.expect("StartNewExtraction 6d");
    }
    assert!(
        p.ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(athanor_id)], &kw, None).await.is_err(),
        "FractureChunkWithStructure before chunk arrival must be refused"
    );
    p.ctx.clock().advance_secs(518_400 + 10_801);
    eve_server::services::mining_advanced::tick_all_moon_extractions(&p.ctx).await;
    let rigged_fractured_at_3h = p.ctx.db.with_snapshot(|s| s.moon_extractions.get(&tatara_id).map(|e| e.auto_fracture).unwrap_or(false));
    assert!(!rigged_fractured_at_3h, "Rigged refinery (Moon Drilling Stability I +20%) must not auto-fracture at 3h");

    p.ctx.clock().advance_secs(2_161);
    eve_server::services::mining_advanced::tick_all_moon_extractions(&p.ctx).await;
    let (auto_fractured, rigged_fractured, spawned_ids, chunk_vol_m3, null_ids, low_ids, wh_ids) = p.ctx.db.with_snapshot(|s| {
        let ext = s.moon_extractions.get(&athanor_id).unwrap();
        let vol: f64 = ext.spawned_asteroid_ids.iter().filter_map(|id| s.asteroids.get(id))
            .map(|a| (a.quantity as f64) * p.ctx.sde.types.get(&a.type_id).map(|t| t.volume).unwrap_or(0.0)).sum();
        let ids_of = |k: i64| s.moon_extractions.get(&k).map(|e| e.spawned_asteroid_ids.clone()).unwrap_or_default();
        (ext.auto_fracture, s.moon_extractions.get(&tatara_id).unwrap().auto_fracture, ext.spawned_asteroid_ids.clone(), vol, ids_of(null_athanor_id), ids_of(low_athanor_id), ids_of(wh_athanor_id))
    });
    assert!(auto_fractured && rigged_fractured && !spawned_ids.is_empty());
    assert!((chunk_vol_m3 - 4_320_000.0).abs() < 1_000.0, "6-day chunk volume must be 4,320,000 m³, got {chunk_vol_m3}");
    let only_r4 = |ids: &[i64]| p.ctx.db.with_snapshot(|s| ids.iter().all(|id| s.asteroids.get(id).and_then(|a| p.ctx.sde.types.get(&a.type_id)).is_some_and(|t| t.group_id == 1884)));
    assert!(only_r4(&spawned_ids), "High-sec moon must contain only R4 ores");
    assert!(!wh_ids.is_empty() && only_r4(&wh_ids), "Wormhole moon must contain only R4 ores");

    let (null_valid_groups, low_valid_groups, has_improved) = p.ctx.db.with_snapshot(|s| {
        let in_r4_r64 = |id: &i64| s.asteroids.get(id).and_then(|a| p.ctx.sde.types.get(&a.type_id)).is_some_and(|t| matches!(t.group_id, 1884 | 1920 | 1921 | 1922 | 1923));
        let imp = null_ids.iter().any(|id| s.asteroids.get(id).and_then(|a| p.ctx.sde.types.get(&a.type_id)).is_some_and(|t| t.default_attributes.contains_key(&2699) || t.name.starts_with("Bountiful ") || t.name.starts_with("Glowing ") || t.name.starts_with("Twinkling ") || t.name.starts_with("Scintillating ") || t.name.starts_with("Sparkling ")));
        (null_ids.iter().all(in_r4_r64), low_ids.iter().all(in_r4_r64), imp)
    });
    assert!(null_valid_groups && !null_ids.is_empty() && low_valid_groups && !low_ids.is_empty(), "Low-sec and null-sec moons must contain R4-R64 ores");
    assert!(has_improved, "Fractured moon field must mix basic and improved (+15%) quality ores");

    // Jackpot fracture check
    let mut hit_jackpot = false;
    for _ in 0..40 {
        p.ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(null_athanor_id), PyRep::Integer(518_400)], &kw, None).await.unwrap();
        p.ctx.clock().advance_secs(518_401);
        let frac = p.ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[PyRep::Integer(null_athanor_id)], &kw, None).await.unwrap();
        if dict_get(&frac, "isJackpot") == Some(&PyRep::Bool(true)) {
            assert_eq!(dict_get(&frac, "jackpotBonus"), Some(&PyRep::Real(2.0)));
            hit_jackpot = true;
            break;
        }
    }
    assert!(hit_jackpot, "Moon fracture must produce jackpot (+100%) ore variant when jackpot roll hits");

    // 3. Upwell refinery base yield via GetQuotes & Reprocess: Citadel/EC 50%, Athanor 51%, Tatara 52% (+1.5% = 52.75%)
    for (idx, (st_id, exp_base, exp_trit)) in [
        (raitaru_id, 0.50, 200),
        (astrahus_id, 0.50, 200),
        (athanor_id, 0.51, 204),
        (tatara_id, 0.5275, 211),
    ].into_iter().enumerate() {
        let item_id = 9_400_030 + idx as i64;
        insert_item(&p.ctx, item_id, VELDSPAR, CHAR_ID, st_id, InventoryFlag::Hangar, 100);
        sess.station_id = None;
        sess.structure_id = Some(st_id);
        p.ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&CHAR_ID) { ch.station_id = None; ch.structure_id = Some(st_id); }
        });
        let quote = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
        let q_base = dict_get(&quote, "stationBaseYield").and_then(|v| v.as_f64()).unwrap_or(0.0);
        assert!((q_base - exp_base).abs() < 1e-4, "GetQuotes stationBaseYield at {st_id} expected {exp_base}, got {q_base}");
        let rep = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(item_id)], &kw, None).await.unwrap();
        let r_yield = dict_get(&rep, "yieldRate").and_then(|v| v.as_f64()).unwrap_or(0.0);
        assert!((r_yield - exp_base).abs() < 1e-4, "Reprocess yieldRate at {st_id} expected {exp_base}, got {r_yield}");
        let trit: i32 = p.ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == st_id && i.type_id == 34).map(|i| i.quantity).sum());
        assert_eq!(trit, exp_trit, "100 Veldspar at {st_id} must yield {exp_trit} Tritanium");
    }

    // 4. NPC station equipment (< 50% & 50%) via GetQuotes, tax curve (5% at 0, 2.5% at 3.335, 0% at 6.67, max(personal, corp)), and Connections 0-vs-1 mission gate
    sess.structure_id = None;
    sess.station_id = Some(low_npc_station.0);
    p.ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&CHAR_ID) { ch.structure_id = None; ch.station_id = Some(low_npc_station.0); }
    });
    let low_q = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
    let low_base = dict_get(&low_q, "stationBaseYield").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((low_base - low_npc_station.1).abs() < 1e-4 && low_base < 0.50, "NPC station <50% base yield must match SDE: got {low_base}, expected {}", low_npc_station.1);

    sess.station_id = Some(60_003_760);
    p.ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&CHAR_ID) { ch.structure_id = None; ch.station_id = Some(60_003_760); }
        s.characters.get_mut(&CHAR_ID).unwrap().standings.insert(1_000_035, 0.0);
    });
    let q0 = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
    assert!((dict_get(&q0, "stationTaxRate").and_then(|v| v.as_f64()).unwrap() - 0.05).abs() < 1e-4);

    p.ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&CHAR_ID).unwrap().standings.insert(1_000_035, 3.335); });
    let q_mid = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
    assert!((dict_get(&q_mid, "stationTaxRate").and_then(|v| v.as_f64()).unwrap() - 0.025).abs() < 1e-3);

    // Corp standing via real corp members' personal standings (0.0 + 10.0 + 10.0 => avg 6.67 => 0% tax)
    p.ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&CHAR_ID).unwrap().standings.insert(1_000_035, 0.0);
        for m_id in [9_800_101_i64, 9_800_102_i64] {
            let mut m = s.characters.get(&CHAR_ID).unwrap().clone();
            m.character_id = m_id;
            m.corporation_id = corp_id;
            m.standings.insert(1_000_035, 10.0);
            s.characters.insert(m_id, m);
        }
    });
    let q_corp = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
    assert!(dict_get(&q_corp, "stationTaxRate").and_then(|v| v.as_f64()).unwrap().abs() < 1e-4);

    // Connections V applies ONLY after >= 1 completed mission for that corp
    let agent_id = p.ctx.sde.agents.values().find(|a| a.corporation_id == 1_000_035).map(|a| a.agent_id).unwrap_or(3_008_416);
    p.ctx.db.with_mut_snapshot(|s| {
        s.characters.remove(&9_800_101);
        s.characters.remove(&9_800_102);
        s.characters.get_mut(&CHAR_ID).unwrap().standings.insert(1_000_035, 5.0);
    });
    let q_no_mission = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
    let tax_no_mission = dict_get(&q_no_mission, "stationTaxRate").and_then(|v| v.as_f64()).unwrap();
    assert!((tax_no_mission - 0.0125).abs() < 1e-4, "Connections must not apply with 0 completed corp missions, got {tax_no_mission}");

    p.ctx.db.with_mut_snapshot(|s| {
        s.active_missions.insert((CHAR_ID, agent_id as u32), eve_db::ActiveMissionRecord {
            mission_id: 9_900_101, character_id: CHAR_ID, agent_id: i64::from(agent_id),
            solar_system_id: JITA, state: "Completed".into(), current_room_id: 1, npcs_remaining: 0,
            spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
        });
    });
    let q_with_mission = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
    let tax_with_mission = dict_get(&q_with_mission, "stationTaxRate").and_then(|v| v.as_f64()).unwrap();
    assert!((tax_with_mission - 0.005).abs() < 1e-4, "Connections V must apply after >=1 completed corp mission, got {tax_with_mission}");

    // 5. Scrapmetal yield exclusions: ignores Upwell structure, rig, security-band, and implant bonuses (capped at 55%)
    insert_item(&p.ctx, 9_400_012, MINER_I, CHAR_ID, 60_003_760, InventoryFlag::Hangar, 2);
    let r_scrap_npc = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(9_400_012)], &kw, None).await.unwrap();
    assert!((dict_get(&r_scrap_npc, "yieldRate").and_then(|v| v.as_f64()).unwrap_or(0.0) - 0.55).abs() < 1e-4);

    insert_item(&p.ctx, 9_400_013, MINER_I, CHAR_ID, null_tatara_id, InventoryFlag::Hangar, 2);
    sess.station_id = None;
    sess.structure_id = Some(null_tatara_id);
    p.ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&CHAR_ID) { ch.station_id = None; ch.structure_id = Some(null_tatara_id); }
        s.active_implants.entry(CHAR_ID).or_default().push(RX_804_IMPLANT);
    });
    let r_scrap_tatara = p.ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(9_400_013)], &kw, None).await.unwrap();
    assert!(
        (dict_get(&r_scrap_tatara, "yieldRate").and_then(|v| v.as_f64()).unwrap_or(0.0) - 0.55).abs() < 1e-4,
        "Scrapmetal reprocessing in rigged null-sec Tatara with RX-804 implant must still cap at 55%"
    );
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
            (3_429, 5), // Long Range Targeting V
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

    // 1. Auto Targeting System I (+2) & II (+3) passive maxLockedTargets; active auto-lock ignores neutral Jita players and locks hostile targets
    let fit = p.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p.ship_id).unwrap();
    let max_targets = fit.ship.get_attribute(192, &p.ctx.sde);
    assert!((max_targets - 9.0).abs() < 1e-4, "Auto Targeting System I must passively add +2 maxLockedTargets (expected 9, got {max_targets})");

    let ats2_id = 9_500_007_i64;
    insert_item(&p.ctx, ats2_id, AUTO_TARGETING_SYS_II, CHAR_ID, p.ship_id, InventoryFlag::HiSlot3, 1);
    p.ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(ats_id, false);
        s.module_online_states.insert(ats2_id, true);
    });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    let fit2 = p.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p.ship_id).unwrap();
    let max_targets_2 = fit2.ship.get_attribute(192, &p.ctx.sde);
    assert!((max_targets_2 - 10.0).abs() < 1e-4, "Auto Targeting System II must passively add +3 maxLockedTargets (expected 10, got {max_targets_2})");
    p.ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&ats2_id);
        s.module_online_states.insert(ats_id, true);
    });
    p.ctx.invalidate_ship_fitting(p.ship_id);

    let neutral_ship_id = 9_500_099_i64;
    let hostile_npc_id = 9_500_100_i64;
    p.ctx.db.with_mut_snapshot(|s| {
        s.items.insert(neutral_ship_id, InventoryItemRecord {
            item_id: neutral_ship_id, type_id: PORPOISE, owner_id: 9_500_999, location_id: i64::from(JITA),
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });
    let sys_handle = p.ctx.systems.get_or_spawn(JITA);
    sys_handle.spawn_ball(Ball {
        id: neutral_ship_id, type_id: PORPOISE, position: p.ship_pos + DVec3::new(5_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();

    let ats_neutral = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(ats_id)]).await.unwrap();
    assert_eq!(
        dict_get(&ats_neutral, "lockingCount").and_then(|v| v.as_i64()).unwrap_or(-1),
        0,
        "Auto Targeting System must NOT auto-lock unaggressing neutral player ships in high-sec Jita"
    );
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ats_id)]).await;

    sys_handle.spawn_ball(Ball {
        id: hostile_npc_id, type_id: ANGEL_ROGUE_NPC, position: p.ship_pos + DVec3::new(6_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    let ats_rep = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(ats_id)]).await.unwrap();
    assert_eq!(
        dict_get(&ats_rep, "maxLockedTargets").and_then(|v| v.as_i64()).unwrap_or(0),
        6,
        "Auto Targeting System I must obey Target Management IV skill cap (2 + 4 = 6, no double-counted +2)"
    );
    assert_eq!(
        dict_get(&ats_rep, "lockingCount").and_then(|v| v.as_i64()).unwrap_or(0),
        1,
        "Active Auto Targeting System I must automatically initiate target locks on hostile targets"
    );

    // Repeating cycle tick must auto-lock a hostile NPC arriving after activation while still ignoring neutral ship
    let late_npc_id = 9_500_101_i64;
    sys_handle.spawn_ball_with_meta(
        Ball {
            id: late_npc_id, type_id: ANGEL_ROGUE_NPC, position: p.ship_pos + DVec3::new(7_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta { group_id: 550, category_id: 11, owner_id: 500_010, ..Default::default() },
    ).await.unwrap();
    sys_handle.step_manual(11.0).await.unwrap();
    let cycle_locks = sys_handle.get_target_locks(p.ship_id).await.unwrap();
    assert!(cycle_locks.iter().any(|l| l.target_id == late_npc_id), "Repeating Auto Targeting System cycle must lock late-arriving hostile NPC");
    assert!(!cycle_locks.iter().any(|l| l.target_id == neutral_ship_id), "Repeating Auto Targeting System cycle must not lock neutral player ship");

    // 2. ARSB consumes cap booster charge instead of capacitor while loaded, falls back to capacitor when unloaded, and reloads in 60s
    let target_ship_id = neutral_ship_id;
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

    let reload_cap_id = 9_500_008_i64;
    insert_item(&p.ctx, reload_cap_id, 11_283, CHAR_ID, p.ship_id, InventoryFlag::Cargo, 4);
    let ship_id = p.ship_id;
    for (mod_id, ammo_id) in [(arar_id, reload_paste_id), (arsb_id, reload_cap_id)] {
        let load_rep = rpc(
            &mut p,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_id)]), PyRep::List(vec![PyRep::Integer(ammo_id)])],
        )
        .await
        .unwrap();
        let reload_ms = dict_get(&load_rep, "reloadTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0);
        assert!((reload_ms - 60_000.0).abs() < 1e-3, "Ancillary remote repairer {mod_id} reload must be 60s (60000 ms), got {reload_ms}");
    }
}
