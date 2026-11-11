//! Additional unit and mutation-killing tests for `insurgency_complex_ops.rs`.

#![forbid(unsafe_code)]

use super::insurgency_complex_ops::*;
use crate::router::ServerContext;
use crate::services::fw_services::{HQ_SYSTEM_AMO, HQ_SYSTEM_INTAKI, HQ_SYSTEM_MEHATOOR};
use crate::services::insurgency_services::{
    InsurgencyCampaign, FACTION_ANGEL_CARTEL, FACTION_GURISTAS,
};
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

pub(super) fn ensure_test_type(sde: &mut Arc<SdeRegistry>, type_id: u32, group_id: u32, faction_id: Option<u32>) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg
        .types
        .get(&type_id)
        .cloned()
        .or_else(|| reg.types.values().next().cloned())
        .expect("base sde type");
    rec.type_id = type_id;
    rec.group_id = group_id;
    rec.faction_id = faction_id;
    rec.radius = 50.0;
    reg.types.insert(type_id, rec);
}

#[test]
fn outpost_raid_size_defenders_ship_groups_roamers_and_loot_cover_all_branches() {
    assert_eq!(OutpostRaidSize::Medium.mordus_defender_type_id(), 79_549);
    assert_eq!(OutpostRaidSize::Moderate.mordus_defender_type_id(), 79_553);
    assert_eq!(OutpostRaidSize::LargeAdv.mordus_defender_type_id(), 79_557);

    assert_eq!(OutpostRaidSize::Medium.pirate_defender_type_id(FACTION_ANGEL_CARTEL), 79_520);
    assert_eq!(OutpostRaidSize::Moderate.pirate_defender_type_id(FACTION_ANGEL_CARTEL), 79_523);
    assert_eq!(OutpostRaidSize::LargeAdv.pirate_defender_type_id(FACTION_ANGEL_CARTEL), 79_526);

    assert_eq!(OutpostRaidSize::Medium.pirate_defender_type_id(FACTION_GURISTAS), 79_533);
    assert_eq!(OutpostRaidSize::Moderate.pirate_defender_type_id(FACTION_GURISTAS), 79_537);
    assert_eq!(OutpostRaidSize::LargeAdv.pirate_defender_type_id(FACTION_GURISTAS), 79_541);

    for s in OutpostRaidSize::ALL {
        let g = s.gate_position();
        let p = s.pocket_position();
        assert!((g.x - p.x).abs() < 1e-6);
        assert!((p.z - 2_500_000.0).abs() < 1e-6);
    }

    // T3 Destroyer (1305), T3 Cruiser (963), Battlecruiser (540), Battleship (898), Capital (485)
    assert!(!OutpostRaidSize::Small.allows_ship_group(1305));
    assert!(OutpostRaidSize::Medium.allows_ship_group(1305));
    assert!(!OutpostRaidSize::Medium.allows_ship_group(963));
    assert!(OutpostRaidSize::Moderate.allows_ship_group(540));
    assert!(!OutpostRaidSize::Moderate.allows_ship_group(963));
    assert!(OutpostRaidSize::LargeAdv.allows_ship_group(963));
    assert!(OutpostRaidSize::LargeAdv.allows_ship_group(898));
    assert!(!OutpostRaidSize::LargeAdv.allows_ship_group(485));

    // Wreck loot deduplication
    let mut loot = vec![(TYPE_UPWELL_CONSORTIUM_DATA_CACHE, 3, false)];
    append_insurgency_npc_wreck_loot(TYPE_ORE_RORQUAL, &mut loot);
    assert_eq!(loot.len(), 2);
    assert!(loot.iter().any(|&(tid, qty, _)| tid == 17_867 && qty == 5));

    let mut ctx = test_ctx();
    for &tid in &[79_329u32, 79_330, 79_332, 79_334] {
        ensure_test_type(&mut ctx.sde, tid, 4637, Some(FACTION_ANGEL_CARTEL as u32));
    }
    for &tid in &[79_358u32, 79_360, 79_362, 79_364] {
        ensure_test_type(&mut ctx.sde, tid, 4637, Some(FACTION_GURISTAS as u32));
    }
    ensure_test_type(&mut ctx.sde, 79_300, 4637, Some(FACTION_ANGEL_CARTEL as u32));
    for &tid in &[79_394u32, 79_389, 79_395, 79_396] {
        ensure_test_type(&mut ctx.sde, tid, 4638, Some(500_001));
    }

    let sys_id = 30000142u32;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ops = load_ops_state(snap);
        ops.named_roamer_rolls.insert(format!("{sys_id}:4637"), true);
        ops.named_roamer_rolls.insert(format!("{sys_id}:4638"), true);
        save_ops_state(snap, &ops);
    });
    assert_eq!(
        select_named_or_regular_roamer_type(&ctx, sys_id, 4637, FACTION_ANGEL_CARTEL, 3, 0),
        Some(79_329)
    );
    assert_eq!(
        select_named_or_regular_roamer_type(&ctx, sys_id, 4637, FACTION_GURISTAS, 5, 0),
        Some(79_362)
    );
    assert_eq!(
        select_named_or_regular_roamer_type(&ctx, sys_id, 4638, FACTION_GURISTAS, 0, 4),
        Some(79_389)
    );
    // Stage < 3 clears latch; stage == 2 returns regular roamer
    assert_eq!(
        select_named_or_regular_roamer_type(&ctx, sys_id, 4637, FACTION_ANGEL_CARTEL, 2, 0),
        Some(79_300)
    );
    assert_eq!(
        select_named_or_regular_roamer_type(&ctx, sys_id, 4637, FACTION_ANGEL_CARTEL, 1, 0),
        None
    );
    assert_eq!(
        select_named_or_regular_roamer_type(&ctx, sys_id, 4638, FACTION_ANGEL_CARTEL, 0, 2),
        None
    );
}

