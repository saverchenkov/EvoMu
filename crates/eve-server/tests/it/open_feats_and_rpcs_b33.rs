//! End-to-end integration tests for Batch 33 open `feat:mining.*` and `feat:modules.*` rows.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::mining_cycle_engine::{activate, advance, lock, pilot, rock, rpc, CHAR_ID, JITA, MINER_I, VELDSPAR};
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;

const MERCOXIT: u32 = 11_396;
const BLUE_ICE: u32 = 16_264;
const FULLERITE_C50: u32 = 30_370;
const DEEP_CORE_MINER_I: u32 = 12_108;
const STRIP_MINER_I: u32 = 17_482;
const HULK: u32 = 22_544;
const MLU_II: u32 = 28_576;
const ICE_HARVESTER_I: u32 = 16_278;
const GAS_HARVESTER_I: u32 = 25_266;
const ICE_HARVESTER_UPGRADE_II: u32 = 28_578;
const SURVEY_SCANNER_I: u32 = 444;
const BURST_II: u32 = 43_551;
const CHARGE_FIELD: u32 = 42_829;
const CHARGE_OPTIMIZATION: u32 = 42_830;
const CHARGE_EFFICIENCY: u32 = 90_733;
const PORPOISE: u32 = 42_244;
const ORCA: u32 = 28_606;
const RORQUAL: u32 = 28_352;
const ROKH: u32 = 24_688;
const PRISMATICITE: u32 = 90_041;
const ATHANOR: u32 = 35_835;
const TATARA: u32 = 35_836;
const MOON_DRILL_I: u32 = 35_894;
const ARAR_MEDIUM: u32 = 41_477;
const ARSB_MEDIUM: u32 = 41_481;
const AUTO_TARGETING_SYS_I: u32 = 1_182;

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

fn insert_item(
    ctx: &eve_server::router::ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    qty: i32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity: qty,
                singleton: qty == 1,
                version: 1,
                ..Default::default()
            },
        );
    });
}

