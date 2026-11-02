//! End-to-end integration tests for Batch 33 open `feat:mining.*` rows (part 1: Foreman bursts,
//! harvester validity, phased asteroid field lifecycle, Mobile Phase Anchor, Prismaticite &
//! Unrefined Mineral reactions, and wormhole ore sites).

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

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
const DEEP_CORE_STRIP_II: u32 = 24_305;
const STRIP_MINER_I: u32 = 17_482;
const MOD_STRIP_MINER_II: u32 = 17_912;
const HULK: u32 = 22_544;
const MLU_II: u32 = 28_576;
const ICE_HARVESTER_I: u32 = 16_278;
const GAS_HARVESTER_I: u32 = 25_266;
const ICE_HARVESTER_UPGRADE_II: u32 = 28_578;
const ICE_ACCEL_RIG_I: u32 = 32_819;
const YETI_IMPLANT_1005: u32 = 22_571;
const FOREMAN_MINDLINK: u32 = 22_559;
const LARGE_IND_CORE_I: u32 = 58_945;
const BURST_II: u32 = 43_551;
const CHARGE_FIELD: u32 = 42_829;
const CHARGE_OPTIMIZATION: u32 = 42_830;
const CHARGE_PRESERVATION: u32 = 42_831;
const PORPOISE: u32 = 42_244;
const ORCA: u32 = 28_606;
const ROKH: u32 = 24_688;
const PRISMATICITE: u32 = 90_041;
const COMPRESSED_PRISMATICITE: u32 = 90_307;
const ATMOSPHERIC_GASES: u32 = 16_634;
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

    let (miner_id, strip_id, strip2_id, deep_core_id, deep_strip_id, ice_harv_id, gas_harv_id, burst_id, ind_core_id) = (
        9_100_001_i64, 9_100_003_i64, 9_100_004_i64, 9_100_005_i64,
        9_100_006_i64, 9_100_007_i64, 9_100_008_i64, 9_100_009_i64, 9_100_010_i64,
    );

    // 0. Command Burst command-hull enforcement: Rokh battleship must be rejected on both fitting and activation
    let mut p_rokh = pilot(
        ROKH,
        &[(22_536, 5), (22_552, 5), (3_426, 5), (3_413, 5)],
        &[(9_100_099, BURST_II, InventoryFlag::HiSlot0)],
    )
    .await;
    insert_item(&p_rokh.ctx, 9_100_098, CHARGE_FIELD, CHAR_ID, p_rokh.ship_id, InventoryFlag::HiSlot0, 2);
    let rokh_fit = p_rokh.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p_rokh.ship_id).unwrap();
    assert!(!rokh_fit.modules.contains_key(&9_100_099), "Dogma fitting must reject Mining Foreman Burst II on Rokh");
    let rokh_act_err = rpc(&mut p_rokh, "dogmaIM", None, "Activate", &[PyRep::Integer(9_100_099)]).await.unwrap_err();
    assert!(rokh_act_err.contains("CantFitToShip"), "Activate must reject Mining Foreman Burst II on Rokh: {rokh_act_err}");

    let mut p = pilot(
        ORCA,
        &[
            (3_386, 5), (3_410, 5), (11_395, 5), (16_281, 5), (25_544, 5),
            (22_536, 5), (22_552, 5), (29_637, 5), (33_399, 5), (3_429, 5), (3_426, 5), (3_413, 5),
        ],
        &[
            (miner_id, MINER_I, InventoryFlag::HiSlot0),
            (burst_id, BURST_II, InventoryFlag::HiSlot1),
            (ind_core_id, LARGE_IND_CORE_I, InventoryFlag::HiSlot2),
        ],
    )
    .await;
    p.ctx.db.with_mut_snapshot(|s| {
        s.active_implants.entry(CHAR_ID).or_default().push(FOREMAN_MINDLINK);
        s.module_active_states.insert(ind_core_id);
    });
    let bid = p.beyonce_id;
    let _ = rpc(&mut p, "beyonceBound", Some(bid), "CmdStop", &[]).await;

    // 1. Ice harvester cycle duration modifiers: skill, non-stacking Ice Harvester Upgrade II, Hulk hull, rig, implant (1 block/cycle)
    let (ice_upg_1, ice_upg_2, ice_rig_id) = (9_100_021_i64, 9_100_022_i64, 9_100_023_i64);
    let mut p_ice = pilot(
        HULK,
        &[(3_386, 5), (11_395, 5), (16_281, 5), (17_940, 5), (22_551, 5), (22_552, 5), (3_429, 5), (3_426, 5), (3_413, 5)],
        &[
            (ice_harv_id, ICE_HARVESTER_I, InventoryFlag::HiSlot0),
            (ice_upg_1, ICE_HARVESTER_UPGRADE_II, InventoryFlag::LoSlot0),
            (ice_upg_2, ICE_HARVESTER_UPGRADE_II, InventoryFlag::LoSlot1),
            (ice_rig_id, ICE_ACCEL_RIG_I, InventoryFlag::RigSlot0),
        ],
    )
    .await;
    p_ice.ctx.db.with_mut_snapshot(|s| {
        for mid in [ice_harv_id, ice_upg_1, ice_upg_2, ice_rig_id] { s.module_online_states.insert(mid, true); }
        s.active_implants.entry(CHAR_ID).or_default().push(YETI_IMPLANT_1005);
    });
    p_ice.ctx.invalidate_ship_fitting(p_ice.ship_id);
    let fit_ice = p_ice.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p_ice.ship_id).unwrap();
    let base_ice_dur = p_ice.ctx.sde.types.get(&ICE_HARVESTER_I).unwrap().default_attributes[&73];
    let ice_dur = fit_ice.get_module_effective_attribute(ice_harv_id, 73, &p_ice.ctx.sde);
    assert!(ice_dur < base_ice_dur * 0.60, "Ice Harvester duration ({ice_dur}) must be reduced by skill+2xUpgrade+Hulk+rig+Yeti from {base_ice_dur}");
    let ice_rock_id = 9_200_099_i64;
    rock(&p_ice, ice_rock_id, BLUE_ICE, 100, 500.0).await;
    lock(&mut p_ice, ice_rock_id).await;
    activate(&mut p_ice, ice_harv_id, ice_rock_id).await.unwrap();
    let ice_out = advance(&p_ice, 200).await;
    assert!(!ice_out.is_empty() && ice_out.iter().all(|o| o.units_delivered == 1), "Each ice harvester cycle must yield 1 block");

    // 2. Target validity & Mercoxit Deep Core restriction (including Miner II, Strip Miners, Deep Core Strip Miner II, and positive gas harvesting)
    let (veld_rock, merc_rock, ice_rock, gas_cloud, far_rock) = (9_200_001, 9_200_002, 9_200_003, 9_200_004, 9_200_005);
    for (rid, tid, qty, dist) in [
        (veld_rock, VELDSPAR, 10_000, 500.0), (merc_rock, MERCOXIT, 10_000, 500.0),
        (ice_rock, BLUE_ICE, 100, 500.0), (gas_cloud, FULLERITE_C50, 1_000, 500.0), (far_rock, VELDSPAR, 10_000, 35_000.0),
    ] {
        rock(&p, rid, tid, qty, dist).await;
        lock(&mut p, rid).await;
    }
    assert!(activate(&mut p, miner_id, merc_rock).await.is_err(), "Miner I must be refused on Mercoxit");
    assert!(activate(&mut p, miner_id, ice_rock).await.is_err());
    assert!(activate(&mut p, miner_id, gas_cloud).await.is_err());
    assert!(activate(&mut p, miner_id, far_rock).await.is_err());

    let mut p_dc = pilot(
        ROKH,
        &[(3_386, 5), (11_395, 5), (25_544, 5), (3_429, 5), (3_426, 5), (3_413, 5)],
        &[(deep_core_id, DEEP_CORE_MINER_I, InventoryFlag::HiSlot0), (gas_harv_id, GAS_HARVESTER_I, InventoryFlag::HiSlot1)],
    )
    .await;
    let bid_dc = p_dc.beyonce_id;
    let _ = rpc(&mut p_dc, "beyonceBound", Some(bid_dc), "CmdStop", &[]).await;
    p_dc.ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(deep_core_id, true);
        s.module_online_states.insert(gas_harv_id, true);
    });
    p_dc.ctx.invalidate_ship_fitting(p_dc.ship_id);
    let (dc_veld_rock, dc_merc_rock, dc_gas_cloud) = (9_200_101_i64, 9_200_102_i64, 9_200_103_i64);
    for (rid, tid, qty) in [(dc_veld_rock, VELDSPAR, 10_000), (dc_merc_rock, MERCOXIT, 10_000), (dc_gas_cloud, FULLERITE_C50, 1_000)] {
        rock(&p_dc, rid, tid, qty, 100.0).await;
        lock(&mut p_dc, rid).await;
    }
    assert!(activate(&mut p_dc, gas_harv_id, dc_veld_rock).await.is_err());
    activate(&mut p_dc, deep_core_id, dc_merc_rock).await.unwrap();
    let _ = rpc(&mut p_dc, "dogmaIM", None, "Deactivate", &[PyRep::Integer(deep_core_id)]).await;

    // Positive gas-cloud harvesting
    activate(&mut p_dc, gas_harv_id, dc_gas_cloud).await.unwrap();
    let gas_out = advance(&p_dc, 45).await;
    assert!(!gas_out.is_empty() && gas_out[0].units_delivered > 0, "Gas Cloud Harvester must harvest Fullerite-C50");
    let _ = rpc(&mut p_dc, "dogmaIM", None, "Deactivate", &[PyRep::Integer(gas_harv_id)]).await;

    let hulk_merc_id = 9_200_098_i64;
    rock(&p_ice, hulk_merc_id, MERCOXIT, 10_000, 500.0).await;
    lock(&mut p_ice, hulk_merc_id).await;
    p_ice.ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&ice_harv_id);
        s.module_online_states.remove(&ice_harv_id);
    });
    for (bad_mod, bad_tid) in [(strip_id, STRIP_MINER_I), (strip2_id, MOD_STRIP_MINER_II)] {
        insert_item(&p_ice.ctx, bad_mod, bad_tid, CHAR_ID, p_ice.ship_id, InventoryFlag::HiSlot0, 1);
        p_ice.ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(bad_mod, true); });
        p_ice.ctx.invalidate_ship_fitting(p_ice.ship_id);
        assert!(activate(&mut p_ice, bad_mod, hulk_merc_id).await.is_err(), "Non-Deep-Core strip {bad_mod} must be refused on Mercoxit");
        p_ice.ctx.db.with_mut_snapshot(|s| { s.items.remove(&bad_mod); });
    }
    insert_item(&p_ice.ctx, deep_strip_id, DEEP_CORE_STRIP_II, CHAR_ID, p_ice.ship_id, InventoryFlag::HiSlot0, 1);
    p_ice.ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(deep_strip_id, true); });
    p_ice.ctx.invalidate_ship_fitting(p_ice.ship_id);
    activate(&mut p_ice, deep_strip_id, hulk_merc_id).await.unwrap();
    let _ = rpc(&mut p_ice, "dogmaIM", None, "Deactivate", &[PyRep::Integer(deep_strip_id)]).await;

    // 3. Fleet setup with populated member_details (wing/squad), in-range (5 km), out-of-range (120 km), and non-fleet bystander (3 km)
    let (in_range_cid, in_range_sid, in_range_miner) = (9_210_001_i64, 9_210_002_i64, 9_210_010_i64);
    let (out_range_cid, out_range_sid) = (9_210_003_i64, 9_210_004_i64);
    let (non_fleet_cid, non_fleet_sid) = (9_210_005_i64, 9_210_006_i64);
    p.ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.get(&CHAR_ID).unwrap().clone();
        let base_skills = snap.skills.get(&CHAR_ID).cloned().unwrap_or_default();
        for (cid, sid) in [(in_range_cid, in_range_sid), (out_range_cid, out_range_sid), (non_fleet_cid, non_fleet_sid)] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
            snap.skills.insert(cid, base_skills.clone());
            snap.items.insert(sid, InventoryItemRecord {
                item_id: sid, type_id: ROKH, owner_id: cid, location_id: i64::from(JITA),
                flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        }
        snap.items.insert(in_range_miner, InventoryItemRecord {
            item_id: in_range_miner, type_id: MINER_I, owner_id: in_range_cid, location_id: in_range_sid,
            flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.module_online_states.insert(in_range_miner, true);
        let mut fl = FleetRecord { fleet_id: 77_001, boss_id: CHAR_ID, members: vec![CHAR_ID, in_range_cid, out_range_cid], ..Default::default() };
        fl.member_details.insert(CHAR_ID, eve_db::FleetMemberRecord {
            character_id: CHAR_ID, role: eve_db::FleetRole::FleetCommander, wing_id: Some(1), squad_id: Some(1),
            takes_fleet_warp: true, accepts_regroup: true, accepts_conduit_jumps: true,
            job: 2, join_time_unix: 0, role_name: "FC".into(),
        });
        fl.member_details.insert(in_range_cid, eve_db::FleetMemberRecord {
            character_id: in_range_cid, role: eve_db::FleetRole::SquadMember, wing_id: Some(1), squad_id: Some(1),
            takes_fleet_warp: true, accepts_regroup: true, accepts_conduit_jumps: true,
            job: 0, join_time_unix: 0, role_name: String::new(),
        });
        fl.member_details.insert(out_range_cid, eve_db::FleetMemberRecord {
            character_id: out_range_cid, role: eve_db::FleetRole::SquadMember, wing_id: Some(1), squad_id: Some(1),
            takes_fleet_warp: true, accepts_regroup: true, accepts_conduit_jumps: true,
            job: 0, join_time_unix: 0, role_name: String::new(),
        });
        snap.fleets.insert(77_001, fl);
    });
    let sys_handle = p.ctx.systems.get_or_spawn(JITA);
    for (sid, offset_x) in [(in_range_sid, 5_000.0), (out_range_sid, 120_000.0), (non_fleet_sid, 3_000.0)] {
        sys_handle.spawn_ball(Ball {
            id: sid, type_id: HULK, position: p.ship_pos + DVec3::new(offset_x, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 150.0, inertia: 1.0, mass: 1e7, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
    }

    let miner_type = p.ctx.sde.types.get(&MINER_I).unwrap().clone();
    let dbuff = |ctx: &eve_server::router::ServerContext, sid: i64, attr: u16| {
        eve_server::services::mining_cycle::dbuff_multiplier(ctx, sid, &miner_type, attr)
    };

    // 4a. Field Enhancement charge: boosts range (54), ignores out-of-range & non-fleet; 60s reload on charge switch
    let charge_item_id = 9_100_030_i64;
    insert_item(&p.ctx, charge_item_id, CHARGE_FIELD, CHAR_ID, p.ship_id, InventoryFlag::HiSlot1, 5);
    let burst_rep = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
    assert_eq!(dict_get(&burst_rep, "chargeTypeID"), Some(&PyRep::Integer(i64::from(CHARGE_FIELD))));
    let buff_dur = dict_get(&burst_rep, "buffDurationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((buff_dur - 112.5).abs() < 1e-4, "Mining Foreman V + Mindlink must scale burst duration to 112.5s, got {buff_dur}");
    assert_eq!(p.ctx.db.with_snapshot(|s| s.items.get(&charge_item_id).map(|i| i.quantity).unwrap_or(0)), 4);

    assert!(dbuff(&p.ctx, in_range_sid, 54) > 1.5, "In-range fleet member must receive Field Enhancement range boost");
    assert!((dbuff(&p.ctx, out_range_sid, 54) - 1.0).abs() < 1e-6, "Out-of-range ship must not receive burst");
    assert!((dbuff(&p.ctx, non_fleet_sid, 54) - 1.0).abs() < 1e-6, "Non-fleet bystander must not receive burst");
    assert!((dbuff(&p.ctx, in_range_sid, 73) - 1.0).abs() < 1e-6);

    // 4b. Optimization charge: reduces duration (73) & capacitorNeed (6) on live mining cycle; no range/volatility
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
    p.ctx.db.with_mut_snapshot(|s| { s.items.remove(&charge_item_id); });
    p.ctx.clock().advance_secs(120);
    let mut sess_m = p.sess.clone();
    sess_m.character_id = Some(in_range_cid);
    sess_m.ship_id = Some(in_range_sid);
    let _ = sys_handle.try_update_combat_state(in_range_sid, |c| { c.capacitor = 1_000.0; c.max_capacitor = 1_000.0; });
    p.ctx.dispatch_rpc(&mut sess_m, "dogmaIM", None, "AddTarget", &[PyRep::Integer(veld_rock)], &HashMap::new(), None).await.unwrap();
    for _ in 0..30 {
        if sys_handle.get_target_locks(in_range_sid).await.unwrap().iter().any(|l| l.target_id == veld_rock && l.locked) { break; }
        sys_handle.step_manual(1.0).await.unwrap();
    }
    let cap_before_unboosted = sys_handle.get_combat_state(in_range_sid).await.unwrap().unwrap().capacitor;
    p.ctx.dispatch_rpc(&mut sess_m, "dogmaIM", None, "Activate", &[PyRep::Integer(in_range_miner), PyRep::Integer(veld_rock)], &HashMap::new(), None).await.unwrap();
    let unboosted_cap_used = cap_before_unboosted - sys_handle.get_combat_state(in_range_sid).await.unwrap().unwrap().capacitor;
    let _ = p.ctx.dispatch_rpc(&mut sess_m, "dogmaIM", None, "Deactivate", &[PyRep::Integer(in_range_miner)], &HashMap::new(), None).await;

    insert_item(&p.ctx, 9_100_031, CHARGE_OPTIMIZATION, CHAR_ID, p.ship_id, InventoryFlag::HiSlot1, 3);
    let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
    assert!(dbuff(&p.ctx, in_range_sid, 73) < 0.70, "Optimization + Mindlink + IndCore must reduce duration >= 30%");
    assert!(dbuff(&p.ctx, in_range_sid, 6) < 0.70, "Optimization + Mindlink + IndCore must reduce cap use >= 30%");
    assert!((dbuff(&p.ctx, in_range_sid, 54) - 1.0).abs() < 1e-6);

    let cap_before_boosted = sys_handle.get_combat_state(in_range_sid).await.unwrap().unwrap().capacitor;
    p.ctx.dispatch_rpc(&mut sess_m, "dogmaIM", None, "Activate", &[PyRep::Integer(in_range_miner), PyRep::Integer(veld_rock)], &HashMap::new(), None).await.unwrap();
    let boosted_cap_used = cap_before_boosted - sys_handle.get_combat_state(in_range_sid).await.unwrap().unwrap().capacitor;
    assert!(boosted_cap_used < unboosted_cap_used * 0.75, "Boosted mining cycle cap ({boosted_cap_used}) must be reduced vs unboosted ({unboosted_cap_used})");

    // 4c. Equipment Preservation charge: reduces crystal volatility (783), does NOT grant range or duration
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
    p.ctx.db.with_mut_snapshot(|s| { s.items.remove(&9_100_031); });
    p.ctx.clock().advance_secs(120);
    insert_item(&p.ctx, 9_100_032, CHARGE_PRESERVATION, CHAR_ID, p.ship_id, InventoryFlag::HiSlot1, 3);
    let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
    assert!(dbuff(&p.ctx, in_range_sid, 783) < 0.70, "Preservation charge must reduce crystal volatility >= 30%");
    assert!((dbuff(&p.ctx, in_range_sid, 54) - 1.0).abs() < 1e-6);
    assert!((dbuff(&p.ctx, in_range_sid, 73) - 1.0).abs() < 1e-6);
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

    // 2. Deploy Mobile Phase Anchor (90037) via ship.Drop; foreign anchor must not block; ICS balls required within 50 km
    let (porp1_ship, orca_ship, porp2_ship) = (9_300_031_i64, 9_300_032_i64, 9_300_033_i64);
    assert_eq!(eve_server::services::mining_b33::connect_ics_to_phase_anchor(&p.ctx, NULL_SYS, porp1_ship).await, None, "Must refuse ICS without deployed anchor & ball");

    insert_item(&p.ctx, 9_300_019, MOBILE_PHASE_ANCHOR, 9_999_999, i64::from(NULL_SYS), InventoryFlag::None, 1);
    let anchor_cargo_id = 9_300_020_i64;
    insert_item(&p.ctx, anchor_cargo_id, MOBILE_PHASE_ANCHOR, CHAR_ID, p.ship_id, InventoryFlag::Cargo, 1);
    let _ = rpc(&mut p, "ship", None, "Drop", &[PyRep::Integer(anchor_cargo_id)]).await.unwrap();

    p.ctx.db.with_mut_snapshot(|snap| {
        for (idx, (cid, sid, tid)) in [(9_300_131_i64, porp1_ship, PORPOISE), (9_300_132_i64, orca_ship, ORCA), (9_300_133_i64, porp2_ship, PORPOISE)].into_iter().enumerate() {
            let mut ch = snap.characters.get(&CHAR_ID).unwrap().clone();
            ch.character_id = cid;
            ch.name = format!("ICS_Pilot_{idx}");
            ch.active_ship_id = sid;
            ch.solar_system_id = NULL_SYS;
            snap.characters.insert(cid, ch);
            snap.items.insert(sid, InventoryItemRecord {
                item_id: sid, type_id: tid, owner_id: cid, location_id: i64::from(NULL_SYS),
                flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
        }
        snap.fleets.insert(9_300_200, FleetRecord {
            fleet_id: 9_300_200, boss_id: CHAR_ID, members: vec![CHAR_ID, 9_300_131, 9_300_132, 9_300_133], ..Default::default()
        });
    });
    for (sid, tid, offset) in [(porp1_ship, PORPOISE, 2_000.0), (orca_ship, ORCA, 4_000.0), (porp2_ship, PORPOISE, 6_000.0)] {
        null_sys_handle.spawn_ball(Ball {
            id: sid, type_id: tid, position: p.ship_pos + DVec3::new(offset, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 150.0, inertia: 1.0, mass: 1e7, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
    }

    // Re-connecting the same Porpoise ship_id twice must not double-count energy (35 + 50 + 35 = 120 >= 100)
    assert_eq!(eve_server::services::mining_b33::connect_ics_to_phase_anchor(&p.ctx, NULL_SYS, porp1_ship).await, Some(false));
    assert_eq!(eve_server::services::mining_b33::connect_ics_to_phase_anchor(&p.ctx, NULL_SYS, porp1_ship).await, Some(false));
    assert_eq!(eve_server::services::mining_b33::connect_ics_to_phase_anchor(&p.ctx, NULL_SYS, orca_ship).await, Some(false));
    assert_eq!(eve_server::services::mining_b33::connect_ics_to_phase_anchor(&p.ctx, NULL_SYS, porp2_ship).await, Some(true));

    let phased_outcomes = advance(&p, 46).await;
    let phased_units = phased_outcomes[0].units_delivered;
    assert!(phased_units >= breached_units * 8, "Phased-in Prismaticite ({phased_units}) must yield ~10x breached yield ({breached_units})");

    // 3. Live cluster tick advances fracture past 0.5 -> beacon spawns at site position; past 1.0 -> field collapses
    p.ctx.clock().advance_secs(1);
    eve_server::services::mining_b33::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
    p.ctx.clock().advance_secs(7_201);
    eve_server::services::mining_b33::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
    let anom_pos = p.ctx.db.with_snapshot(|s| {
        s.cosmic_anomalies.get(&NULL_SYS).and_then(|v| v.iter().find(|a| a.anomaly_id == "PHS-001" && !a.cleared)).map(|a| a.position)
    });
    assert_eq!(anom_pos, Some([ast_pos.x, ast_pos.y, ast_pos.z]), "Beacon must spawn at actual site position");

    p.ctx.clock().advance_secs(7_201);
    eve_server::services::mining_b33::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
    assert_eq!(p.ctx.db.with_snapshot(|s| s.asteroids.get(&pris_ast_id).map(|a| a.quantity).unwrap_or(-1)), 0);

    // 4. Unrefined Mineral reaction (requires online Standup Composite Reactor I 45537) & live reprocessingSvc.Reprocess variable batches
    let (athanor_id, bp_id) = (1_000_000_099_001_i64, 9_300_200_i64);
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(9_800_099, CorporationRecord {
            corporation_id: 9_800_099, name: "React Corp".into(), ticker: "RCT".into(), ceo_id: CHAR_ID, ..Default::default()
        });
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.corporation_id = 9_800_099;
            ch.balance = eve_db::IskCents(100_000_000_000);
            ch.structure_id = Some(athanor_id);
        }
        snap.structures.insert(athanor_id, UpwellStructureRecord {
            structure_id: athanor_id, type_id: ATHANOR, owner_corp_id: 9_800_099, solar_system_id: NULL_SYS,
            name: "React Athanor".into(), position: [100_000.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 1_000, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0, ..Default::default()
        });
        snap.items.insert(bp_id, InventoryItemRecord {
            item_id: bp_id, type_id: UNREFINED_TRIT_FORMULA, owner_id: CHAR_ID, location_id: athanor_id,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: false, version: 1, ..Default::default()
        });
    });
    p.sess.structure_id = Some(athanor_id);
    insert_item(&p.ctx, 9_300_202, ATMOSPHERIC_GASES, CHAR_ID, athanor_id, InventoryFlag::Hangar, 1_000);
    insert_item(&p.ctx, 9_300_203, COMPRESSED_PRISMATICITE, CHAR_ID, athanor_id, InventoryFlag::Hangar, 1_000);
    insert_item(&p.ctx, 9_300_204, 35_899, 9_800_099, athanor_id, InventoryFlag::ServiceSlot1, 1);
    p.ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(9_300_204, true); });

    let react_err = rpc(&mut p, "industryManager", None, "InstallJob", &[PyRep::Integer(i64::from(UNREFINED_TRIT_FORMULA)), PyRep::Integer(9), PyRep::Integer(5)]).await.unwrap_err();
    assert!(react_err.contains("CompositeReactorRequired"), "Unexpected error: {react_err}");

    insert_item(&p.ctx, 9_300_201, STANDUP_COMPOSITE_REACTOR, 9_800_099, athanor_id, InventoryFlag::ServiceSlot0, 1);
    p.ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(9_300_201, true); });
    let job_rep = rpc(&mut p, "industryManager", None, "InstallJob", &[PyRep::Integer(i64::from(UNREFINED_TRIT_FORMULA)), PyRep::Integer(9), PyRep::Integer(5)]).await.expect("Reaction InstallJob must succeed");
    let job_id = dict_get(&job_rep, "jobID").and_then(|v| v.as_i64()).expect("jobID");
    p.ctx.clock().advance_secs(3_600);
    let _ = rpc(&mut p, "industryManager", None, "CompleteJob", &[PyRep::Integer(job_id)]).await.expect("CompleteJob must deliver Unrefined Tritanium");
    let produced_unref: i32 = p.ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == athanor_id && i.type_id == UNREFINED_TRITANIUM).map(|i| i.quantity).sum());
    assert!(produced_unref >= 100, "Completed reaction must produce Unrefined Tritanium, got {produced_unref}");

    // Reprocess multiple batches of Prismaticite & produced Unrefined Tritanium via live reprocessingSvc.Reprocess
    let (mut seen_minerals, mut pris_qtys, mut unref_qtys) = (HashSet::new(), HashSet::new(), HashSet::new());
    for batch in 0..8i64 {
        let pris_batch_id = 9_300_300 + batch;
        let unref_batch_id = 9_300_400 + batch;
        insert_item(&p.ctx, pris_batch_id, PRISMATICITE, CHAR_ID, athanor_id, InventoryFlag::Hangar, 100);
        insert_item(&p.ctx, unref_batch_id, UNREFINED_TRITANIUM, CHAR_ID, athanor_id, InventoryFlag::Hangar, 100);
        let before_minerals: std::collections::HashMap<u32, i32> = p.ctx.db.with_snapshot(|s| {
            let mut m = HashMap::new();
            for it in s.items.values().filter(|i| i.location_id == athanor_id && ((34..=40).contains(&i.type_id) || i.type_id == 11_399)) {
                *m.entry(it.type_id).or_insert(0) += it.quantity;
            }
            m
        });
        let _ = rpc(&mut p, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(pris_batch_id)]).await.unwrap();
        p.ctx.db.with_snapshot(|s| {
            let mut after = HashMap::new();
            for it in s.items.values().filter(|i| i.location_id == athanor_id && ((34..=40).contains(&i.type_id) || i.type_id == 11_399)) {
                *after.entry(it.type_id).or_insert(0) += it.quantity;
            }
            for (tid, qty) in after {
                let delta = qty - before_minerals.get(&tid).copied().unwrap_or(0);
                if delta > 0 {
                    seen_minerals.insert(tid);
                    pris_qtys.insert(delta);
                }
            }
        });
        let trit_before: i32 = p.ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == athanor_id && i.type_id == 34).map(|i| i.quantity).sum());
        let _ = rpc(&mut p, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(unref_batch_id)]).await.unwrap();
        let trit_after: i32 = p.ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == athanor_id && i.type_id == 34).map(|i| i.quantity).sum());
        unref_qtys.insert(trit_after - trit_before);
    }
    assert!(seen_minerals.len() >= 3, "Prismaticite reprocessing across batches must yield random minerals from the 8 mineral types");
    assert!(pris_qtys.len() >= 2 && unref_qtys.len() >= 3, "Prismaticite and Unrefined Mineral reprocessing must yield variable batch quantities");

    // 5. Wormhole ore sites by class
    let c1_sites = eve_server::services::mining_sites::wormhole_ore_sites_for_class(1);
    let c5_sites = eve_server::services::mining_sites::wormhole_ore_sites_for_class(5);
    assert!(c1_sites.iter().all(|s| !s.dungeon_name.contains("Core")));
    assert!(c5_sites.iter().any(|s| s.dungeon_name.contains("Core")));
}