#[tokio::test]
async fn warp_blocking_pocket_dbuffs_and_laundering_trades_work_for_pirates_and_empires() {
    let mut ctx = test_ctx();
    let bc_tid = 940_001u32;
    let frig_tid = 940_002u32;
    ensure_test_type(&mut ctx.sde, bc_tid, 419, None);
    ensure_test_type(&mut ctx.sde, frig_tid, 25, None);

    let sys_id = 30002537u32;
    let gate_id = 1_893_999_003i64;
    let beacon_id = gate_id + 1_000_000;
    let pir_char = 94_101i64;
    let emp_char = 94_102i64;
    let ship_id = 94_201i64;
    let pocket = OutpostRaidSize::Moderate.pocket_position();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = pir_char;
        ch.war_faction_id = Some(FACTION_ANGEL_CARTEL);
        ch.loyalty_points.clear();
        snap.characters.insert(pir_char, ch.clone());

        ch.character_id = emp_char;
        ch.war_faction_id = Some(500_003); // Amarr opposes Angel Cartel
        ch.loyalty_points.clear();
        snap.characters.insert(emp_char, ch);

        snap.insurgency_runtime.campaigns.insert(
            77,
            InsurgencyCampaign {
                campaign_id: 77,
                pirate_faction_id: FACTION_ANGEL_CARTEL,
                target_faction_id: 500_003,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: now.saturating_sub(3_600),
                forecasting_end_unix: now.saturating_sub(1_800),
                active_end_unix: now + 86_400,
                winddown_end_unix: now + 172_800,
                corruption_points: HashMap::from([(sys_id, 0)]),
                suppression_points: HashMap::from([(sys_id, 0)]),
            },
        );
        snap.item_positions.insert(ship_id, [pocket.x, pocket.y, pocket.z]);
        let mut ops = load_ops_state(snap);
        ops.outpost_raids.insert(
            gate_id,
            OutpostRaidInstance {
                gate_id,
                beacon_id,
                campaign_id: 77,
                solar_system_id: sys_id,
                pirate_faction_id: FACTION_ANGEL_CARTEL,
                target_faction_id: 500_003,
                size: OutpostRaidSize::Moderate,
                is_static: false,
                occupants: vec![(pir_char, ship_id)],
                mordus_defender_ball_id: None,
                pirate_defender_ball_id: None,
                mordus_defender_cleared: false,
                pirate_defender_cleared: false,
                weighted_elapsed_secs: 0.0,
                last_step_unix: now,
                completed_on_cooldown: false,
                respawn_at_unix: 0,
            },
        );
        save_ops_state(snap, &ops);
    });

    // Warp from outside pocket into pocket is blocked; warp when already inside pocket is allowed
    assert!(is_warp_into_outpost_raid_pocket_blocked(
        &ctx,
        sys_id,
        [0.0, 0.0, 0.0],
        [pocket.x, pocket.y, pocket.z],
        [pocket.x, pocket.y, pocket.z],
    ));
    assert!(!is_warp_into_outpost_raid_pocket_blocked(
        &ctx,
        sys_id,
        [pocket.x + 1_000.0, pocket.y, pocket.z],
        [pocket.x, pocket.y, pocket.z],
        [pocket.x, pocket.y, pocket.z],
    ));

    // Moderate raid pocket dbuff applies to BC, not to Frigate
    let dbuffs = active_insurgency_pocket_dbuffs(&ctx, pir_char, ship_id, bc_tid);
    assert_eq!(dbuffs.len(), 1);
    assert_eq!(dbuffs[0].dbuff_id, Some(MODERATE_OUTPOST_BC_SKILL_DBUFF_ID));
    assert!((dbuffs[0].value - MODERATE_OUTPOST_BC_SKILL_BONUS_PCT).abs() < 1e-6);
    assert!(active_insurgency_pocket_dbuffs(&ctx, pir_char, ship_id, frig_tid).is_empty());

    // Pirate laundering access & trade completion
    assert_eq!(
        check_insurgency_laundering_access(&ctx, pir_char, 30000142, TYPE_ANGEL_LAUNDERING_FACILITY)
            .unwrap_err()
            .code,
        "InvalidLaunderingLocation"
    );
    assert!(check_insurgency_laundering_access(&ctx, pir_char, sys_id, TYPE_ANGEL_LAUNDERING_FACILITY).is_ok());
    on_insurgency_laundering_trade_completed(&ctx, pir_char, sys_id, TYPE_ANGEL_LAUNDERING_FACILITY, 2);

    // Empire CI Outpost Hub access & trade completion across all 4 hubs
    assert_eq!(
        check_insurgency_laundering_access(&ctx, emp_char, HQ_SYSTEM_INTAKI, TYPE_FEDERATION_CI_OUTPOST_HUB)
            .unwrap_err()
            .code,
        "FactionNotAligned"
    );
    assert_eq!(
        check_insurgency_laundering_access(&ctx, emp_char, HQ_SYSTEM_AMO, TYPE_REPUBLIC_CI_OUTPOST_HUB)
            .unwrap_err()
            .code,
        "FactionNotAligned"
    );
    assert!(check_insurgency_laundering_access(&ctx, emp_char, HQ_SYSTEM_MEHATOOR, TYPE_IMPERIAL_CI_OUTPOST_HUB).is_ok());
    on_insurgency_laundering_trade_completed(&ctx, emp_char, HQ_SYSTEM_MEHATOOR, TYPE_IMPERIAL_CI_OUTPOST_HUB, 3);

    ctx.db.with_snapshot(|snap| {
        let camp = &snap.insurgency_runtime.campaigns[&77];
        assert_eq!(camp.corruption_points[&sys_id], LAUNDERING_CAMPAIGN_PTS_PER_TRADE * 2);
        assert_eq!(camp.suppression_points[&sys_id], LAUNDERING_CAMPAIGN_PTS_PER_TRADE * 3);
        let pir_lp: i64 = snap.characters[&pir_char].loyalty_points.values().sum();
        let emp_lp: i64 = snap.characters[&emp_char].loyalty_points.values().sum();
        assert_eq!(pir_lp, LAUNDERING_BASE_LP_PER_TRADE * 2);
        assert_eq!(emp_lp, LAUNDERING_BASE_LP_PER_TRADE * 3);
    });
}