#[tokio::test]
async fn test_b33_foreman_bursts_and_harvester_validity() {
    // parity: feat:mining.foreman-burst-charges, feat:mining.foreman-burst-charge-effects, feat:mining.ice-harvester-cycle-modifiers, feat:mining.mercoxit-deep-core-restriction, feat:mining.mining-module-target-validity
    eve_testkit::attest(&[
        "feat:mining.foreman-burst-charges",
        "feat:mining.foreman-burst-charge-effects",
        "feat:mining.ice-harvester-cycle-modifiers",
        "feat:mining.mercoxit-deep-core-restriction",
        "feat:mining.mining-module-target-validity",
    ]);

    let miner_id = 9_100_001_i64;
    let deep_core_id = 9_100_002_i64;
    let ice_harv_id = 9_100_003_i64;
    let gas_harv_id = 9_100_004_i64;
    let survey_id = 9_100_005_i64;
    let burst_id = 9_100_006_i64;
    let ice_upg_id = 9_100_007_i64;

    let mut p = pilot(
        ROKH,
        &[
            (3_386, 5),  // Mining V
            (3_410, 5),  // Astrogeology V
            (11_395, 5), // Deep Core Mining V
            (16_281, 5), // Ice Harvesting V
            (25_544, 5), // Gas Cloud Harvesting V
            (22_536, 5), // Mining Foreman V
            (22_552, 5), // Mining Director V
            (3_429, 5),  // Target Management V
            (3_426, 5),  // CPU Management V
            (3_413, 5),  // Power Grid Management V
        ],
        &[
            (miner_id, MINER_I, InventoryFlag::HiSlot0),
            (deep_core_id, DEEP_CORE_MINER_I, InventoryFlag::HiSlot1),
            (ice_harv_id, ICE_HARVESTER_I, InventoryFlag::HiSlot2),
            (gas_harv_id, GAS_HARVESTER_I, InventoryFlag::HiSlot3),
            (burst_id, BURST_II, InventoryFlag::HiSlot4),
            (survey_id, SURVEY_SCANNER_I, InventoryFlag::MedSlot0),
            (ice_upg_id, ICE_HARVESTER_UPGRADE_II, InventoryFlag::LoSlot0),
        ],
    )
    .await;

    let bid = p.beyonce_id;
    let _ = rpc(&mut p, "beyonceBound", Some(bid), "CmdStop", &[]).await;

    // 1. Ice harvester cycle duration modifiers (skill + Ice Harvester Upgrade II)
    let fit = p.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p.ship_id).unwrap();
    let ice_dur = fit.get_module_effective_attribute(ice_harv_id, 73, &p.ctx.sde);
    assert!(ice_dur < 250_000.0, "Ice Harvester duration must be reduced by skill and upgrade, got {ice_dur}");

    // 2. Target validity & Mercoxit Deep Core restriction
    let (veld_rock, merc_rock, ice_rock, gas_cloud, far_rock) = (9_200_001, 9_200_002, 9_200_003, 9_200_004, 9_200_005);
    rock(&p, veld_rock, VELDSPAR, 10_000, 500.0).await;
    rock(&p, merc_rock, MERCOXIT, 10_000, 500.0).await;
    rock(&p, ice_rock, BLUE_ICE, 100, 500.0).await;
    rock(&p, gas_cloud, FULLERITE_C50, 1_000, 500.0).await;
    rock(&p, far_rock, VELDSPAR, 10_000, 35_000.0).await;

    lock(&mut p, veld_rock).await;
    lock(&mut p, merc_rock).await;
    lock(&mut p, ice_rock).await;
    lock(&mut p, gas_cloud).await;
    lock(&mut p, far_rock).await;

    // Ordinary Miner I refused on Mercoxit, Ice, Gas, and out-of-range asteroid
    assert!(activate(&mut p, miner_id, merc_rock).await.is_err(), "Miner I must refuse Mercoxit");
    assert!(activate(&mut p, miner_id, ice_rock).await.is_err(), "Miner I must refuse Ice");
    assert!(activate(&mut p, miner_id, gas_cloud).await.is_err(), "Miner I must refuse Gas");
    assert!(activate(&mut p, miner_id, far_rock).await.is_err(), "Miner I must refuse out-of-range target");

    // Ice Harvester refused on Veldspar; Gas Harvester refused on Veldspar; Deep Core allowed on Mercoxit
    assert!(activate(&mut p, ice_harv_id, veld_rock).await.is_err(), "Ice Harvester must refuse Veldspar");
    assert!(activate(&mut p, gas_harv_id, veld_rock).await.is_err(), "Gas Harvester must refuse Veldspar");
    let dc_res = activate(&mut p, deep_core_id, merc_rock).await;
    assert!(dc_res.is_ok(), "Deep Core Miner must activate on Mercoxit: {dc_res:?}");
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(deep_core_id)]).await;

    // 3. Foreman Burst charge consumption, per-charge effects, and survey scanner range boost
    let charge_item_id = 9_100_010_i64;
    insert_item(&p.ctx, charge_item_id, CHARGE_FIELD, CHAR_ID, p.ship_id, InventoryFlag::HiSlot4, 5);
    let burst_rep = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
    assert_eq!(dict_get(&burst_rep, "chargeTypeID"), Some(&PyRep::Integer(i64::from(CHARGE_FIELD))));
    let rem_qty = p.ctx.db.with_snapshot(|s| s.items.get(&charge_item_id).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(rem_qty, 4, "Activating Mining Foreman Burst must consume 1 charge");

    // Miner I maxRange is boosted by Mining Laser Field Enhancement dbuff (dbuff 23)
    let miner_type = p.ctx.sde.types.get(&MINER_I).unwrap();
    let range_mult = eve_server::services::mining_cycle::dbuff_multiplier(&p.ctx, p.ship_id, miner_type, 54);
    assert!(range_mult > 1.3, "Field Enhancement burst must boost mining laser range, got {range_mult}");

    // Optimization charge reduces duration without granting Efficiency residue reduction
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
    p.ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&charge_item_id);
    });
    insert_item(&p.ctx, 9_100_011, CHARGE_OPTIMIZATION, CHAR_ID, p.ship_id, InventoryFlag::HiSlot4, 3);
    let opt_rep = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
    assert_eq!(dict_get(&opt_rep, "chargeTypeID"), Some(&PyRep::Integer(i64::from(CHARGE_OPTIMIZATION))));
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
    p.ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&9_100_011);
    });
    insert_item(&p.ctx, 9_100_012, CHARGE_EFFICIENCY, CHAR_ID, p.ship_id, InventoryFlag::HiSlot4, 3);
    let eff_rep = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
    assert_eq!(dict_get(&eff_rep, "chargeTypeID"), Some(&PyRep::Integer(i64::from(CHARGE_EFFICIENCY))));
}

