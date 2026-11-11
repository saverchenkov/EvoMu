#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::test_ctx;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insurgency_complex_ops_helpers_and_lifecycle_mutations() {
    assert_eq!(OutpostRaidSize::Small.slot_index(), 1);
    assert_eq!(OutpostRaidSize::Medium.slot_index(), 2);
    assert_eq!(OutpostRaidSize::Moderate.slot_index(), 3);
    assert_eq!(OutpostRaidSize::LargeAdv.slot_index(), 4);

    assert_eq!(OutpostRaidSize::Small.beacon_type_id(), TYPE_OUTPOST_BEACON_SMALL);
    assert_eq!(OutpostRaidSize::Medium.beacon_type_id(), TYPE_OUTPOST_BEACON_MEDIUM);
    assert_eq!(OutpostRaidSize::Moderate.beacon_type_id(), TYPE_OUTPOST_BEACON_MODERATE);
    assert_eq!(OutpostRaidSize::LargeAdv.beacon_type_id(), TYPE_OUTPOST_BEACON_LARGE_ADV);

    assert_eq!(OutpostRaidSize::Small.base_capture_duration_secs(), 600);
    assert_eq!(OutpostRaidSize::Medium.base_capture_duration_secs(), 900);
    assert_eq!(OutpostRaidSize::Moderate.base_capture_duration_secs(), 900);
    assert_eq!(OutpostRaidSize::LargeAdv.base_capture_duration_secs(), 900);

    assert!((OutpostRaidSize::Small.contribution_percent() - 0.25).abs() < 1e-6);
    assert!((OutpostRaidSize::Medium.contribution_percent() - 0.5).abs() < 1e-6);
    assert!((OutpostRaidSize::Moderate.contribution_percent() - 1.5).abs() < 1e-6);
    assert!((OutpostRaidSize::LargeAdv.contribution_percent() - 1.0).abs() < 1e-6);

    assert_eq!(OutpostRaidSize::Small.contribution_subpoints(), 250);
    assert_eq!(OutpostRaidSize::Medium.contribution_subpoints(), 500);
    assert_eq!(OutpostRaidSize::Moderate.contribution_subpoints(), 1_500);
    assert_eq!(OutpostRaidSize::LargeAdv.contribution_subpoints(), 1_000);

    assert_eq!(OutpostRaidSize::Small.base_lp_reward(), 29_000);
    assert_eq!(OutpostRaidSize::Medium.base_lp_reward(), 41_500);
    assert_eq!(OutpostRaidSize::Moderate.base_lp_reward(), 52_500);
    assert_eq!(OutpostRaidSize::LargeAdv.base_lp_reward(), 52_500);

    assert_eq!(OutpostRaidSize::Small.respawn_delay_secs(), STANDARD_OUTPOST_RESPAWN_SECS);
    assert_eq!(OutpostRaidSize::LargeAdv.respawn_delay_secs(), LARGE_ADV_OUTPOST_RESPAWN_SECS);

    assert!(OutpostRaidSize::Small.allows_ship_group(25));
    assert!(!OutpostRaidSize::Small.allows_ship_group(26));
    assert!(OutpostRaidSize::Medium.allows_ship_group(26));
    assert!(!OutpostRaidSize::Medium.allows_ship_group(419));
    assert!(OutpostRaidSize::Moderate.allows_ship_group(419));
    assert!(!OutpostRaidSize::Moderate.allows_ship_group(27));
    assert!(OutpostRaidSize::LargeAdv.allows_ship_group(27));

    assert_eq!(OutpostRaidSize::Small.mordus_defender_type_id(), 79_545);
    assert_eq!(OutpostRaidSize::Small.pirate_defender_type_id(FACTION_ANGEL_CARTEL), 79_515);
    assert_eq!(OutpostRaidSize::Small.pirate_defender_type_id(FACTION_GURISTAS), 79_529);

    assert!(is_warzone_opposing_empire(FACTION_ANGEL_CARTEL, 500_002));
    assert!(is_warzone_opposing_empire(FACTION_ANGEL_CARTEL, 500_003));
    assert!(!is_warzone_opposing_empire(FACTION_ANGEL_CARTEL, 500_001));
    assert!(is_warzone_opposing_empire(FACTION_GURISTAS, 500_001));
    assert!(is_warzone_opposing_empire(FACTION_GURISTAS, 500_004));

    assert!(is_battlecruiser_group(419));
    assert!(is_battlecruiser_group(540));
    assert!(is_battlecruiser_group(1201));
    assert!(!is_battlecruiser_group(25));

    assert_eq!(share_site_lp_up_to_5(10_000, 0), 0);
    assert_eq!(share_site_lp_up_to_5(0, 3), 0);
    assert_eq!(share_site_lp_up_to_5(10_000, 1), 10_000);
    assert_eq!(share_site_lp_up_to_5(10_000, 5), 10_000);
    assert_eq!(share_site_lp_up_to_5(10_000, 10), 5_000);

    assert!(is_named_pirate_commander_roamer(79_329));
    assert!(is_named_pirate_commander_roamer(79_358));
    assert!(!is_named_pirate_commander_roamer(79_389));
    assert!(is_named_insurgency_roamer(79_389));

    assert_eq!(
        named_insurgency_roamer_loot_drop(79_329),
        Some((TYPE_ANGEL_INSURGENCY_DATA_SEQ, 1))
    );
    assert_eq!(
        named_insurgency_roamer_loot_drop(79_358),
        Some((TYPE_GURISTAS_INSURGENCY_DATA_SEQ, 1))
    );
    assert_eq!(
        named_insurgency_roamer_loot_drop(79_389),
        Some((TYPE_UPWELL_CONSORTIUM_DATA_CACHE, 1))
    );
    assert_eq!(
        named_insurgency_roamer_loot_drop(TYPE_ORE_RORQUAL),
        Some((TYPE_UPWELL_CONSORTIUM_DATA_CACHE, 3))
    );

    let mut loot = Vec::new();
    append_insurgency_npc_wreck_loot(TYPE_ORE_RORQUAL, &mut loot);
    assert_eq!(loot.len(), 2);

    let ctx = test_ctx();
    // Non-insurgency trader always passes access check
    assert!(check_insurgency_laundering_access(&ctx, 90_000_001, 30000142, 1234).is_ok());
    // Unaligned character fails pirate laundering access
    assert!(check_insurgency_laundering_access(&ctx, 90_000_001, 30000142, TYPE_GURISTAS_LAUNDERING_FACILITY).is_err());
    // Wrong system fails War HQ CI hub access
    assert!(check_insurgency_laundering_access(&ctx, 90_000_001, 30000142, TYPE_STATE_CI_OUTPOST_HUB).is_err());

    assert!(!is_warp_into_outpost_raid_pocket_blocked(
        &ctx,
        30000142,
        [0.0, 0.0, 0.0],
        [1000.0, 0.0, 0.0],
        [1000.0, 0.0, 0.0]
    ));

    assert!(active_insurgency_pocket_dbuffs(&ctx, 90_000_001, 1, 587).is_empty());

    let err = activate_outpost_raid_gate_in_space(&ctx, 90_000_001, 1, 30000142, 999_999)
        .await
        .unwrap_err();
    assert_eq!(err.code, "GateNotFound");

    // Non-positive char_id / lp and non-trader / unaligned laundering calls are safe no-ops
    credit_militia_lp(&ctx, 0, FACTION_GURISTAS, 500);
    credit_militia_lp(&ctx, 90_000_001, FACTION_GURISTAS, 0);
    on_insurgency_laundering_trade_completed(&ctx, 90_000_001, 30000142, 34, 1);
    on_insurgency_laundering_trade_completed(
        &ctx,
        90_000_001,
        30000142,
        TYPE_GURISTAS_LAUNDERING_FACILITY,
        1,
    );
}