fn ops_ctx_with_clock(now_unix: u64, extra_systems: &[u32]) -> (ServerContext, Arc<eve_db::VirtualServerClock>) {
    let base = test_ctx();
    let mut sde = Arc::clone(&base.sde);
    {
        let reg = Arc::make_mut(&mut sde);
        for &sid in extra_systems {
            reg.solar_systems.entry(sid).or_insert_with(|| eve_sde::SolarSystem {
                solar_system_id: sid,
                region_id: 10_000_001,
                constellation_id: 20_000_001,
                name: format!("InsSys_{sid}"),
                security_status: 0.3,
                radius: 1.0e12,
                galactic_pos_ly: [0.0; 3],
                planet_ids: Vec::new(),
                stations: Vec::new(),
                wormhole_class_id: None,
            });
        }
    }
    ensure_test_type(&mut sde, 90_315, 315, None); // Warp Core Stabilizer
    ensure_test_type(&mut sde, 90_419, 419, None); // Attack Battlecruiser
    ensure_test_type(&mut sde, 90_540, 540, None); // Command Ship
    ensure_test_type(&mut sde, 91_201, 1201, None); // Combat Battlecruiser
    ensure_test_type(&mut sde, 90_587, 25, None); // Frigate
    ensure_test_type(&mut sde, 90_898, 898, None); // Black Ops / BS
    ensure_test_type(&mut sde, 670, 29, None); // Capsule
    ensure_test_type(&mut sde, 79_553, 1803, Some(10_001)); // Moderate Mordu's Defender
    ensure_test_type(&mut sde, 79_537, 1803, Some(500_010)); // Moderate Guristas Defender
    ensure_test_type(&mut sde, TYPE_ORE_RORQUAL, 28, Some(10_001));
    ensure_test_type(&mut sde, TYPE_ORE_RORQUAL_WRECK, 186, None);

    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let clock = Arc::new(eve_db::VirtualServerClock::with_fixed_base(now_unix));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn eve_db::ServerClock>);
    (ctx, clock)
}

fn ops_seed_pilot(
    snap: &mut eve_db::WorldStateSnapshot,
    cid: i64,
    ship_id: i64,
    ship_type: u32,
    war_fac: Option<i64>,
    sys_id: u32,
) {
    let mut ch = snap.characters.values().next().expect("seed char").clone();
    ch.character_id = cid;
    ch.user_id = cid as u64;
    ch.name = format!("OpsPilot_{cid}");
    ch.corporation_id = 1_000_009;
    ch.war_faction_id = war_fac;
    ch.solar_system_id = sys_id;
    ch.station_id = None;
    ch.structure_id = None;
    ch.active_ship_id = ship_id;
    ch.loyalty_points.clear();
    snap.characters.insert(cid, ch);
    snap.items.insert(
        ship_id,
        eve_db::InventoryItemRecord {
            item_id: ship_id,
            type_id: ship_type,
            owner_id: cid,
            location_id: sys_id as i64,
            flag: eve_db::InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            ..eve_db::InventoryItemRecord::default()
        },
    );
}

fn ops_ball(id: i64, type_id: u32, pos: glam::DVec3, radius: f64) -> eve_destiny::Ball {
    eve_destiny::Ball {
        id,
        type_id,
        position: pos,
        velocity: glam::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    }
}

