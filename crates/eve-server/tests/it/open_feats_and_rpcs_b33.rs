//! End-to-end integration tests for Batch 33 open `feat:mining.*` rows (part 1: Foreman bursts,
//! harvester validity, phased asteroid field lifecycle, Mobile Phase Anchor, Prismaticite &
//! Unrefined Mineral reactions, and wormhole ore sites).

#![forbid(unsafe_code)]

use std::collections::HashSet;

use crate::mining_cycle_engine::{
    activate, advance, lock, pilot, rock, rpc, CHAR_ID, JITA, MINER_I, VELDSPAR,
};
use eve_db::{CorporationRecord, FleetRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
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
const BURST_II: u32 = 43_551;
const CHARGE_FIELD: u32 = 42_829;
const CHARGE_OPTIMIZATION: u32 = 42_830;
const CHARGE_EFFICIENCY: u32 = 90_733;
const PORPOISE: u32 = 42_244;
const ORCA: u32 = 28_606;
const ROKH: u32 = 24_688;
const PRISMATICITE: u32 = 90_041;
const UNREFINED_TRITANIUM: u32 = 90_283;
const UNREFINED_TRIT_FORMULA: u32 = 90_274;
const STANDUP_COMPOSITE_REACTOR: u32 = 45_537;
const ATHANOR: u32 = 35_835;
const MOBILE_PHASE_ANCHOR: u32 = 90_037;
const NULL_SYS: u32 = 30_004_759; // 1DQ1-A

pub fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

pub fn insert_item(
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
    let burst_id = 9_100_006_i64;
    let ice_upg_1 = 9_100_007_i64;

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
            (33_399, 5), // Industrial Command Ships V
            (3_429, 5),  // Target Management V
            (3_426, 5),  // CPU Management V
            (3_413, 5),  // Power Grid Management V
        ],
        &[
            (miner_id, MINER_I, InventoryFlag::HiSlot0),
            (deep_core_id, DEEP_CORE_MINER_I, InventoryFlag::HiSlot1),
            (gas_harv_id, GAS_HARVESTER_I, InventoryFlag::HiSlot2),
            (burst_id, BURST_II, InventoryFlag::HiSlot3),
        ],
    )
    .await;

    let bid = p.beyonce_id;
    let _ = rpc(&mut p, "beyonceBound", Some(bid), "CmdStop", &[]).await;

    // 1. Ice harvester cycle duration modifiers (skill + Ice Harvester Upgrade II, 1 block per cycle)
    let mut p_ice = pilot(
        HULK,
        &[
            (3_386, 5),  // Mining V
            (16_281, 5), // Ice Harvesting V
            (17_940, 5), // Mining Barge V
            (22_551, 5), // Exhumers V
            (3_429, 5),  // Target Management V
            (3_426, 5),  // CPU Management V
            (3_413, 5),  // Power Grid Management V
        ],
        &[
            (ice_harv_id, ICE_HARVESTER_I, InventoryFlag::HiSlot0),
            (ice_upg_1, ICE_HARVESTER_UPGRADE_II, InventoryFlag::LoSlot0),
        ],
    )
    .await;
    p_ice.ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(ice_harv_id, true);
        s.module_online_states.insert(ice_upg_1, true);
    });
    p_ice.ctx.invalidate_ship_fitting(p_ice.ship_id);
    let fit_ice = p_ice.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p_ice.ship_id).unwrap();
    let base_ice_dur = p_ice.ctx.sde.types.get(&ICE_HARVESTER_I).unwrap().default_attributes[&73];
    let ice_dur = fit_ice.get_module_effective_attribute(ice_harv_id, 73, &p_ice.ctx.sde);
    assert!(
        ice_dur < base_ice_dur * 0.80,
        "Ice Harvester duration ({ice_dur}) must be reduced by skill and upgrade from {base_ice_dur}"
    );
    let ice_rock_id = 9_200_099_i64;
    rock(&p_ice, ice_rock_id, BLUE_ICE, 100, 500.0).await;
    lock(&mut p_ice, ice_rock_id).await;
    activate(&mut p_ice, ice_harv_id, ice_rock_id).await.unwrap();
    let ice_out = advance(&p_ice, 200).await;
    assert!(!ice_out.is_empty());
    assert!(ice_out.iter().all(|o| o.units_delivered == 1), "Each ice harvester cycle must yield 1 block");

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

    // Ordinary Miner I refused on Mercoxit, Ice, Gas, and out-of-range
    let e_merc_miner = activate(&mut p, miner_id, merc_rock).await.unwrap_err();
    assert!(!e_merc_miner.is_empty());
    assert!(activate(&mut p, miner_id, ice_rock).await.is_err());
    assert!(activate(&mut p, miner_id, gas_cloud).await.is_err());
    assert!(activate(&mut p, miner_id, far_rock).await.is_err());
    assert!(activate(&mut p, gas_harv_id, veld_rock).await.is_err());

    // Deep Core allowed on Mercoxit
    activate(&mut p, deep_core_id, merc_rock).await.unwrap();
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(deep_core_id)]).await;

    // 3. Fleet setup: in-range member (5 km) and out-of-range member (120 km)
    let in_range_cid = 9_210_001_i64;
    let in_range_sid = 9_210_002_i64;
    let out_range_cid = 9_210_003_i64;
    let out_range_sid = 9_210_004_i64;
    p.ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.get(&CHAR_ID).unwrap().clone();
        for (cid, sid) in [(in_range_cid, in_range_sid), (out_range_cid, out_range_sid)] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: HULK,
                    owner_id: cid,
                    location_id: i64::from(JITA),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.fleets.insert(
            77_001,
            FleetRecord {
                fleet_id: 77_001,
                boss_id: CHAR_ID,
                members: vec![CHAR_ID, in_range_cid, out_range_cid],
                ..Default::default()
            },
        );
    });
    let sys_handle = p.ctx.systems.get_or_spawn(JITA);
    for (sid, offset_x) in [(in_range_sid, 5_000.0), (out_range_sid, 120_000.0)] {
        sys_handle
            .spawn_ball(Ball {
                id: sid,
                type_id: HULK,
                position: p.ship_pos + DVec3::new(offset_x, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 150.0,
                inertia: 1.0,
                mass: 1e7,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await
            .unwrap();
    }

    let miner_type = p.ctx.sde.types.get(&MINER_I).unwrap().clone();
    let dbuff = |ctx: &eve_server::router::ServerContext, sid: i64, attr: u16| {
        eve_server::services::mining_cycle::dbuff_multiplier(ctx, sid, &miner_type, attr)
    };

    // 4a. Field Enhancement charge: boosts range (54), does NOT alter duration (73), cap (6), or waste (3154); AoE hits in-range only
    let charge_item_id = 9_100_010_i64;
    insert_item(&p.ctx, charge_item_id, CHARGE_FIELD, CHAR_ID, p.ship_id, InventoryFlag::HiSlot3, 5);
    let burst_rep = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
    assert_eq!(dict_get(&burst_rep, "chargeTypeID"), Some(&PyRep::Integer(i64::from(CHARGE_FIELD))));
    let buff_dur = dict_get(&burst_rep, "buffDurationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((buff_dur - 90.0).abs() < 1e-4, "Mining Foreman V must scale burst duration to 90s, got {buff_dur}");
    let rem_qty = p.ctx.db.with_snapshot(|s| s.items.get(&charge_item_id).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(rem_qty, 4, "Activating Mining Foreman Burst must consume 1 charge");

    assert!(dbuff(&p.ctx, in_range_sid, 54) > 1.3, "In-range fleet member must receive Field Enhancement range boost");
    assert!((dbuff(&p.ctx, out_range_sid, 54) - 1.0).abs() < 1e-6, "Out-of-range ship must not receive burst");
    assert!((dbuff(&p.ctx, in_range_sid, 73) - 1.0).abs() < 1e-6, "Field charge must not grant duration reduction");
    assert!((dbuff(&p.ctx, in_range_sid, 6) - 1.0).abs() < 1e-6, "Field charge must not grant capacitor reduction");
    assert!((dbuff(&p.ctx, in_range_sid, 3154) - 1.0).abs() < 1e-6, "Field charge must not grant residue reduction");

    // 4b. Optimization charge: reduces duration (73) & capacitorNeed (6) by >= 15%, does NOT grant range or residue
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
    p.ctx.db.with_mut_snapshot(|s| { s.items.remove(&charge_item_id); });
    p.ctx.clock().advance_secs(100);
    insert_item(&p.ctx, 9_100_011, CHARGE_OPTIMIZATION, CHAR_ID, p.ship_id, InventoryFlag::HiSlot3, 3);
    let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
    assert!(dbuff(&p.ctx, in_range_sid, 73) < 0.80, "Optimization charge must reduce duration >= 15%, got {}", dbuff(&p.ctx, in_range_sid, 73));
    assert!(dbuff(&p.ctx, in_range_sid, 6) < 0.80, "Optimization charge must reduce cap use >= 15%, got {}", dbuff(&p.ctx, in_range_sid, 6));
    assert!((dbuff(&p.ctx, in_range_sid, 54) - 1.0).abs() < 1e-6, "Optimization charge must not grant range boost");
    assert!((dbuff(&p.ctx, in_range_sid, 3154) - 1.0).abs() < 1e-6, "Optimization charge must not grant residue reduction");

    // 4c. Efficiency charge: boosts crit chance (5967) and lowers residue probability (3154), does NOT grant range or duration
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
    p.ctx.db.with_mut_snapshot(|s| { s.items.remove(&9_100_011); });
    p.ctx.clock().advance_secs(100);
    insert_item(&p.ctx, 9_100_012, CHARGE_EFFICIENCY, CHAR_ID, p.ship_id, InventoryFlag::HiSlot3, 3);
    let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
    assert!(dbuff(&p.ctx, in_range_sid, 5967) > 1.5, "Efficiency charge must boost crit chance, got {}", dbuff(&p.ctx, in_range_sid, 5967));
    assert!(dbuff(&p.ctx, in_range_sid, 3154) < 0.80, "Efficiency charge must reduce residue probability, got {}", dbuff(&p.ctx, in_range_sid, 3154));
    assert!((dbuff(&p.ctx, in_range_sid, 54) - 1.0).abs() < 1e-6, "Efficiency charge must not grant range boost");
    assert!((dbuff(&p.ctx, in_range_sid, 73) - 1.0).abs() < 1e-6, "Efficiency charge must not grant duration reduction");
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
            (45_746, 5), // Reactions V
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

    // 1. High-sec Jita refuses Phased Asteroid Field; null-sec 1DQ1-A spawns probeable signature
    assert!(
        eve_server::services::mining_b33::spawn_phased_asteroid_field(
            &p.ctx, JITA, "PHS-HI", 9_300_009, 10_000, (0.0, 0.0, 0.0),
        )
        .is_none(),
        "Phased Asteroid Field must not spawn in high-sec"
    );

    let ast_pos = DVec3::new(250_000.0, 12_000.0, -180_000.0);
    p.ship_pos = ast_pos - DVec3::new(1_000.0, 0.0, 0.0);
    p.sess.solar_system_id = Some(NULL_SYS);
    p.ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.solar_system_id = NULL_SYS;
        }
        if let Some(sh) = snap.items.get_mut(&p.ship_id) {
            sh.location_id = i64::from(NULL_SYS);
        }
    });
    let null_sys_handle = p.ctx.systems.get_or_spawn(NULL_SYS);
    null_sys_handle
        .spawn_ball(Ball {
            id: p.ship_id,
            type_id: HULK,
            position: p.ship_pos,
            velocity: DVec3::ZERO,
            max_velocity: 150.0,
            inertia: 1.0,
            mass: 1e7,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    let pris_ast_id = 9_300_010_i64;
    let field = eve_server::services::mining_b33::spawn_phased_asteroid_field(
        &p.ctx,
        NULL_SYS,
        "PHS-001",
        pris_ast_id,
        50_000,
        (ast_pos.x, ast_pos.y, ast_pos.z),
    )
    .expect("Null-sec Phased Asteroid Field must spawn");
    assert!(!field.beacon_spawned && !field.phased_in);
    null_sys_handle
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

    // Breached Prismaticite mines at 10% efficiency before Mobile Phase Anchor is deployed & powered
    rpc(&mut p, "dogmaIM", None, "AddTarget", &[PyRep::Integer(pris_ast_id)]).await.expect("AddTarget");
    for _ in 0..30 {
        if null_sys_handle.get_target_locks(p.ship_id).await.unwrap().iter().any(|l| l.target_id == pris_ast_id && l.locked) {
            break;
        }
        null_sys_handle.step_manual(1.0).await.unwrap();
    }
    activate(&mut p, miner_id, pris_ast_id).await.unwrap();
    let breached_outcomes = advance(&p, 46).await;
    let breached_units = breached_outcomes[0].units_delivered;
    assert!(breached_units > 0);

    // 2. Deploy Mobile Phase Anchor (90037) via ship.Drop and power with ICS (Porpoise 35 + Orca 50 = 85 < 100; + Porpoise 35 = 120 >= 100)
    let anchor_cargo_id = 9_300_020_i64;
    insert_item(&p.ctx, anchor_cargo_id, MOBILE_PHASE_ANCHOR, CHAR_ID, p.ship_id, InventoryFlag::Cargo, 1);
    let _ = rpc(&mut p, "ship", None, "Drop", &[PyRep::Integer(anchor_cargo_id)]).await.unwrap();
    assert_eq!(eve_server::services::mining_b33::connect_ics_to_phase_anchor(&p.ctx, NULL_SYS, PORPOISE), Some(false));
    assert_eq!(eve_server::services::mining_b33::connect_ics_to_phase_anchor(&p.ctx, NULL_SYS, ORCA), Some(false));
    assert_eq!(eve_server::services::mining_b33::connect_ics_to_phase_anchor(&p.ctx, NULL_SYS, PORPOISE), Some(true));

    let phased_outcomes = advance(&p, 46).await;
    let phased_units = phased_outcomes[0].units_delivered;
    assert!(
        phased_units >= breached_units * 8,
        "Phased-in Prismaticite ({phased_units}) must yield ~10x breached yield ({breached_units})"
    );

    // 3. Live cluster tick advances fracture past 0.5 -> beacon spawns at site position; past 1.0 -> field collapses
    p.ctx.clock().advance_secs(1);
    eve_server::services::mining_b33::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
    p.ctx.clock().advance_secs(7_201);
    eve_server::services::mining_b33::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
    let anom_pos = p.ctx.db.with_snapshot(|s| {
        s.cosmic_anomalies
            .get(&NULL_SYS)
            .and_then(|v| v.iter().find(|a| a.anomaly_id == "PHS-001" && !a.cleared))
            .map(|a| a.position)
    });
    assert_eq!(anom_pos, Some([ast_pos.x, ast_pos.y, ast_pos.z]), "Beacon must spawn at actual site position");

    p.ctx.clock().advance_secs(7_201);
    eve_server::services::mining_b33::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
    let collapsed_qty = p.ctx.db.with_snapshot(|s| s.asteroids.get(&pris_ast_id).map(|a| a.quantity).unwrap_or(-1));
    assert_eq!(collapsed_qty, 0, "Collapsed phased asteroid must be depleted");

    // 4. Prismaticite single-random-mineral-of-8 reprocessing, Unrefined Mineral reprocessing, and Unrefined Mineral reaction
    let pris_type = p.ctx.sde.types.get(&PRISMATICITE).unwrap();
    let mut seen_minerals = HashSet::new();
    for seed in 1..=25u64 {
        let mats = eve_server::services::space_mining_harvest::resolve_reprocessable_materials_for_portions(
            &p.ctx.sde, pris_type, 9_300_099, 1, seed * 0x9E37_79B9, 1, false,
        )
        .expect("Prismaticite batch");
        assert_eq!(mats.len(), 1, "Single batch of Prismaticite must yield exactly 1 mineral type");
        seen_minerals.insert(mats[0].material_type_id);
    }
    assert!(seen_minerals.len() >= 3, "Prismaticite batches must randomly select across the 8 minerals");

    let unref_type = p.ctx.sde.types.get(&UNREFINED_TRITANIUM).unwrap();
    let unref_mats = eve_server::services::space_mining_harvest::resolve_reprocessable_materials_for_portions(
        &p.ctx.sde, unref_type, 9_300_100, 1, 0xABCD_1234, 5, false,
    )
    .expect("Unrefined Tritanium reprocesses into Tritanium");
    assert_eq!(unref_mats.len(), 1);
    assert_eq!(unref_mats[0].material_type_id, 34);
    assert!(unref_mats[0].quantity > 0);

    // Unrefined Mineral reaction requires online Standup Composite Reactor I (45537)
    let athanor_id = 1_000_000_099_001_i64;
    let bp_id = 9_300_200_i64;
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            9_800_099,
            CorporationRecord { corporation_id: 9_800_099, name: "React Corp".into(), ticker: "RCT".into(), ceo_id: CHAR_ID, ..Default::default() },
        );
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) { ch.corporation_id = 9_800_099; }
        snap.structures.insert(
            athanor_id,
            UpwellStructureRecord {
                structure_id: athanor_id, type_id: ATHANOR, owner_corp_id: 9_800_099, solar_system_id: NULL_SYS,
                name: "React Athanor".into(), position: [100_000.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
                quantum_core_installed: true, fuel_blocks: 1_000, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
                ..Default::default()
            },
        );
        snap.items.insert(
            bp_id,
            InventoryItemRecord {
                item_id: bp_id, type_id: UNREFINED_TRIT_FORMULA, owner_id: CHAR_ID, location_id: athanor_id,
                flag: InventoryFlag::Hangar, quantity: 1, singleton: false, version: 1, ..Default::default()
            },
        );
    });
    p.sess.structure_id = Some(athanor_id);
    p.ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&CHAR_ID) { ch.structure_id = Some(athanor_id); }
    });
    let react_err = rpc(
        &mut p,
        "industryManager",
        None,
        "InstallJob",
        &[PyRep::Integer(i64::from(UNREFINED_TRIT_FORMULA)), PyRep::Integer(9), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert!(react_err.contains("CompositeReactorRequired"), "Unexpected error: {react_err}");

    insert_item(&p.ctx, 9_300_201, STANDUP_COMPOSITE_REACTOR, 9_800_099, athanor_id, InventoryFlag::ServiceSlot0, 1);
    p.ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(9_300_201, true); });
    let react_after = rpc(
        &mut p,
        "industryManager",
        None,
        "InstallJob",
        &[PyRep::Integer(i64::from(UNREFINED_TRIT_FORMULA)), PyRep::Integer(9), PyRep::Integer(1)],
    )
    .await;
    if let Err(e) = &react_after {
        assert!(!e.contains("CompositeReactorRequired"), "Standup Composite Reactor requirement must be satisfied: {e}");
    }

    // 5. Wormhole ore sites by class
    let c1_sites = eve_server::services::mining_sites::wormhole_ore_sites_for_class(1);
    let c5_sites = eve_server::services::mining_sites::wormhole_ore_sites_for_class(5);
    assert!(c1_sites.iter().all(|s| !s.dungeon_name.contains("Core")));
    assert!(c5_sites.iter().any(|s| s.dungeon_name.contains("Core")));
}
