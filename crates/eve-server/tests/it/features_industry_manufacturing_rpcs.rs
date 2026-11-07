//! End-to-end integration tests for `feat:mining.*` Foreman bursts,
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
const CHARGE_EFFICIENCY: u32 = 90_733;
const SURVEY_SCANNER_I: u32 = 444;
const BURST_I: u32 = 42_528;
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
        &[(3_386, 5), (11_395, 5), (16_281, 5), (17_940, 5), (22_551, 5), (22_552, 5), (22_578, 4), (3_429, 5), (3_426, 5), (3_413, 5)],
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
    p_ice.ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(ice_upg_2, false); });
    p_ice.ctx.invalidate_ship_fitting(p_ice.ship_id);
    let ice_dur_1_upg = p_ice.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p_ice.ship_id).unwrap().get_module_effective_attribute(ice_harv_id, 73, &p_ice.ctx.sde);
    assert!(ice_dur < ice_dur_1_upg, "Second Ice Harvester Upgrade II (stackable=1) must further reduce duration ({ice_dur} < {ice_dur_1_upg})");
    p_ice.ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(ice_upg_2, true); });
    p_ice.ctx.invalidate_ship_fitting(p_ice.ship_id);
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
    assert!(activate(&mut p_ice, ice_harv_id, hulk_merc_id).await.is_err(), "Ice Harvester must be refused on ore");
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

    // 4a. Field Enhancement charge: boosts range (54) on Miner/Strip/Ice/Survey Scanner without squaring; timed expiry
    let survey_id = 9_100_033_i64;
    insert_item(&p.ctx, survey_id, SURVEY_SCANNER_I, CHAR_ID, p.ship_id, InventoryFlag::MedSlot0, 1);
    p.ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(survey_id, true); });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    let charge_item_id = 9_100_030_i64;
    insert_item(&p.ctx, charge_item_id, CHARGE_FIELD, CHAR_ID, p.ship_id, InventoryFlag::HiSlot1, 8);
    let burst_rep = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
    assert_eq!(dict_get(&burst_rep, "chargeTypeID"), Some(&PyRep::Integer(i64::from(CHARGE_FIELD))));
    let buff_dur = dict_get(&burst_rep, "buffDurationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((buff_dur - 112.5).abs() < 1e-4, "Mining Foreman V + Mindlink must scale burst duration to 112.5s, got {buff_dur}");
    assert_eq!(p.ctx.db.with_snapshot(|s| s.items.get(&charge_item_id).map(|i| i.quantity).unwrap_or(0)), 7);

    let range_mult = dbuff(&p.ctx, in_range_sid, 54);
    assert!(range_mult > 2.0 && range_mult < 3.5, "In-range fleet member must receive Field Enhancement range boost ({range_mult})");
    assert!((dbuff(&p.ctx, out_range_sid, 54) - 1.0).abs() < 1e-6, "Out-of-range ship must not receive burst");
    assert!((dbuff(&p.ctx, non_fleet_sid, 54) - 1.0).abs() < 1e-6, "Non-fleet bystander must not receive burst");
    let strip_t = p.ctx.sde.types.get(&STRIP_MINER_I).unwrap();
    let ice_t = p.ctx.sde.types.get(&ICE_HARVESTER_I).unwrap();
    assert!((eve_server::services::mining_cycle::dbuff_multiplier(&p.ctx, in_range_sid, strip_t, 54) - range_mult).abs() < 1e-6);
    assert!((eve_server::services::mining_cycle::dbuff_multiplier(&p.ctx, in_range_sid, ice_t, 54) - range_mult).abs() < 1e-6);

    // Survey Scanner range uses single range_mult (not squared)
    let survey_t = p.ctx.sde.types.get(&SURVEY_SCANNER_I).unwrap();
    let survey_range_mult = eve_server::services::mining_cycle::dbuff_multiplier(&p.ctx, p.ship_id, survey_t, 54);
    let survey_near = 9_200_201_i64;
    let survey_too_far = 9_200_202_i64;
    rock(&p, survey_near, VELDSPAR, 5_000, 15_000.0 * survey_range_mult * 0.90).await;
    rock(&p, survey_too_far, VELDSPAR, 5_000, 15_000.0 * survey_range_mult * 1.15).await;
    let scan_rep = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(survey_id)]).await.unwrap();
    let scanned_ids: Vec<i64> = dict_get(&scan_rep, "asteroids").and_then(|v| match v {
        PyRep::List(items) => Some(items.iter().filter_map(|e| dict_get(e, "asteroidID").and_then(|x| x.as_i64())).collect()),
        _ => None,
    }).unwrap_or_default();
    assert!(scanned_ids.contains(&survey_near) && !scanned_ids.contains(&survey_too_far), "Survey Scanner must boost range once without squaring: {scanned_ids:?}");

    // Timed buff expiry check
    p.ctx.clock().advance_secs(60);
    assert!((dbuff(&p.ctx, in_range_sid, 54) - range_mult).abs() < 1e-6, "Buff must remain active before expires_at_unix");
    p.ctx.clock().advance_secs(60);
    assert!((dbuff(&p.ctx, in_range_sid, 54) - 1.0).abs() < 1e-6, "Buff must expire after buffDurationSeconds");

    // 4b. Isolated strength scaling: T1 vs T2 (+25%), Mining Director (+10%/lvl), Mindlink (+25%), Industrial Core (+10%)
    let full_str = dict_get(&burst_rep, "buffStrength").and_then(|v| v.as_f64()).unwrap();
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
    p.ctx.db.with_mut_snapshot(|s| { s.module_active_states.remove(&ind_core_id); });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    let no_core_str = dict_get(&rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap(), "buffStrength").and_then(|v| v.as_f64()).unwrap();
    assert!((full_str / no_core_str - 1.10).abs() < 1e-4, "Large Ind Core I must scale strength by 1.10x");
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
    p.ctx.db.with_mut_snapshot(|s| { s.active_implants.remove(&CHAR_ID); });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    let no_mind_str = dict_get(&rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap(), "buffStrength").and_then(|v| v.as_f64()).unwrap();
    assert!((no_core_str / no_mind_str - 1.25).abs() < 1e-4, "Foreman Mindlink must scale strength by 1.25x");
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
    p.ctx.db.with_mut_snapshot(|s| { if let Some(sk) = s.skills.get_mut(&CHAR_ID).and_then(|m| m.get_mut(&22_552)) { sk.level = 1; } });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    let dir1_str = dict_get(&rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap(), "buffStrength").and_then(|v| v.as_f64()).unwrap();
    assert!((no_mind_str / dir1_str - (1.5 / 1.1)).abs() < 1e-4, "Mining Director V vs I must scale by 1.5/1.1");
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
    p.ctx.db.with_mut_snapshot(|s| {
        if let Some(it) = s.items.get_mut(&burst_id) { it.type_id = BURST_I; }
        if let Some(sk) = s.skills.get_mut(&CHAR_ID).and_then(|m| m.get_mut(&22_552)) { sk.level = 5; }
        s.active_implants.entry(CHAR_ID).or_default().push(FOREMAN_MINDLINK);
        s.module_active_states.insert(ind_core_id);
    });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    let t1_str = dict_get(&rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap(), "buffStrength").and_then(|v| v.as_f64()).unwrap();
    assert!((full_str / t1_str - 1.25).abs() < 1e-4, "T2 Burst must scale strength by 1.25x over T1 Burst");
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
    p.ctx.db.with_mut_snapshot(|s| { if let Some(it) = s.items.get_mut(&burst_id) { it.type_id = BURST_II; } s.items.remove(&charge_item_id); });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    p.ctx.clock().advance_secs(120);

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

    // 4c. Efficiency charge (90733): boosts crit chance (5967) & lowers residue (3154); Preservation (42831): lowers volatility (783)
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
    p.ctx.db.with_mut_snapshot(|s| { s.items.remove(&9_100_031); });
    p.ctx.clock().advance_secs(120);
    insert_item(&p.ctx, 9_100_032, CHARGE_EFFICIENCY, CHAR_ID, p.ship_id, InventoryFlag::HiSlot1, 3);
    let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
    assert!(dbuff(&p.ctx, in_range_sid, 5967) > 1.5, "Efficiency charge must boost mining crit chance (5967)");
    assert!(dbuff(&p.ctx, in_range_sid, 3154) < 0.70, "Efficiency charge must reduce mining residue probability (3154)");
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_id)]).await;
    p.ctx.db.with_mut_snapshot(|s| { s.items.remove(&9_100_032); });
    p.ctx.clock().advance_secs(120);
    insert_item(&p.ctx, 9_100_034, CHARGE_PRESERVATION, CHAR_ID, p.ship_id, InventoryFlag::HiSlot1, 3);
    let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_id)]).await.unwrap();
    assert!(dbuff(&p.ctx, in_range_sid, 783) < 0.70, "Preservation charge must reduce crystal volatility >= 30%");
    assert!((dbuff(&p.ctx, in_range_sid, 54) - 1.0).abs() < 1e-6);
    assert!((dbuff(&p.ctx, in_range_sid, 73) - 1.0).abs() < 1e-6);
}