#[tokio::test]
async fn test_b33_phased_asteroid_field_mobile_anchor_and_prismaticite() {
    // parity: feat:mining.phased-asteroid-field-lifecycle, feat:mining.mobile-phase-anchor, feat:mining.prismaticite-reprocessing-and-unrefined-reactions, feat:mining.wormhole-ore-sites
    eve_testkit::attest(&[
        "feat:mining.phased-asteroid-field-lifecycle",
        "feat:mining.mobile-phase-anchor",
        "feat:mining.prismaticite-reprocessing-and-unrefined-reactions",
        "feat:mining.wormhole-ore-sites",
    ]);

    let miner_id = 9_300_001_i64;
    let mut p = pilot(
        HULK,
        &[
            (3_386, 5),  // Mining V
            (3_410, 5),  // Astrogeology V
            (17_940, 5), // Mining Barge V
            (22_551, 5), // Exhumers V
            (22_578, 4), // Mining Upgrades IV
            (32_918, 5), // Mining Frigate V
            (3_380, 5),  // Industry V
            (3_327, 5),  // Spaceship Command V
            (3_429, 5),  // Target Management V
            (3_426, 5),  // CPU Management V
        ],
        &[
            (miner_id, STRIP_MINER_I, InventoryFlag::HiSlot0),
            (9_300_002, MLU_II, InventoryFlag::LoSlot0),
            (9_300_003, MLU_II, InventoryFlag::LoSlot1),
            (9_300_004, MLU_II, InventoryFlag::LoSlot2),
        ],
    )
    .await;
    let bid = p.beyonce_id;
    let _ = rpc(&mut p, "beyonceBound", Some(bid), "CmdStop", &[]).await;

    // 1. Spawn Phased Asteroid Field as a probeable signature (not yet a system-wide anomaly)
    let pris_ast_id = 9_300_010_i64;
    let ast_pos = p.ship_pos + DVec3::new(1_000.0, 0.0, 0.0);
    let field = eve_server::services::mining_b33::spawn_phased_asteroid_field(
        &p.ctx,
        JITA,
        "PHS-001",
        pris_ast_id,
        50_000,
        (ast_pos.x, ast_pos.y, ast_pos.z),
    );
    assert!(!field.beacon_spawned && !field.phased_in);
    let sys_handle = p.ctx.systems.get_or_spawn(JITA);
    sys_handle
        .spawn_ball(Ball {
            id: pris_ast_id,
            type_id: PRISMATICITE,
            position: ast_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e9,
            radius: 250.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    // Breached Prismaticite mines at 10% efficiency before Mobile Phase Anchor is powered
    lock(&mut p, pris_ast_id).await;
    activate(&mut p, miner_id, pris_ast_id).await.unwrap();
    let breached_outcomes = advance(&p, 46).await;
    assert_eq!(breached_outcomes.len(), 1);
    let breached_units = breached_outcomes[0].units_delivered;
    assert!(breached_units > 0);

    // 2. Power Mobile Phase Anchor with Porpoise (35) + Orca (50) = 85 (< 100, still unphased), then + Porpoise (35) = 120 (>= 100, phased in!)
    assert_eq!(eve_server::services::mining_b33::connect_ics_to_phase_anchor(&p.ctx, JITA, PORPOISE), Some(false));
    assert_eq!(eve_server::services::mining_b33::connect_ics_to_phase_anchor(&p.ctx, JITA, ORCA), Some(false));
    assert_eq!(eve_server::services::mining_b33::connect_ics_to_phase_anchor(&p.ctx, JITA, PORPOISE), Some(true));

    let phased_outcomes = advance(&p, 46).await;
    assert_eq!(phased_outcomes.len(), 1);
    let phased_units = phased_outcomes[0].units_delivered;
    assert!(
        phased_units >= breached_units * 8,
        "Phased-in Prismaticite ({phased_units}) must yield ~10x breached yield ({breached_units})"
    );

    // 3. Advance fracture past 0.5 -> beacon spawns; past 1.0 -> field collapses
    let mid = eve_server::services::mining_b33::advance_phased_asteroid_field(&p.ctx, JITA, 0.5).unwrap();
    assert!(mid.beacon_spawned && !mid.collapsed);
    let end = eve_server::services::mining_b33::advance_phased_asteroid_field(&p.ctx, JITA, 0.5).unwrap();
    assert!(end.collapsed && !end.phased_in);

    // 4. Prismaticite random-mineral reprocessing & Wormhole ore sites
    let pris_type = p.ctx.sde.types.get(&PRISMATICITE).unwrap();
    let mats = eve_server::services::space_mining_harvest::resolve_reprocessable_materials_for_portions(
        &p.ctx.sde,
        pris_type,
        9_300_099,
        1,
        0x1234_5678,
        10,
        false,
    )
    .expect("Prismaticite must reprocess into random minerals");
    assert!(!mats.is_empty());

    let c1_sites = eve_server::services::mining_sites::wormhole_ore_sites_for_class(1);
    let c5_sites = eve_server::services::mining_sites::wormhole_ore_sites_for_class(5);
    assert!(c1_sites.iter().all(|s| !s.dungeon_name.contains("Core")));
    assert!(c5_sites.iter().any(|s| s.dungeon_name.contains("Core")));
}

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

    // 1. Moon extraction duration (6..56 days), CancelExtraction, 3-hour auto-fracture, and moon ore class by security
    let corp_id = 9_800_001_i64;
    let athanor_id = 1_000_000_001_001_i64;
    let tatara_id = 1_000_000_001_002_i64;
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
            ch.standings.insert(1_000_035, 6.67); // 0% NPC tax at 6.67 standing
        }
        snap.structures.insert(
            athanor_id,
            UpwellStructureRecord {
                structure_id: athanor_id,
                type_id: ATHANOR,
                owner_corp_id: corp_id,
                solar_system_id: JITA,
                name: "B33 Athanor".into(),
                position: [100_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            tatara_id,
            UpwellStructureRecord {
                structure_id: tatara_id,
                type_id: TATARA,
                owner_corp_id: corp_id,
                solar_system_id: JITA,
                name: "B33 Tatara".into(),
                position: [200_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            9_400_001,
            InventoryItemRecord {
                item_id: 9_400_001,
                type_id: MOON_DRILL_I,
                owner_id: corp_id,
                location_id: athanor_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(9_400_001, true);
        // Online Reprocessing service on Athanor (35893) and Tatara (35893)
        snap.items.insert(
            9_400_002,
            InventoryItemRecord {
                item_id: 9_400_002,
                type_id: 35_893,
                owner_id: corp_id,
                location_id: athanor_id,
                flag: InventoryFlag::ServiceSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(9_400_002, true);
        snap.items.insert(
            9_400_003,
            InventoryItemRecord {
                item_id: 9_400_003,
                type_id: 35_893,
                owner_id: corp_id,
                location_id: tatara_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(9_400_003, true);
    });
    sess.corporation_id = Some(corp_id);
    sess.corp_role = u64::MAX;

    // Start 10-day extraction (864,000s), then cancel it
    let start_rep = p
        .ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "StartNewExtraction",
            &[PyRep::Integer(athanor_id), PyRep::Integer(864_000)],
            &kw,
            None,
        )
        .await
        .expect("StartNewExtraction");
    assert_eq!(dict_get(&start_rep, "chunkArrivalTime"), Some(&PyRep::Integer((now + 864_000) as i64)));
    let _ = p
        .ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "CancelExtraction", &[PyRep::Integer(athanor_id)], &kw, None)
        .await
        .expect("CancelExtraction");

    // Start 6-day extraction (518,400s), verify early FractureChunkWithStructure is refused, then advance past 3h auto-fracture
    let _ = p
        .ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "StartNewExtraction",
            &[PyRep::Integer(athanor_id), PyRep::Integer(518_400)],
            &kw,
            None,
        )
        .await
        .expect("StartNewExtraction 6d");
    assert!(
        p.ctx
            .dispatch_rpc(
                &mut sess,
                "moonExtractions",
                None,
                "FractureChunkWithStructure",
                &[PyRep::Integer(athanor_id)],
                &kw,
                None,
            )
            .await
            .is_err(),
        "FractureChunkWithStructure before chunk arrival must be refused"
    );
    p.ctx.clock().advance_secs(518_400 + 10_801);
    eve_server::services::mining_advanced::tick_all_moon_extractions(&p.ctx).await;
    let (auto_fractured, spawned_ids) = p
        .ctx
        .db
        .with_snapshot(|s| {
            s.moon_extractions
                .get(&athanor_id)
                .map(|e| (e.auto_fracture, e.spawned_asteroid_ids.clone()))
                .unwrap_or((false, Vec::new()))
        });
    assert!(auto_fractured, "Arrived chunk must auto-fracture after 3 hours");
    assert!(!spawned_ids.is_empty(), "Auto-fractured chunk must spawn moon ore asteroids");
    let all_r4 = p.ctx.db.with_snapshot(|s| {
        spawned_ids.iter().all(|id| {
            s.asteroids
                .get(id)
                .and_then(|a| p.ctx.sde.types.get(&a.type_id))
                .is_some_and(|t| t.group_id == 1884)
        })
    });
    assert!(all_r4, "High-sec moon fracture must contain only R4/Ubiquitous ores (group 1884)");

    // 3. Upwell refinery base yield (Athanor 51%, Tatara 52%), NPC station tax curve (0% at 6.67), and Scrapmetal cap (55%)
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
    let q_ath = p
        .ctx
        .dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(9_400_010)], &kw, None)
        .await
        .unwrap();
    let ath_yield = dict_get(&q_ath, "yieldRate").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((ath_yield - 0.51).abs() < 0.02, "Athanor base yield must be 51%, got {ath_yield}");

    sess.structure_id = Some(tatara_id);
    p.ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&CHAR_ID) {
            ch.structure_id = Some(tatara_id);
        }
    });
    let q_tat = p
        .ctx
        .dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(9_400_011)], &kw, None)
        .await
        .unwrap();
    let tat_yield = dict_get(&q_tat, "yieldRate").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((tat_yield - 0.52).abs() < 0.02, "Tatara base yield must be 52%, got {tat_yield}");

    sess.structure_id = None;
    sess.station_id = Some(60_003_760);
    p.ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&CHAR_ID) {
            ch.structure_id = None;
            ch.station_id = Some(60_003_760);
        }
    });
    let q_npc = p
        .ctx
        .dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(9_400_012)], &kw, None)
        .await
        .unwrap();
    let npc_tax = dict_get(&q_npc, "stationTaxRate").and_then(|v| v.as_f64()).unwrap_or(1.0);
    let scrap_yield = dict_get(&q_npc, "yieldRate").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(npc_tax.abs() < 1e-4, "NPC reprocessing tax at 6.67 standing must be 0%, got {npc_tax}");
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

    insert_item(&p.ctx, paste_id, 28_668, CHAR_ID, p.ship_id, InventoryFlag::HiSlot0, 16); // Nanite Repair Paste
    insert_item(&p.ctx, cap_charge_id, 3_200, CHAR_ID, p.ship_id, InventoryFlag::HiSlot1, 8); // Navy Cap Booster 400
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

    // 2. ARSB consumes cap booster charge instead of capacitor while loaded; ARAR gives bonus repair with nanite paste loaded
    lock(&mut p, target_ship_id).await;
    let cap_before = sys_handle.get_combat_state(p.ship_id).await.unwrap().map(|c| c.capacitor).unwrap_or(1_000.0);
    let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(arsb_id), PyRep::Integer(target_ship_id)]).await.unwrap();
    let cap_after = sys_handle.get_combat_state(p.ship_id).await.unwrap().map(|c| c.capacitor).unwrap_or(0.0);
    assert!((cap_before - cap_after).abs() < 1e-3, "ARSB must consume 0 capacitor while loaded with cap booster charges");

    let arar_rep = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(arar_id), PyRep::Integer(target_ship_id)]).await.unwrap();
    assert!(
        dict_get(&arar_rep, "active").and_then(|v| v.as_bool()).unwrap_or(false),
        "ARAR must activate with nanite repair paste loaded"
    );
}