#[tokio::test]
async fn warzone_opposing_warp_block_dbuffs_rng_roamer_loot_and_ci_hubs_complete() {
    use crate::services::fw_services::HQ_SYSTEM_ONNAMON;
    use crate::services::insurgency_services::{PHASE_ACTIVE, PHASE_FORECASTING};

    let (ctx, _) = ops_ctx_with_clock(1_000_000, &[30_099_101, 30_099_102]);

    // 1. is_warzone_opposing_empire negative & positive coverage
    assert!(is_warzone_opposing_empire(FACTION_GURISTAS, 500_001));
    assert!(is_warzone_opposing_empire(FACTION_GURISTAS, 500_004));
    assert!(!is_warzone_opposing_empire(FACTION_GURISTAS, 500_002));
    assert!(!is_warzone_opposing_empire(FACTION_GURISTAS, 500_003));
    assert!(!is_warzone_opposing_empire(FACTION_GURISTAS, FACTION_GURISTAS));
    assert!(is_warzone_opposing_empire(FACTION_ANGEL_CARTEL, 500_002));
    assert!(is_warzone_opposing_empire(FACTION_ANGEL_CARTEL, 500_003));
    assert!(!is_warzone_opposing_empire(FACTION_ANGEL_CARTEL, 500_001));
    assert!(!is_warzone_opposing_empire(FACTION_ANGEL_CARTEL, 500_004));
    assert!(!is_warzone_opposing_empire(500_001, 500_002));

    // 2. is_warp_into_outpost_raid_pocket_blocked: wrong sys, cooldown skip, dest-inside-while-anchor-outside, 75_000 vs 75_001 radius
    let sys_id = 30_099_101u32;
    let pocket = OutpostRaidSize::Moderate.pocket_position();
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_ops_state(snap);
        st.outpost_raids.insert(
            770_001,
            OutpostRaidInstance {
                gate_id: 770_001,
                beacon_id: 770_002,
                campaign_id: 1,
                solar_system_id: sys_id,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                size: OutpostRaidSize::Moderate,
                is_static: false,
                occupants: vec![(88_001, 880_001)],
                mordus_defender_ball_id: None,
                pirate_defender_ball_id: None,
                mordus_defender_cleared: false,
                pirate_defender_cleared: false,
                weighted_elapsed_secs: 0.0,
                last_step_unix: 1_000_000,
                completed_on_cooldown: false,
                respawn_at_unix: 0,
            },
        );
        st.outpost_raids.insert(
            770_011,
            OutpostRaidInstance {
                gate_id: 770_011,
                beacon_id: 770_012,
                campaign_id: 1,
                solar_system_id: sys_id,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                size: OutpostRaidSize::Small,
                is_static: false,
                occupants: vec![],
                mordus_defender_ball_id: None,
                pirate_defender_ball_id: None,
                mordus_defender_cleared: false,
                pirate_defender_cleared: false,
                weighted_elapsed_secs: 0.0,
                last_step_unix: 1_000_000,
                completed_on_cooldown: true,
                respawn_at_unix: 2_000_000,
            },
        );
        save_ops_state(snap, &st);
    });
    let small_p = OutpostRaidSize::Small.pocket_position();
    // Completed raid on cooldown does NOT block
    assert!(!is_warp_into_outpost_raid_pocket_blocked(
        &ctx,
        sys_id,
        [0.0; 3],
        small_p.to_array(),
        small_p.to_array()
    ));
    // Wrong system does NOT block
    assert!(!is_warp_into_outpost_raid_pocket_blocked(
        &ctx,
        30_099_102,
        [0.0; 3],
        pocket.to_array(),
        pocket.to_array()
    ));
    // Anchor outside (> 75_000), dest inside (<= 75_000) -> blocks! Exact 75_000.0 vs 75_001.0 boundary
    let edge_in = (pocket + glam::DVec3::new(OUTPOST_RAID_POCKET_RADIUS_M, 0.0, 0.0)).to_array();
    let edge_out = (pocket + glam::DVec3::new(OUTPOST_RAID_POCKET_RADIUS_M + 1.0, 0.0, 0.0)).to_array();
    assert!(is_warp_into_outpost_raid_pocket_blocked(
        &ctx, sys_id, [0.0; 3], edge_out, edge_in
    ));
    assert!(!is_warp_into_outpost_raid_pocket_blocked(
        &ctx, sys_id, [0.0; 3], edge_out, edge_out
    ));

    // 3. active_insurgency_pocket_dbuffs: all BC groups 419/540/1201, live ball 75_000 vs 75_001, missing ball, non-occupant
    let ship_id = 880_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        ops_seed_pilot(snap, 88_001, ship_id, 90_419, Some(FACTION_GURISTAS), sys_id);
        ops_seed_pilot(snap, 88_002, 880_002, 90_419, Some(FACTION_GURISTAS), sys_id);
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    // Ship ball not yet in space -> empty dbuffs
    assert!(active_insurgency_pocket_dbuffs(&ctx, 88_001, ship_id, 90_419).is_empty());
    // Spawn ship ball at exact 75_000.0 from Moderate pocket
    sys_h
        .spawn_ball(ops_ball(ship_id, 90_419, glam::DVec3::from_array(edge_in), 40.0))
        .await
        .unwrap();
    for bc_type in [90_419, 90_540, 91_201] {
        let d = active_insurgency_pocket_dbuffs(&ctx, 88_001, ship_id, bc_type);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].dbuff_id, Some(MODERATE_OUTPOST_BC_SKILL_DBUFF_ID));
        assert!((d[0].value - MODERATE_OUTPOST_BC_SKILL_BONUS_PCT).abs() < 1e-6);
    }
    // Non-occupant char -> empty
    assert!(active_insurgency_pocket_dbuffs(&ctx, 88_002, ship_id, 90_419).is_empty());
    // Non-BC hull -> empty
    assert!(active_insurgency_pocket_dbuffs(&ctx, 88_001, ship_id, 90_587).is_empty());
    // Move ball outside 75_001.0 -> empty
    sys_h.try_set_ball_position(ship_id, glam::DVec3::from_array(edge_out));
    assert!(active_insurgency_pocket_dbuffs(&ctx, 88_001, ship_id, 90_419).is_empty());

    // 4. select_named_or_regular_roamer_type unseeded RNG path, c_st/s_st clamping, group 4637 vs 4638 & unknown, !active clears latch
    assert_eq!(
        select_named_or_regular_roamer_type(&ctx, sys_id, 99_999, FACTION_GURISTAS, 3, 3),
        None
    );
    let mut saw_promoted = false;
    let mut saw_unpromoted_none = false;
    let mut saw_regular = false;
    for idx in 0..40 {
        let test_sys = 30_099_200 + idx;
        if let Some(tid) = select_named_or_regular_roamer_type(&ctx, test_sys, 4637, FACTION_ANGEL_CARTEL, 7, 1) {
            assert!(is_named_insurgency_roamer(tid));
            saw_promoted = true;
        } else {
            saw_unpromoted_none = true;
        }
        let latched = ctx.db.with_snapshot(|s| load_ops_state(s).named_roamer_rolls.contains_key(&format!("{test_sys}:4637")));
        assert!(latched);
        // Dropping both stages < 3 clears the latch cleanly; stage 2 returns regular roamer, stage 1 returns None
        let reg = select_named_or_regular_roamer_type(&ctx, test_sys, 4637, FACTION_ANGEL_CARTEL, 2, 2);
        assert!(reg.is_some_and(|tid| !is_named_insurgency_roamer(tid)));
        saw_regular = true;
        let cleared = ctx.db.with_snapshot(|s| !load_ops_state(s).named_roamer_rolls.contains_key(&format!("{test_sys}:4637")));
        assert!(cleared);
        assert_eq!(select_named_or_regular_roamer_type(&ctx, test_sys, 4637, FACTION_ANGEL_CARTEL, 1, 1), None);
    }
    assert!(saw_promoted && saw_unpromoted_none && saw_regular);
    // Group 4638 when s_st >= 3 (empire suppression roamer) across RNG rolls + latch clearing
    let mut saw_4638_promoted = false;
    let mut saw_4638_none = false;
    for idx in 0..40 {
        let test_sys = 30_099_300 + idx;
        if let Some(tid) = select_named_or_regular_roamer_type(&ctx, test_sys, 4638, FACTION_GURISTAS, 0, 5) {
            assert!(is_named_insurgency_roamer(tid));
            saw_4638_promoted = true;
        } else {
            saw_4638_none = true;
        }
        assert!(ctx.db.with_snapshot(|s| load_ops_state(s).named_roamer_rolls.contains_key(&format!("{test_sys}:4638"))));
        assert_eq!(select_named_or_regular_roamer_type(&ctx, test_sys, 4638, FACTION_GURISTAS, 0, 2), None);
        assert!(ctx.db.with_snapshot(|s| !load_ops_state(s).named_roamer_rolls.contains_key(&format!("{test_sys}:4638"))));
    }
    assert!(saw_4638_promoted && saw_4638_none);

    // 5. append_insurgency_npc_wreck_loot: Angel roamer, Guristas roamer, dedup, fresh Rorqual, and unrelated 587
    let mut angel_loot = Vec::new();
    append_insurgency_npc_wreck_loot(79_329, &mut angel_loot);
    append_insurgency_npc_wreck_loot(79_329, &mut angel_loot);
    assert_eq!(angel_loot.len(), 2);
    assert!(angel_loot.iter().any(|&(t, _, _)| t == TYPE_ANGEL_INSURGENCY_DATA_SEQ));
    assert!(angel_loot.iter().any(|&(t, _, _)| t == 78_865));

    let mut gur_loot = Vec::new();
    append_insurgency_npc_wreck_loot(79_364, &mut gur_loot);
    append_insurgency_npc_wreck_loot(79_364, &mut gur_loot);
    assert_eq!(gur_loot.len(), 2);
    assert!(gur_loot.iter().any(|&(t, _, _)| t == TYPE_GURISTAS_INSURGENCY_DATA_SEQ));
    assert!(gur_loot.iter().any(|&(t, _, _)| t == 78_859));

    let mut rorq_loot = Vec::new();
    append_insurgency_npc_wreck_loot(TYPE_ORE_RORQUAL, &mut rorq_loot);
    append_insurgency_npc_wreck_loot(TYPE_ORE_RORQUAL, &mut rorq_loot);
    assert_eq!(
        rorq_loot,
        vec![
            (TYPE_UPWELL_CONSORTIUM_DATA_CACHE, 3, false),
            (17_867, 5, false),
        ]
    );
    append_insurgency_npc_wreck_loot(587, &mut rorq_loot);
    assert_eq!(rorq_loot.len(), 2);

    // 6. check_insurgency_laundering_access & on_insurgency_laundering_trade_completed across all 4 CI hubs & Guristas/Forecasting errors
    ctx.db.with_mut_snapshot(|snap| {
        ops_seed_pilot(snap, 88_100, 881_100, 90_587, None, sys_id);
        ops_seed_pilot(snap, 88_101, 881_101, 90_587, Some(FACTION_GURISTAS), sys_id);
        ops_seed_pilot(snap, 88_102, 881_102, 90_587, Some(500_001), HQ_SYSTEM_ONNAMON);
        ops_seed_pilot(snap, 88_103, 881_103, 90_587, Some(500_002), HQ_SYSTEM_AMO);
        ops_seed_pilot(snap, 88_104, 881_104, 90_587, Some(500_003), HQ_SYSTEM_INTAKI);
        ops_seed_pilot(snap, 88_105, 881_105, 90_587, Some(500_004), HQ_SYSTEM_MEHATOOR);
        snap.insurgency_runtime.campaigns.insert(
            99,
            InsurgencyCampaign {
                campaign_id: 99,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: 900_000,
                forecasting_end_unix: 1_500_000, // currently in PHASE_FORECASTING!
                active_end_unix: 2_000_000,
                winddown_end_unix: 2_500_000,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
    });
    // Non-enlisted pirate trader rejected
    assert_eq!(
        check_insurgency_laundering_access(&ctx, 88_100, sys_id, TYPE_GURISTAS_LAUNDERING_FACILITY)
            .unwrap_err()
            .code,
        "FactionNotAligned"
    );
    // Forecasting campaign rejects pirate laundering access (`InvalidLaunderingLocation`) & does not award corruption points on trade
    assert_eq!(
        check_insurgency_laundering_access(&ctx, 88_101, sys_id, TYPE_GURISTAS_LAUNDERING_FACILITY)
            .unwrap_err()
            .code,
        "InvalidLaunderingLocation"
    );
    on_insurgency_laundering_trade_completed(&ctx, 88_101, sys_id, TYPE_GURISTAS_LAUNDERING_FACILITY, 0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.insurgency_runtime.campaigns[&99].corruption_points.get(&sys_id).copied(), None);
        // Multiplier 0 clamped to 1 for LP
        assert!(snap.characters[&88_101].loyalty_points.values().sum::<i64>() >= LAUNDERING_BASE_LP_PER_TRADE);
    });

    // Switch campaign 99 to PHASE_ACTIVE and test TYPE_GURISTAS_LAUNDERING_FACILITY + STATE_CI_OUTPOST_HUB + REPUBLIC_CI_OUTPOST_HUB + unknown char no-op
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.get_mut(&99).unwrap().forecasting_end_unix = 950_000;
        let _ = PHASE_ACTIVE;
        let _ = PHASE_FORECASTING;
    });
    assert!(check_insurgency_laundering_access(&ctx, 88_101, sys_id, TYPE_GURISTAS_LAUNDERING_FACILITY).is_ok());
    on_insurgency_laundering_trade_completed(&ctx, 999_999_999, sys_id, TYPE_GURISTAS_LAUNDERING_FACILITY, 1);
    on_insurgency_laundering_trade_completed(&ctx, 88_100, sys_id, TYPE_GURISTAS_LAUNDERING_FACILITY, 1);
    assert!(check_insurgency_laundering_access(&ctx, 88_102, HQ_SYSTEM_ONNAMON, TYPE_STATE_CI_OUTPOST_HUB).is_ok());
    assert!(check_insurgency_laundering_access(&ctx, 88_103, HQ_SYSTEM_AMO, TYPE_REPUBLIC_CI_OUTPOST_HUB).is_ok());
    assert_eq!(
        check_insurgency_laundering_access(&ctx, 88_102, HQ_SYSTEM_AMO, TYPE_REPUBLIC_CI_OUTPOST_HUB)
            .unwrap_err()
            .code,
        "FactionNotAligned"
    );
    assert_eq!(
        check_insurgency_laundering_access(&ctx, 88_102, HQ_SYSTEM_MEHATOOR, TYPE_STATE_CI_OUTPOST_HUB)
            .unwrap_err()
            .code,
        "InvalidWarHQLocation"
    );
    on_insurgency_laundering_trade_completed(&ctx, 88_102, HQ_SYSTEM_ONNAMON, TYPE_STATE_CI_OUTPOST_HUB, -5);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&99].suppression_points[&sys_id]),
        LAUNDERING_CAMPAIGN_PTS_PER_TRADE
    );
}