#[tokio::test]
async fn test_b33_phased_asteroid_field_mobile_anchor_and_prismaticite() {
    // parity: feat:mining.prismaticite-reprocessing-and-unrefined-reactions
    eve_testkit::attest(&[
        "feat:mining.prismaticite-reprocessing-and-unrefined-reactions",
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
        eve_server::services::mining_phased_autotarget::spawn_phased_asteroid_field(
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
    let field = eve_server::services::mining_phased_autotarget::spawn_phased_asteroid_field(
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
        .spawn_asteroid(
            eve_server::system_actor::AsteroidState {
                asteroid_id: pris_ast_id, belt_id: 0, type_id: PRISMATICITE,
                initial_units: 50_000, remaining_units: 50_000, unit_volume: 16.0, depleted: false, respawn_at_tick: None,
            },
            Ball {
                id: pris_ast_id, type_id: PRISMATICITE, position: ast_pos, velocity: DVec3::ZERO,
                max_velocity: 0.0, inertia: 1.0, mass: 1e9, radius: 250.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
        )
        .await
        .unwrap();
    let other_pris_id = 9_300_011_i64;
    p.ctx.db.with_mut_snapshot(|s| {
        s.asteroids.insert(other_pris_id, eve_db::AsteroidRecord {
            asteroid_id: other_pris_id, solar_system_id: NULL_SYS, belt_id: 0, type_id: PRISMATICITE,
            quantity: 25_000, initial_quantity: 25_000, position: (ast_pos.x + 5_000.0, ast_pos.y, ast_pos.z), radius: 250.0,
        });
    });

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

    // 2. Deploy Mobile Phase Anchor (90037) via ship.Drop; reject WH/Pochven; live tick reconciles ICS ships within 50 km
    let (porp1_ship, orca_ship, porp2_ship) = (9_300_031_i64, 9_300_032_i64, 9_300_033_i64);
    assert!(!eve_server::services::mining_phased_autotarget::is_low_or_null_kspace(&p.ctx, 31_000_005), "Wormhole space must reject Mobile Phase Anchor");
    assert!(!eve_server::services::mining_phased_autotarget::is_low_or_null_kspace(&p.ctx, 30_001_445), "Pochven must reject Mobile Phase Anchor");

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
    for (sid, tid, offset) in [(porp1_ship, PORPOISE, 2_000.0), (orca_ship, ORCA, 4_000.0)] {
        null_sys_handle.spawn_ball(Ball {
            id: sid, type_id: tid, position: p.ship_pos + DVec3::new(offset, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 150.0, inertia: 1.0, mass: 1e7, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
    }
    null_sys_handle.spawn_ball(Ball {
        id: porp2_ship, type_id: PORPOISE, position: p.ship_pos + DVec3::new(6_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 150.0, inertia: 1.0, mass: 1e7, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    // Without active Industrial Cores, ICS hulls in range do NOT connect
    eve_server::services::mining_phased_autotarget::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
    assert!((eve_server::services::mining_phased_autotarget::phased_field_efficiency_for_asteroid(&p.ctx, NULL_SYS, pris_ast_id, PRISMATICITE) - 0.10).abs() < 1e-6);

    // Activating Industrial Cores on Porpoise #1 + Orca + Porpoise #2 connects (35 + 50 + 35 = 120 >= 100)
    p.ctx.db.with_mut_snapshot(|s| {
        for (idx, sid) in [porp1_ship, orca_ship, porp2_ship].into_iter().enumerate() {
            let cid = 9_300_300 + idx as i64;
            s.items.insert(cid, InventoryItemRecord { item_id: cid, type_id: LARGE_IND_CORE_I, owner_id: CHAR_ID, location_id: sid, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default() });
            s.module_active_states.insert(cid);
        }
    });
    eve_server::services::mining_phased_autotarget::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
    assert!((eve_server::services::mining_phased_autotarget::phased_field_efficiency_for_asteroid(&p.ctx, NULL_SYS, pris_ast_id, PRISMATICITE) - 1.0).abs() < 1e-6);

    // Moving Porpoise #2 beyond 50 km tether range drops energy to 85 < 100 and reverts phased_in
    assert!(null_sys_handle.set_ball_position(porp2_ship, p.ship_pos + DVec3::new(60_000.0, 0.0, 0.0)).await);
    eve_server::services::mining_phased_autotarget::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
    assert!((eve_server::services::mining_phased_autotarget::phased_field_efficiency_for_ore(&p.ctx, NULL_SYS, PRISMATICITE) - 0.10).abs() < 1e-6);
    assert!(null_sys_handle.set_ball_position(porp2_ship, p.ship_pos + DVec3::new(6_000.0, 0.0, 0.0)).await);
    eve_server::services::mining_phased_autotarget::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;

    let phased_outcomes = advance(&p, 46).await;
    let phased_units = phased_outcomes[0].units_delivered;
    assert!(phased_units >= breached_units * 8, "Phased-in Prismaticite ({phased_units}) must yield ~10x breached yield ({breached_units})");

    // 3. Live cluster tick advances fracture past 0.5 -> beacon spawns at site position; past 1.0 -> field collapses
    p.ctx.clock().advance_secs(1);
    eve_server::services::mining_phased_autotarget::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
    p.ctx.clock().advance_secs(7_201);
    eve_server::services::mining_phased_autotarget::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
    let anom_pos = p.ctx.db.with_snapshot(|s| {
        s.cosmic_anomalies.get(&NULL_SYS).and_then(|v| v.iter().find(|a| a.anomaly_id == "PHS-001" && !a.cleared)).map(|a| a.position)
    });
    assert_eq!(anom_pos, Some([ast_pos.x, ast_pos.y, ast_pos.z]), "Beacon must spawn at actual site position");

    p.ctx.clock().advance_secs(7_201);
    eve_server::services::mining_phased_autotarget::tick_phased_asteroid_fields(&p.ctx, p.ctx.now_unix_secs()).await;
    assert_eq!(p.ctx.db.with_snapshot(|s| s.asteroids.get(&pris_ast_id).map(|a| a.quantity).unwrap_or(-1)), 0);
    assert_eq!(p.ctx.db.with_snapshot(|s| s.asteroids.get(&other_pris_id).map(|a| a.quantity).unwrap_or(-1)), 25_000, "Collapse must not wipe unrelated Prismaticite");

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
    let hangar_minerals = |ctx: &eve_server::router::ServerContext| {
        ctx.db.with_snapshot(|s| {
            let mut m = HashMap::new();
            for it in s.items.values().filter(|i| i.location_id == athanor_id && ((34..=40).contains(&i.type_id) || i.type_id == 11_399)) {
                *m.entry(it.type_id).or_insert(0) += it.quantity;
            }
            m
        })
    };
    for batch in 0..8i64 {
        let pris_batch_id = 9_300_300 + batch;
        let unref_batch_id = 9_300_400 + batch;
        insert_item(&p.ctx, pris_batch_id, PRISMATICITE, CHAR_ID, athanor_id, InventoryFlag::Hangar, 100);
        insert_item(&p.ctx, unref_batch_id, UNREFINED_TRITANIUM, CHAR_ID, athanor_id, InventoryFlag::Hangar, 100);
        let before_minerals = hangar_minerals(&p.ctx);
        let _ = rpc(&mut p, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(pris_batch_id)]).await.unwrap();
        for (tid, qty) in hangar_minerals(&p.ctx) {
            let delta = qty - before_minerals.get(&tid).copied().unwrap_or(0);
            if delta > 0 { seen_minerals.insert(tid); pris_qtys.insert(delta); }
        }
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

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
pub(crate) mod moon_mining_reprocessing_and_remote_repairers {
    use super::*;

// End-to-end integration tests for `feat:mining.*` and `feat:modules.*` rows (
// moon extraction duration/yield, auto-fracture, moon ore class by security, Upwell refinery
// base yield, NPC reprocessing tax curve, Scrapmetal exclusions, ARSB/ARAR, and Auto Targeting System).


use std::collections::HashMap;

use crate::mining_cycle_engine::{lock, pilot, rpc, CHAR_ID, JITA, MINER_I, VELDSPAR};
use crate::features_industry_manufacturing_rpcs::{dict_get, insert_item};
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
            (3_429, 4), // Target Management IV
            (3_430, 0), // Advanced Target Management 0
            (3_428, 5), // Long Range Targeting V
            (3_431, 5), // Signature Analysis V
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

    // Repeating cycle tick must auto-lock a hostile NPC arriving after activation while ignoring neutral ship & friendly remote repairer
    let late_npc_id = 9_500_101_i64;
    sys_handle.spawn_ball_with_meta(
        Ball {
            id: late_npc_id, type_id: ANGEL_ROGUE_NPC, position: p.ship_pos + DVec3::new(7_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        eve_destiny::CombatState { signature_radius: 0.0, ..Default::default() },
        eve_destiny::SlimItemMeta { group_id: 550, category_id: 11, owner_id: 500_010, ..Default::default() },
    ).await.unwrap();
    // Friendly remote repairer active on p.ship_id must NOT be auto-locked as hostile
    let friendly_rep_ship = 9_500_102_i64;
    p.ctx.db.with_mut_snapshot(|s| {
        s.items.insert(friendly_rep_ship, InventoryItemRecord {
            item_id: friendly_rep_ship, type_id: PORPOISE, owner_id: 9_500_998, location_id: i64::from(JITA),
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });
    sys_handle.spawn_ball_with_meta(
        Ball {
            id: friendly_rep_ship, type_id: PORPOISE, position: p.ship_pos + DVec3::new(4_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta { group_id: 941, category_id: 6, owner_id: 9_500_998, ..Default::default() },
    ).await.unwrap();
    sys_handle.add_module_cycle(eve_server::system_actor::ActiveModuleCycle {
        ship_id: friendly_rep_ship, module_id: 9_500_901, module_type_id: ARAR_MEDIUM, owner_char_id: Some(9_500_998),
        effect_name: "remoteArmorRepairFalloff".into(), target_id: Some(p.ship_id), repeat: 1, duration_s: 12.0,
        remote_armor_amount: 100.0, remote_optimal: 10_000.0, remote_falloff: 10_000.0, ..Default::default()
    }).await.unwrap();
    sys_handle.step_manual(11.0).await.unwrap();
    let cycle_locks = sys_handle.get_target_locks(p.ship_id).await.unwrap();
    let late_lock = cycle_locks.iter().find(|l| l.target_id == late_npc_id).expect("Repeating Auto Targeting System cycle must lock late-arriving hostile NPC");
    let npc_sig = p.ctx.sde.types.get(&ANGEL_ROGUE_NPC).and_then(|t| t.default_attributes.get(&552).copied()).filter(|&s| s > 0.0).unwrap_or(50.0);
    let expected_lock_ms = eve_destiny::combat::compute_lock_time_seconds(fit.ship.get_attribute(564, &p.ctx.sde).max(1.0), npc_sig) * 1000.0;
    assert!((late_lock.lock_time_ms - expected_lock_ms).abs() < 1e-3, "ATS must use ship scanResolution & target signatureRadius ({expected_lock_ms}ms), got {}", late_lock.lock_time_ms);
    assert!(!cycle_locks.iter().any(|l| l.target_id == neutral_ship_id), "Repeating Auto Targeting System cycle must not lock neutral player ship");
    assert!(!cycle_locks.iter().any(|l| l.target_id == friendly_rep_ship), "Repeating Auto Targeting System cycle must not lock friendly remote repairer");

    // CONCORD (301) and Gate Sentry Gun (99) on grid must NOT be auto-locked; bloomed CombatState::signature_radius must govern lock time
    let (concord_id, sentry_id, bloomed_rat_id) = (9_500_105_i64, 9_500_106_i64, 9_500_107_i64);
    sys_handle.spawn_ball_with_meta(
        Ball { id: concord_id, type_id: 11_957, position: p.ship_pos + DVec3::new(4_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta { group_id: 301, category_id: 11, owner_id: 500_019, ..Default::default() },
    ).await.unwrap();
    sys_handle.spawn_ball_with_meta(
        Ball { id: sentry_id, type_id: 16_670, position: p.ship_pos + DVec3::new(4_500.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta { group_id: 99, category_id: 11, owner_id: 500_002, ..Default::default() },
    ).await.unwrap();
    sys_handle.spawn_ball_with_meta(
        Ball { id: bloomed_rat_id, type_id: ANGEL_ROGUE_NPC, position: p.ship_pos + DVec3::new(8_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 20.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        eve_destiny::CombatState { signature_radius: 180.0, ..Default::default() },
        eve_destiny::SlimItemMeta { group_id: 550, category_id: 11, owner_id: 500_010, ..Default::default() },
    ).await.unwrap();
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ats_id)]).await;
    let _ = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(ats_id)]).await.unwrap();
    let jita_locks2 = sys_handle.get_target_locks(p.ship_id).await.unwrap();
    assert!(!jita_locks2.iter().any(|l| l.target_id == concord_id || l.target_id == sentry_id), "ATS must NOT auto-lock CONCORD or Empire Sentry Guns");
    let bloom_lock = jita_locks2.iter().find(|l| l.target_id == bloomed_rat_id).expect("ATS must auto-lock hostile rat");
    let expected_bloom_ms = eve_destiny::combat::compute_lock_time_seconds(fit.ship.get_attribute(564, &p.ctx.sde).max(1.0), 180.0) * 1000.0;
    assert!((bloom_lock.lock_time_ms - expected_bloom_ms).abs() < 1e-3, "ATS must use CombatState::signature_radius (180m), got {}", bloom_lock.lock_time_ms);

    // Null-sec passive neutral player is NOT auto-locked; active offensive aggressor IS auto-locked on initial activation
    let null_sys: u32 = 30_004_759;
    let null_handle = p.ctx.systems.get_or_spawn(null_sys);
    let (null_neutral_id, null_aggressor_id) = (9_500_103_i64, 9_500_104_i64);
    p.ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&CHAR_ID) { ch.solar_system_id = null_sys; }
        for (sid, oid) in [(null_neutral_id, 9_500_997_i64), (null_aggressor_id, 9_500_996_i64)] {
            s.items.insert(sid, InventoryItemRecord {
                item_id: sid, type_id: PORPOISE, owner_id: oid, location_id: i64::from(null_sys),
                flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        }
    });
    for (sid, x) in [(p.ship_id, 0.0), (null_neutral_id, 5_000.0), (null_aggressor_id, 6_000.0)] {
        null_handle.spawn_ball(Ball {
            id: sid, type_id: PORPOISE, position: DVec3::new(x, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
    }
    null_handle.add_module_cycle(eve_server::system_actor::ActiveModuleCycle {
        ship_id: null_aggressor_id, module_id: 9_500_902, module_type_id: 527, owner_char_id: Some(9_500_996),
        effect_name: "warpScrambleForEntity".into(), target_id: Some(p.ship_id), repeat: 1, duration_s: 10.0, ..Default::default()
    }).await.unwrap();
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ats_id)]).await;
    let null_ats = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(ats_id)]).await.unwrap();
    let null_locked_ids: Vec<i64> = dict_get(&null_ats, "autoLockingTargetIDs").and_then(|v| match v {
        PyRep::List(l) => Some(l.iter().filter_map(|x| x.as_i64()).collect()),
        _ => None,
    }).unwrap_or_default();
    assert!(!null_locked_ids.contains(&null_neutral_id), "ATS must NOT auto-lock passive neutral player in null-sec");
    assert!(null_locked_ids.contains(&null_aggressor_id), "ATS must auto-lock active offensive aggressor on initial activation");
    let _ = rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ats_id)]).await;
    p.ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&CHAR_ID) { ch.solar_system_id = JITA; } });

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

    // 3. Unloaded Mining Foreman Burst II with charges ONLY in Cargo must fail with NoChargesLoaded (no cargo bypass)
    let (cargo_burst_id, cargo_charge_id) = (9_500_009_i64, 9_500_010_i64);
    insert_item(&p.ctx, cargo_burst_id, 43_551, CHAR_ID, p.ship_id, InventoryFlag::HiSlot4, 1);
    insert_item(&p.ctx, cargo_charge_id, 42_830, CHAR_ID, p.ship_id, InventoryFlag::Cargo, 10);
    p.ctx.invalidate_ship_fitting(p.ship_id);
    let cargo_burst_err = rpc(&mut p, "dogmaIM", None, "Activate", &[PyRep::Integer(cargo_burst_id)]).await.unwrap_err();
    assert!(cargo_burst_err.contains("NoChargesLoaded"), "Unloaded Command Burst must not consume charges from Cargo: {cargo_burst_err}");
    assert_eq!(
        p.ctx.db.with_snapshot(|s| s.items.get(&cargo_charge_id).map(|i| i.quantity)),
        Some(10),
        "Cargo burst charges must remain untouched when burst slot is unloaded"
    );
}

}
