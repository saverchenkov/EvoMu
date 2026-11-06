//! Additional unit and mutation-killing tests for `insurgency_complex_ops.rs`.

#![forbid(unsafe_code)]

use super::insurgency_complex_ops::*;
use crate::router::ServerContext;
use crate::services::fw_services::{HQ_SYSTEM_AMO, HQ_SYSTEM_INTAKI, HQ_SYSTEM_MEHATOOR};
use crate::services::insurgency_services::{
    InsurgencyCampaign, FACTION_ANGEL_CARTEL, FACTION_GURISTAS,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
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

fn ensure_test_type(sde: &mut Arc<SdeRegistry>, type_id: u32, group_id: u32, faction_id: Option<u32>) {
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

#[tokio::test]
async fn activate_outpost_raid_gate_validates_all_restrictions_and_spawns_defender() {
    let mut ctx = test_ctx();
    let frig_tid = 940_010u32;
    let bs_tid = 940_011u32;
    let wcs_tid = 940_012u32;
    ensure_test_type(&mut ctx.sde, frig_tid, 25, None);
    ensure_test_type(&mut ctx.sde, bs_tid, 27, None);
    ensure_test_type(&mut ctx.sde, wcs_tid, 315, None);

    let sys_id = 30002538u32;
    let gate_id = 1_893_999_011i64;
    let beacon_id = gate_id + 1_000_000;
    let char_id = 94_301i64;
    let ship_id = 94_302i64;
    let wcs_item_id = 94_303i64;
    let gpos = OutpostRaidSize::Small.gate_position();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.war_faction_id = None;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: bs_tid,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            wcs_item_id,
            InventoryItemRecord {
                item_id: wcs_item_id,
                type_id: wcs_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut ops = load_ops_state(snap);
        ops.outpost_raids.insert(
            gate_id,
            OutpostRaidInstance {
                gate_id,
                beacon_id,
                campaign_id: 78,
                solar_system_id: sys_id,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                size: OutpostRaidSize::Small,
                is_static: false,
                occupants: Vec::new(),
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

    assert!(ctx.db.with_snapshot(|snap| is_outpost_raid_gate(snap, gate_id)));

    // 1) NotEnlistedInMilitia
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, sys_id, gate_id)
            .await
            .unwrap_err()
            .code,
        "NotEnlistedInMilitia"
    );

    // 2) WarpCoreStabilizerRestricted
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(FACTION_GURISTAS);
    });
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, sys_id, gate_id)
            .await
            .unwrap_err()
            .code,
        "WarpCoreStabilizerRestricted"
    );

    // 3) ShipRestrictedForComplex (Battleship in Small raid)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&wcs_item_id);
    });
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, sys_id, gate_id)
            .await
            .unwrap_err()
            .code,
        "ShipRestrictedForComplex"
    );

    // 4) ShipNotInSpace
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = frig_tid;
    });
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, sys_id, gate_id)
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // 5) TooFarFromGate (> 2500m)
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: frig_tid,
            position: gpos + DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, sys_id, gate_id)
            .await
            .unwrap_err()
            .code,
        "TooFarFromGate"
    );

    // 6) Within 2500m -> teleports to pocket and spawns Mordu's defender
    let _ = sys_handle.set_ball_position(ship_id, gpos + DVec3::new(500.0, 0.0, 0.0)).await;
    let res = activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, sys_id, gate_id)
        .await
        .expect("gate activation ok");
    assert!(format!("{res:?}").contains("activated"));
    ctx.db.with_snapshot(|snap| {
        let r = &load_ops_state(snap).outpost_raids[&gate_id];
        assert!(r.occupants.contains(&(char_id, ship_id)));
        assert!(r.mordus_defender_ball_id.is_some());
    });
}