#[tokio::test]
async fn outpost_raid_gate_activation_all_error_and_success_branches() {
    let (ctx, _) = ops_ctx_with_clock(2_000_000, &[30_099_150]);
    let sys_id = 30_099_150u32;
    let small_gate = 780_001i64;
    let mod_gate = 780_002i64;
    let cool_gate = 780_003i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_ops_state(snap);
        for (gid, sz, cool) in [
            (small_gate, OutpostRaidSize::Small, false),
            (mod_gate, OutpostRaidSize::Moderate, false),
            (cool_gate, OutpostRaidSize::Small, true),
        ] {
            st.outpost_raids.insert(
                gid,
                OutpostRaidInstance {
                    gate_id: gid,
                    beacon_id: gid + 100,
                    campaign_id: 201,
                    solar_system_id: sys_id,
                    pirate_faction_id: FACTION_GURISTAS,
                    target_faction_id: 500_001,
                    size: sz,
                    is_static: false,
                    occupants: Vec::new(),
                    mordus_defender_ball_id: None,
                    pirate_defender_ball_id: None,
                    mordus_defender_cleared: false,
                    pirate_defender_cleared: false,
                    weighted_elapsed_secs: 0.0,
                    last_step_unix: 2_000_000,
                    completed_on_cooldown: cool,
                    respawn_at_unix: if cool { 2_000_900 } else { 0 },
                },
            );
        }
        save_ops_state(snap, &st);

        ops_seed_pilot(snap, 89_001, 890_001, 90_587, None, sys_id); // Unaligned
        ops_seed_pilot(snap, 89_002, 890_002, 90_587, Some(500_002), sys_id); // Wrong militia (Minmatar in Guristas-Caldari)
        ops_seed_pilot(snap, 89_003, 890_003, 670, Some(FACTION_GURISTAS), sys_id); // Capsule
        ops_seed_pilot(snap, 89_004, 890_004, 90_898, Some(FACTION_GURISTAS), sys_id); // Oversized BS for Small
        ops_seed_pilot(snap, 89_005, 890_005, 90_419, Some(FACTION_GURISTAS), sys_id); // Moderate BC
        ops_seed_pilot(snap, 89_006, 890_006, 90_587, Some(500_001), sys_id); // Opposing Empire Caldari
        // Put WCS on 890_005 in LoSlot0 (flag 11) and verify later moving to Cargo (5) unlocks gate!
        snap.items.insert(
            890_999,
            eve_db::InventoryItemRecord {
                item_id: 890_999,
                type_id: 90_315,
                owner_id: 89_005,
                location_id: 890_005,
                flag: eve_db::InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..eve_db::InventoryItemRecord::default()
            },
        );
    });

    // 1. is_outpost_raid_gate true/false + GateNotFound (missing, cooldown, wrong system)
    assert!(ctx.db.with_snapshot(|s| is_outpost_raid_gate(s, small_gate)));
    assert!(!ctx.db.with_snapshot(|s| is_outpost_raid_gate(s, 999_999)));
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, 89_005, 890_005, sys_id, 999_999)
            .await
            .unwrap_err()
            .code,
        "GateNotFound"
    );
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, 89_005, 890_005, sys_id, cool_gate)
            .await
            .unwrap_err()
            .code,
        "GateNotFound"
    );
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, 89_005, 890_005, 30_099_151, small_gate)
            .await
            .unwrap_err()
            .code,
        "GateNotFound"
    );

    // 2. Militia / WCS / Capsule / ShipSize checks
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, 89_001, 890_001, sys_id, small_gate)
            .await
            .unwrap_err()
            .code,
        "NotEnlistedInMilitia"
    );
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, 89_002, 890_002, sys_id, small_gate)
            .await
            .unwrap_err()
            .code,
        "NotEnlistedInMilitia"
    );
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, 89_005, 890_005, sys_id, mod_gate)
            .await
            .unwrap_err()
            .code,
        "WarpCoreStabilizerRestricted"
    );
    // Moving WCS to Cargo clears WCS error
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&890_999).unwrap().flag = eve_db::InventoryFlag::Cargo;
    });
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, 89_003, 890_003, sys_id, small_gate)
            .await
            .unwrap_err()
            .code,
        "ShipRestrictedForComplex"
    );
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, 89_004, 890_004, sys_id, small_gate)
            .await
            .unwrap_err()
            .code,
        "ShipRestrictedForComplex"
    );

    // 3. ShipNotInSpace -> WarpScrambled -> TooFarFromGate (2501 vs 2500) -> Pirate & Empire Success
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, 89_005, 890_005, sys_id, mod_gate)
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let mod_gate_pos = OutpostRaidSize::Moderate.gate_position();
    sys_h
        .spawn_ball(ops_ball(mod_gate, TYPE_OUTPOST_RAID_GATE, mod_gate_pos, 0.0))
        .await
        .unwrap();
    // Scrambled BC ship
    sys_h
        .spawn_ball_with_combat(
            ops_ball(890_005, 90_419, mod_gate_pos, 40.0),
            eve_destiny::CombatState {
                warp_scramble_strength: 1,
                ..eve_destiny::CombatState::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, 89_005, 890_005, sys_id, mod_gate)
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );

    // Unscrambled but 2501 m surface distance -> TooFarFromGate; 2500 m -> Ok!
    let _ = sys_h.remove_ball(890_005).await;
    sys_h
        .spawn_ball(ops_ball(
            890_005,
            90_419,
            mod_gate_pos + glam::DVec3::new(2_541.0, 0.0, 0.0),
            40.0,
        ))
        .await
        .unwrap();
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, 89_005, 890_005, sys_id, mod_gate)
            .await
            .unwrap_err()
            .code,
        "TooFarFromGate"
    );

    sys_h.try_set_ball_position(890_005, mod_gate_pos + glam::DVec3::new(2_540.0, 0.0, 0.0));
    activate_outpost_raid_gate_in_space(&ctx, 89_005, 890_005, sys_id, mod_gate)
        .await
        .expect("pirate BC enters moderate raid");
    let first_mordus = ctx.db.with_snapshot(|s| {
        let r = &load_ops_state(s).outpost_raids[&mod_gate];
        assert_eq!(r.occupants, vec![(89_005, 890_005)]);
        r.mordus_defender_ball_id.expect("mordus spawned")
    });

    // Re-activating with a second ship swaps occupant's ship_id without duplicating entry or defender
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            890_015,
            eve_db::InventoryItemRecord {
                item_id: 890_015,
                type_id: 90_540,
                owner_id: 89_005,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..eve_db::InventoryItemRecord::default()
            },
        );
    });
    sys_h
        .spawn_ball(ops_ball(890_015, 90_540, mod_gate_pos, 40.0))
        .await
        .unwrap();
    activate_outpost_raid_gate_in_space(&ctx, 89_005, 890_015, sys_id, mod_gate)
        .await
        .expect("re-entry updates ship_id");
    ctx.db.with_snapshot(|s| {
        let r = &load_ops_state(s).outpost_raids[&mod_gate];
        assert_eq!(r.occupants, vec![(89_005, 890_015)]);
        assert_eq!(r.mordus_defender_ball_id, Some(first_mordus));
    });

    // Opposing Empire entry spawns pirate_defender_ball_id
    sys_h
        .spawn_ball(ops_ball(890_006, 90_587, mod_gate_pos, 40.0))
        .await
        .unwrap();
    activate_outpost_raid_gate_in_space(&ctx, 89_006, 890_006, sys_id, mod_gate)
        .await
        .expect("empire frigate enters moderate raid");
    assert!(ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&mod_gate].pirate_defender_ball_id.is_some()));
}