fn ensure_sde_type(
    sde: &mut std::sync::Arc<eve_sde::SdeRegistry>,
    type_id: u32,
    group_id: u32,
    faction_id: Option<u32>,
) {
    let reg = std::sync::Arc::make_mut(sde);
    let mut rec = reg
        .types
        .get(&type_id)
        .cloned()
        .or_else(|| reg.types.values().next().cloned())
        .expect("base type");
    rec.type_id = type_id;
    rec.group_id = group_id;
    rec.faction_id = faction_id;
    rec.radius = 50.0;
    reg.types.insert(type_id, rec);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_facilities_ambush_ransom_empire_raid_capture_and_moderate_bc_lifecycle() {
    use crate::services::fw_services::{
        HQ_SYSTEM_AMO, HQ_SYSTEM_INTAKI, HQ_SYSTEM_MEHATOOR, HQ_SYSTEM_ONNAMON,
    };
    use crate::services::insurgency_services::InsurgencyCampaign;
    use eve_db::SystemDynamicState;
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, MovementMode};
    use glam::DVec3;
    use std::collections::HashMap;

    let mut ctx = test_ctx();
    let frig_tid = 941_001_u32;
    let bc_tid = 941_002_u32;
    let roamer_tid = 79_329_u32;
    ensure_sde_type(&mut ctx.sde, frig_tid, 25, None);
    ensure_sde_type(&mut ctx.sde, bc_tid, 419, None);
    ensure_sde_type(&mut ctx.sde, roamer_tid, 4637, Some(FACTION_ANGEL_CARTEL as u32));

    let sys_angel = 30_002_540_u32;
    let sys_gur = 30_002_541_u32;
    let now = ctx.now_unix();

    let emp_char = 95_101_i64;
    let emp_ship = 95_201_i64;
    let pir_char = 95_102_i64;
    let pir_ship = 95_202_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = emp_char;
        ch.solar_system_id = sys_angel;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = emp_ship;
        ch.war_faction_id = Some(500_003); // Amarr opposes Angel Cartel
        ch.loyalty_points.clear();
        snap.characters.insert(emp_char, ch.clone());

        ch.character_id = pir_char;
        ch.solar_system_id = sys_angel;
        ch.active_ship_id = pir_ship;
        ch.war_faction_id = Some(FACTION_ANGEL_CARTEL);
        ch.loyalty_points.clear();
        snap.characters.insert(pir_char, ch);

        for &(shid, tid, oid) in &[(emp_ship, bc_tid, emp_char), (pir_ship, frig_tid, pir_char)] {
            snap.items.insert(
                shid,
                InventoryItemRecord {
                    item_id: shid,
                    type_id: tid,
                    owner_id: oid,
                    location_id: i64::from(sys_angel),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }

        snap.system_states.insert(
            sys_angel,
            SystemDynamicState {
                solar_system_id: sys_angel,
                insurgency_corruption_stage: 1,
                ..Default::default()
            },
        );
        snap.insurgency_runtime.campaigns.insert(
            81,
            InsurgencyCampaign {
                campaign_id: 81,
                pirate_faction_id: FACTION_ANGEL_CARTEL,
                target_faction_id: 500_003,
                fob_solar_system_id: sys_angel,
                systems: vec![sys_angel],
                created_at_unix: now.saturating_sub(3_600),
                forecasting_end_unix: now.saturating_sub(1_800),
                active_end_unix: now + 86_400,
                winddown_end_unix: now + 172_800,
                corruption_points: HashMap::from([(sys_angel, 0)]),
                suppression_points: HashMap::from([(sys_angel, 0)]),
            },
        );
        snap.insurgency_runtime.campaigns.insert(
            82,
            InsurgencyCampaign {
                campaign_id: 82,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_gur,
                systems: vec![sys_gur],
                created_at_unix: now.saturating_sub(3_600),
                forecasting_end_unix: now.saturating_sub(1_800),
                active_end_unix: now + 86_400,
                winddown_end_unix: now + 172_800,
                corruption_points: HashMap::from([(sys_gur, 0)]),
                suppression_points: HashMap::from([(sys_gur, 0)]),
            },
        );
        let mut ops = load_ops_state(snap);
        ops.named_roamer_rolls.insert(format!("{sys_angel}:4637"), true);
        save_ops_state(snap, &ops);
    });

    // 1) sync_insurgency_complex_sites_and_facilities spawns Laundering, HQ Hubs, 4 Outpost Raids, and Mining Ambush in sys_angel
    sync_insurgency_complex_sites_and_facilities(&ctx, now).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&1_891_100_081].type_id, TYPE_ANGEL_LAUNDERING_FACILITY);
        assert_eq!(snap.items[&1_891_100_082].type_id, TYPE_GURISTAS_LAUNDERING_FACILITY);
        assert!(snap.items.contains_key(&1_891_200_001));
        assert!(snap.items.contains_key(&1_891_200_002));
        assert!(snap.items.contains_key(&1_891_200_003));
        assert!(snap.items.contains_key(&1_891_200_004));
        let ops = load_ops_state(snap);
        assert!(ops.mining_ambushes.contains_key(&sys_angel));
        assert!(!ops.mining_ambushes.contains_key(&sys_gur));
        for s in OutpostRaidSize::ALL {
            let gid = 1_893_100_000 + i64::from(sys_angel) * 10 + s.slot_index();
            assert!(is_outpost_raid_gate(snap, gid));
        }
    });
    let _ = (HQ_SYSTEM_AMO, HQ_SYSTEM_INTAKI, HQ_SYSTEM_MEHATOOR, HQ_SYSTEM_ONNAMON);

    // 2) Roamer destruction clears named_roamer_rolls latch
    on_insurgency_npc_destroyed(&ctx, sys_angel, 777_777, roamer_tid, 0, emp_char).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!load_ops_state(snap).named_roamer_rolls.contains_key(&format!("{sys_angel}:4637")));
    });

    // 3) Mining Ambush ransom completion by pirate pilot after ORE defenders are destroyed
    let amb = ctx.db.with_snapshot(|snap| load_ops_state(snap).mining_ambushes[&sys_angel].clone());
    let sys_h = ctx.systems.get_or_spawn(sys_angel);
    for &def_id in &amb.ore_defender_ids {
        let _ = sys_h.remove_ball(def_id).await;
        on_insurgency_npc_destroyed(&ctx, sys_angel, def_id, 79_592, 0, pir_char).await;
    }
    let grid = DVec3::new(1_350_000.0, 0.0, -150_000.0);
    let _ = sys_h
        .spawn_ball(Ball {
            id: pir_ship,
            type_id: frig_tid,
            position: grid + DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ops = load_ops_state(snap);
        let a = ops.mining_ambushes.get_mut(&sys_angel).unwrap();
        a.ransom_elapsed_secs = MINING_AMBUSH_RANSOM_SECS - 1;
        a.last_step_unix = now;
        save_ops_state(snap, &ops);
    });
    tick_insurgency_complex_ops(&ctx, now + 5).await;
    ctx.db.with_snapshot(|snap| {
        let ops = load_ops_state(snap);
        assert!(ops.mining_ambushes[&sys_angel].resolved);
        assert!(ops.mining_ambushes[&sys_angel].rorqual_warping_off_until.is_some());
        assert_eq!(
            snap.insurgency_runtime.campaigns[&81].corruption_points[&sys_angel],
            MINING_AMBUSH_CORRUPTION_PTS
        );
        let pir_lp: i64 = snap.characters[&pir_char].loyalty_points.values().sum();
        assert_eq!(pir_lp, MINING_AMBUSH_BASE_LP);
    });

    // Advance past rorqual_warping_off_until -> sync removes rorqual ball & clears warp timer
    sync_insurgency_complex_sites_and_facilities(&ctx, now + 30).await;
    ctx.db.with_snapshot(|snap| {
        assert!(load_ops_state(snap).mining_ambushes[&sys_angel].rorqual_warping_off_until.is_none());
    });

    // 4) Pod (type 670) rejected by Outpost Raid gate
    let mod_gate_id = 1_893_100_000 + i64::from(sys_angel) * 10 + OutpostRaidSize::Moderate.slot_index();
    let pod_ship = 95_203_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pod_ship,
            InventoryItemRecord {
                item_id: pod_ship,
                type_id: 670,
                owner_id: emp_char,
                location_id: i64::from(sys_angel),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, emp_char, pod_ship, sys_angel, mod_gate_id)
            .await
            .unwrap_err()
            .code,
        "ShipRestrictedForComplex"
    );

    // 5) Empire BC enters Moderate Outpost Raid gate -> spawns pirate defender, clears ship_fittings, grants BC pocket dbuff
    let _ = sys_h
        .spawn_ball(Ball {
            id: emp_ship,
            type_id: bc_tid,
            position: OutpostRaidSize::Moderate.gate_position() + DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 150.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert!(activate_outpost_raid_gate_in_space(&ctx, emp_char, emp_ship, sys_angel, mod_gate_id)
        .await
        .is_ok());
    // Re-activating by same character updates occupant without duplicating
    let _ = sys_h
        .set_ball_position(emp_ship, OutpostRaidSize::Moderate.gate_position())
        .await;
    assert!(activate_outpost_raid_gate_in_space(&ctx, emp_char, emp_ship, sys_angel, mod_gate_id)
        .await
        .is_ok());
    let p_def_id = ctx.db.with_snapshot(|snap| {
        let r = &load_ops_state(snap).outpost_raids[&mod_gate_id];
        assert_eq!(r.occupants.len(), 1);
        r.pirate_defender_ball_id.expect("pirate defender spawned")
    });
    assert_eq!(active_insurgency_pocket_dbuffs(&ctx, emp_char, emp_ship, bc_tid).len(), 1);

    // Contested pocket (both pirate & empire in pocket) pauses capture progress
    let mod_pocket = OutpostRaidSize::Moderate.pocket_position();
    let _ = sys_h.set_ball_position(pir_ship, mod_pocket + DVec3::new(1_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ops = load_ops_state(snap);
        ops.outpost_raids
            .get_mut(&mod_gate_id)
            .unwrap()
            .occupants
            .push((pir_char, pir_ship));
        save_ops_state(snap, &ops);
    });
    tick_insurgency_complex_ops(&ctx, now + 40).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(load_ops_state(snap).outpost_raids[&mod_gate_id].weighted_elapsed_secs, 0.0);
    });

    // Move pirate out of pocket, kill pirate defender, advance tick to complete Empire capture
    let _ = sys_h.set_ball_position(pir_ship, DVec3::ZERO).await;
    let _ = sys_h.remove_ball(p_def_id).await;
    on_insurgency_npc_destroyed(&ctx, sys_angel, p_def_id, 79_523, 0, emp_char).await;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ops = load_ops_state(snap);
        let r = ops.outpost_raids.get_mut(&mod_gate_id).unwrap();
        assert!(r.pirate_defender_cleared);
        r.weighted_elapsed_secs = -895.0;
        r.last_step_unix = now + 40;
        save_ops_state(snap, &ops);
    });
    tick_insurgency_complex_ops(&ctx, now + 50).await;
    ctx.db.with_snapshot(|snap| {
        let ops = load_ops_state(snap);
        let r = &ops.outpost_raids[&mod_gate_id];
        assert!(r.completed_on_cooldown);
        assert!(snap.insurgency_runtime.campaigns[&81].suppression_points[&sys_angel] > 0);
        let emp_lp: i64 = snap.characters[&emp_char].loyalty_points.values().sum();
        assert!(emp_lp >= OutpostRaidSize::Moderate.base_lp_reward());
    });
    // Completed raid yields no pocket dbuffs
    assert!(active_insurgency_pocket_dbuffs(&ctx, emp_char, emp_ship, bc_tid).is_empty());

    // 6) Respawn raid once respawn_at_unix elapses & despawn mining ambush when corruption drops to 0
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&sys_angel).unwrap().insurgency_corruption_stage = 0;
        let mut ops = load_ops_state(snap);
        ops.mining_ambushes.get_mut(&sys_angel).unwrap().resolved = false;
        save_ops_state(snap, &ops);
    });
    let respawn_at = ctx.db.with_snapshot(|snap| {
        load_ops_state(snap).outpost_raids[&mod_gate_id].respawn_at_unix
    });
    sync_insurgency_complex_sites_and_facilities(&ctx, respawn_at + 1).await;
    ctx.db.with_snapshot(|snap| {
        let ops = load_ops_state(snap);
        assert!(!ops.outpost_raids[&mod_gate_id].completed_on_cooldown);
        assert!(!ops.mining_ambushes.contains_key(&sys_angel));
    });
}