#[tokio::test]
async fn mining_ambush_and_outpost_raid_npc_kills_tick_completion_and_orphan_cleanup() {
    let mut ctx = test_ctx();
    let frig_tid = 940_020u32;
    ensure_test_type(&mut ctx.sde, frig_tid, 25, None);

    let sys_id = 30002539u32;
    let pir_char = 94_401i64;
    let pir_ship = 94_402i64;
    let emp_char = 94_403i64;
    let wreck_id = 94_404i64;
    let rorq_id = 94_410i64;
    let tackler_id = 94_411i64;
    let beacon_id = 1_892_100_000 + i64::from(sys_id);
    let gate_id = 1_893_100_000 + i64::from(sys_id) * 10 + 1;
    let raid_beacon_id = gate_id + 1_000_000;
    let def_id = 94_412i64;
    let pocket = OutpostRaidSize::Small.pocket_position();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = pir_char;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = pir_ship;
        ch.war_faction_id = Some(FACTION_GURISTAS);
        ch.loyalty_points.clear();
        snap.characters.insert(pir_char, ch.clone());

        ch.character_id = emp_char;
        ch.active_ship_id = 0;
        ch.war_faction_id = Some(500_001); // Caldari opposes Guristas
        ch.loyalty_points.clear();
        snap.characters.insert(emp_char, ch);

        snap.items.insert(
            pir_ship,
            InventoryItemRecord {
                item_id: pir_ship,
                type_id: frig_tid,
                owner_id: pir_char,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            wreck_id,
            InventoryItemRecord {
                item_id: wreck_id,
                type_id: 26557,
                owner_id: pir_char,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.insurgency_runtime.campaigns.insert(
            79,
            InsurgencyCampaign {
                campaign_id: 79,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
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
        let mut ops = load_ops_state(snap);
        ops.mining_ambushes.insert(
            sys_id,
            MiningAmbushInstance {
                campaign_id: 79,
                solar_system_id: sys_id,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                beacon_id,
                rorqual_ball_id: rorq_id,
                pirate_tackler_ids: vec![tackler_id],
                ore_defender_ids: Vec::new(),
                ransom_elapsed_secs: 0,
                last_step_unix: now,
                resolved: false,
                rorqual_warping_off_until: None,
                cooldown_until_unix: 0,
            },
        );
        ops.outpost_raids.insert(
            gate_id,
            OutpostRaidInstance {
                gate_id,
                beacon_id: raid_beacon_id,
                campaign_id: 79,
                solar_system_id: sys_id,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                size: OutpostRaidSize::Small,
                is_static: false,
                occupants: vec![(pir_char, pir_ship)],
                mordus_defender_ball_id: Some(def_id),
                pirate_defender_ball_id: None,
                mordus_defender_cleared: false,
                pirate_defender_cleared: false,
                weighted_elapsed_secs: 590.0,
                last_step_unix: now,
                completed_on_cooldown: false,
                respawn_at_unix: 0,
            },
        );
        save_ops_state(snap, &ops);
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: pir_ship,
            type_id: frig_tid,
            position: pocket,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // 1) Killing outpost defender marks mordus_defender_cleared = true
    on_insurgency_npc_destroyed(&ctx, sys_id, def_id, 79_545, 0, pir_char).await;
    ctx.db.with_snapshot(|snap| {
        assert!(load_ops_state(snap).outpost_raids[&gate_id].mordus_defender_cleared);
    });

    // 2) Killing last pirate tackler resolves ambush for Empire and awards suppression points & LP
    on_insurgency_npc_destroyed(&ctx, sys_id, tackler_id, 79_597, 0, emp_char).await;
    ctx.db.with_snapshot(|snap| {
        assert!(load_ops_state(snap).mining_ambushes[&sys_id].resolved);
        assert_eq!(
            snap.insurgency_runtime.campaigns[&79].suppression_points[&sys_id],
            MINING_AMBUSH_SUPPRESSION_PTS
        );
    });

    // 3) Reset ambush for Rorqual kill test -> stamps TYPE_ORE_RORQUAL_WRECK and adds corruption
    ctx.db.with_mut_snapshot(|snap| {
        let mut ops = load_ops_state(snap);
        let amb = ops.mining_ambushes.get_mut(&sys_id).unwrap();
        amb.resolved = false;
        amb.cooldown_until_unix = 0;
        save_ops_state(snap, &ops);
    });
    on_insurgency_npc_destroyed(&ctx, sys_id, rorq_id, TYPE_ORE_RORQUAL, wreck_id, pir_char).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&wreck_id].type_id, TYPE_ORE_RORQUAL_WRECK);
        assert_eq!(
            snap.insurgency_runtime.campaigns[&79].corruption_points[&sys_id],
            MINING_AMBUSH_CORRUPTION_PTS
        );
    });

    // 4) tick_insurgency_complex_ops completes the Small outpost raid capture for pir_char
    tick_insurgency_complex_ops(&ctx, now + 20).await;
    ctx.db.with_snapshot(|snap| {
        let r = &load_ops_state(snap).outpost_raids[&gate_id];
        assert!(r.completed_on_cooldown);
        let pir_lp: i64 = snap.characters[&pir_char].loyalty_points.values().sum();
        assert_eq!(pir_lp, OutpostRaidSize::Small.base_lp_reward());
    });

    // 5) cleanup_orphan_insurgency_complex_ops removes ambushes and raids for the system
    cleanup_orphan_insurgency_complex_ops(&ctx, &[sys_id]).await;
    ctx.db.with_snapshot(|snap| {
        let ops = load_ops_state(snap);
        assert!(!ops.mining_ambushes.contains_key(&sys_id));
        assert!(!ops.outpost_raids.contains_key(&gate_id));
    });
}