#[tokio::test]
async fn sync_mining_ambush_outpost_ticks_subpoints_and_orphan_cleanup_full_lifecycle() {
    use crate::services::insurgency_services::add_system_corruption;
    use eve_db::clock::ServerClock;

    let mut now = 3_000_000u64;
    let (ctx, clock) = ops_ctx_with_clock(now, &[30_099_301, 30_099_302]);
    let sys_gur = 30_099_301u32;
    let sys_ang = 30_099_302u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            301,
            InsurgencyCampaign {
                campaign_id: 301,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_gur,
                systems: vec![sys_gur],
                created_at_unix: now - 10_000,
                forecasting_end_unix: now - 1_000,
                active_end_unix: now + 100_000,
                winddown_end_unix: now + 200_000,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
        snap.insurgency_runtime.campaigns.insert(
            302,
            InsurgencyCampaign {
                campaign_id: 302,
                pirate_faction_id: FACTION_ANGEL_CARTEL,
                target_faction_id: 500_002,
                fob_solar_system_id: sys_ang,
                systems: vec![sys_ang],
                created_at_unix: now - 10_000,
                forecasting_end_unix: now - 1_000,
                active_end_unix: now + 100_000,
                winddown_end_unix: now + 200_000,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
        ops_seed_pilot(snap, 91_001, 910_001, 90_587, Some(FACTION_GURISTAS), sys_gur);
        ops_seed_pilot(snap, 91_002, 910_002, 90_587, Some(500_001), sys_gur);
    });

    // 1. At c_st == 0, sync spawns Outpost Raids across sizes without Mining Ambush
    sync_insurgency_complex_sites_and_facilities(&ctx, now).await;
    ctx.db.with_snapshot(|snap| {
        let st = load_ops_state(snap);
        assert!(st.mining_ambushes.is_empty());
        assert!(st.outpost_raids.values().any(|r| r.solar_system_id == sys_gur && r.size == OutpostRaidSize::Small));
        assert!(st.outpost_raids.values().any(|r| r.solar_system_id == sys_ang));
    });

    // Raise Guristas campaign sys corruption to Stage 1 -> sync spawns Mining Ambush!
    let _ = add_system_corruption(&ctx, 301, sys_gur, 10_000);
    sync_insurgency_complex_sites_and_facilities(&ctx, now).await;
    let amb1 = ctx.db.with_snapshot(|s| load_ops_state(s).mining_ambushes[&sys_gur].clone());
    assert!(!amb1.pirate_tackler_ids.is_empty());
    assert!(!amb1.ore_defender_ids.is_empty());

    // Despawn & re-sync when corruption back at 0 -> ambush stays gone; bump corruption back to test 3 outcomes
    despawn_mining_ambush_in_system(&ctx, sys_gur).await;
    assert!(ctx.db.with_snapshot(|s| !load_ops_state(s).mining_ambushes.contains_key(&sys_gur)));
    sync_insurgency_complex_sites_and_facilities(&ctx, now).await;
    let amb_rorq = ctx.db.with_snapshot(|s| load_ops_state(s).mining_ambushes[&sys_gur].clone());

    // Outcome 1: Rorqual killed -> retypes wreck to TYPE_ORE_RORQUAL_WRECK & adds +50 corruption
    let wreck_id = 919_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_id,
            eve_db::InventoryItemRecord {
                item_id: wreck_id,
                type_id: TYPE_ORE_RORQUAL,
                owner_id: 91_001,
                location_id: sys_gur as i64,
                flag: eve_db::InventoryFlag::Hangar,
                ..eve_db::InventoryItemRecord::default()
            },
        );
    });
    let sys_gur_h = ctx.systems.get_or_spawn(sys_gur);
    sys_gur_h
        .spawn_ball(ops_ball(wreck_id, TYPE_ORE_RORQUAL, glam::DVec3::ZERO, 100.0))
        .await
        .unwrap();
    let cor_before = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&301].corruption_points[&sys_gur]);
    on_insurgency_npc_destroyed(&ctx, sys_gur, amb_rorq.rorqual_ball_id, TYPE_ORE_RORQUAL, wreck_id, 91_001).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&wreck_id].type_id),
        TYPE_ORE_RORQUAL_WRECK
    );
    assert_eq!(
        sys_gur_h.get_ball_snapshot(wreck_id).await.unwrap().unwrap().1.type_id,
        TYPE_ORE_RORQUAL_WRECK
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&301].corruption_points[&sys_gur]),
        cor_before + MINING_AMBUSH_CORRUPTION_PTS
    );
    // Clean up resolved ambush & clear cooldown to respawn next Mining Ambush
    tick_insurgency_complex_ops(&ctx, now + 20).await;
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_ops_state(snap);
        st.mining_ambushes.remove(&sys_gur);
        save_ops_state(snap, &st);
    });
    sync_insurgency_complex_sites_and_facilities(&ctx, now + 20).await;

    // Outcome 2: Kill all pirate tacklers -> Rorqual released, +50 suppression + LP via killer_char_id fallback -> warp-off cleans ambush!
    let amb_tack = ctx.db.with_snapshot(|s| load_ops_state(s).mining_ambushes[&sys_gur].clone());
    for tid in amb_tack.pirate_tackler_ids {
        let _ = sys_gur_h.remove_ball(tid).await;
        on_insurgency_npc_destroyed(&ctx, sys_gur, tid, 79_537, 0, 91_002).await;
    }
    assert!(ctx.db.with_snapshot(|s| {
        s.insurgency_runtime.campaigns[&301].suppression_points.get(&sys_gur).copied().unwrap_or(0)
            >= MINING_AMBUSH_SUPPRESSION_PTS
            && s.characters[&91_002].loyalty_points.values().sum::<i64>() >= MINING_AMBUSH_BASE_LP
    }));
    tick_insurgency_complex_ops(&ctx, now + 40).await; // past rorqual_warping_off_until (+10/+15s) -> removed!
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_ops_state(snap);
        st.mining_ambushes.remove(&sys_gur);
        save_ops_state(snap, &st);
    });
    sync_insurgency_complex_sites_and_facilities(&ctx, now + 40).await;

    // Outcome 3: Kill all ORE defenders, then tick MINING_AMBUSH_RANSOM_SECS with pirate ship near Rorqual -> +50 corruption + LP -> warp-off cleanup
    let amb_def = ctx.db.with_snapshot(|s| load_ops_state(s).mining_ambushes[&sys_gur].clone());
    let rorq_pos = sys_gur_h.get_ball_position_sync(amb_def.rorqual_ball_id).unwrap();
    sys_gur_h.spawn_ball(ops_ball(910_001, 90_587, rorq_pos, 40.0)).await.unwrap();
    for did in amb_def.ore_defender_ids {
        let _ = sys_gur_h.remove_ball(did).await;
        on_insurgency_npc_destroyed(&ctx, sys_gur, did, 79_553, 0, 91_001).await;
    }
    now += MINING_AMBUSH_RANSOM_SECS + 50;
    clock.advance_secs(MINING_AMBUSH_RANSOM_SECS + 50);
    tick_insurgency_complex_ops(&ctx, now).await;
    assert!(ctx.db.with_snapshot(|s| s.characters[&91_001].loyalty_points.values().sum::<i64>() >= MINING_AMBUSH_BASE_LP));
    tick_insurgency_complex_ops(&ctx, now + 20).await;

    // 2. Outpost Raid tick progression: contested pause, defender clear (auto & NPC kill), pirate Small capture subpoint rollover (250 -> +2 pts + 50 rem), and empire reversal
    let small_gid = ctx.db.with_snapshot(|s| {
        load_ops_state(s)
            .outpost_raids
            .values()
            .find(|r| r.solar_system_id == sys_gur && r.size == OutpostRaidSize::Small)
            .expect("small raid")
            .gate_id
    });
    let small_pocket = OutpostRaidSize::Small.pocket_position();
    sys_gur_h.spawn_ball(ops_ball(910_002, 90_587, small_pocket, 40.0)).await.unwrap();
    sys_gur_h.try_set_ball_position(910_001, small_pocket);
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_ops_state(snap);
        let r = st.outpost_raids.get_mut(&small_gid).unwrap();
        r.occupants = vec![(91_001, 910_001), (91_002, 910_002)];
        r.last_step_unix = now;
        save_ops_state(snap, &st);
    });
    // Contested pocket (both 91_001 & 91_002 present) pauses elapsed timer even after advancing clock
    tick_insurgency_complex_ops(&ctx, now + 200).await;
    assert!((ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&small_gid].weighted_elapsed_secs)).abs() < 1e-6);

    // Move empire pilot out -> Tick 1 spawns Mordu's defender -> destroy defender -> Tick 2 (dt=650 >= 600) completes Small raid -> awards +250 subpoints (2 pts, 50 rem)
    sys_gur_h.try_set_ball_position(910_002, glam::DVec3::ZERO);
    let cor_pre_raid = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&301].corruption_points[&sys_gur]);
    tick_insurgency_complex_ops(&ctx, now + 210).await;
    let spawned_mordus = ctx
        .db
        .with_snapshot(|s| load_ops_state(s).outpost_raids[&small_gid].mordus_defender_ball_id)
        .expect("mordus defender spawned");
    let _ = sys_gur_h.remove_ball(spawned_mordus).await;
    on_insurgency_npc_destroyed(&ctx, sys_gur, spawned_mordus, 79_545, 0, 91_001).await;

    tick_insurgency_complex_ops(&ctx, now + 850).await;
    ctx.db.with_snapshot(|s| {
        let st = load_ops_state(s);
        let r = &st.outpost_raids[&small_gid];
        assert!(r.completed_on_cooldown);
        assert_eq!(r.respawn_at_unix, now + 850 + STANDARD_OUTPOST_RESPAWN_SECS);
        assert_eq!(st.subpoint_remainders.get(&format!("301:{sys_gur}:1")).copied(), Some(50));
        assert_eq!(s.insurgency_runtime.campaigns[&301].corruption_points[&sys_gur], cor_pre_raid + 2);
    });

    // Before respawn_at_unix -> stays on cooldown; at >= respawn_at_unix -> sync respawns!
    sync_insurgency_complex_sites_and_facilities(&ctx, now + 850 + STANDARD_OUTPOST_RESPAWN_SECS - 1).await;
    assert!(ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&small_gid].completed_on_cooldown));
    sync_insurgency_complex_sites_and_facilities(&ctx, now + 850 + STANDARD_OUTPOST_RESPAWN_SECS).await;
    assert!(ctx.db.with_snapshot(|s| !load_ops_state(s).outpost_raids[&small_gid].completed_on_cooldown));

    // Empire reverses progress & captures Small raid to completion (with defender cleared) + clears orphan system ops!
    let respawn_t = now + 850 + STANDARD_OUTPOST_RESPAWN_SECS;
    sys_gur_h.try_set_ball_position(910_001, glam::DVec3::ZERO);
    sys_gur_h.try_set_ball_position(910_002, small_pocket);
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_ops_state(snap);
        let r = st.outpost_raids.get_mut(&small_gid).unwrap();
        r.occupants = vec![(91_002, 910_002)];
        r.pirate_defender_cleared = true;
        r.last_step_unix = respawn_t;
        save_ops_state(snap, &st);
    });
    tick_insurgency_complex_ops(&ctx, respawn_t + 650).await;
    assert!(ctx.db.with_snapshot(|s| {
        load_ops_state(s).outpost_raids[&small_gid].completed_on_cooldown
            && s.insurgency_runtime.campaigns[&301].suppression_points[&sys_gur] >= 2
    }));

    // 3. cleanup_orphan_insurgency_complex_ops removes all state in orphaned Angel system
    assert!(ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids.values().any(|r| r.solar_system_id == sys_ang)));
    cleanup_orphan_insurgency_complex_ops(&ctx, &[sys_ang]).await;
    assert!(ctx.db.with_snapshot(|s| !load_ops_state(s).outpost_raids.values().any(|r| r.solar_system_id == sys_ang)));
}
