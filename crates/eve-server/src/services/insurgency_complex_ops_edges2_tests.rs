#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::insurgency_services::{
    add_system_corruption, init_insurgency_campaign, with_insurgency_runtime,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
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

async fn spawn_test_ship(ctx: &ServerContext, char_id: i64, ship_id: i64, type_id: u32, sys_id: u32, pos: DVec3) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(sys_id).spawn_ball(Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raid_gate_restrictions_entry_defenders_and_tick_completion() {
    let ctx = test_ctx();
    let sys_id = 30_002_537_u32;
    let cid = 90_000_001_i64;
    let ship_id = 9_500_001_i64;
    let now = ctx.now_unix();
    let camp_id = 910_001_i64;

    init_insurgency_campaign(&ctx, camp_id, 500_010, 500_001, sys_id, vec![sys_id]);
    with_insurgency_runtime(&ctx, |rt| {
        if let Some(c) = rt.campaigns.get_mut(&camp_id) {
            c.forecasting_end_unix = now - 10;
            c.active_end_unix = now + 10_000;
        }
    });
    add_system_corruption(&ctx, camp_id, sys_id, 50_000);
    sync_insurgency_complex_sites_and_facilities(&ctx, now).await;

    let (gate_id, ambush_tackler, ambush_rorq) = ctx.db.with_snapshot(|snap| {
        let st = load_ops_state(snap);
        let first_raid = st
            .outpost_raids
            .values()
            .find(|r| r.solar_system_id == sys_id && !r.completed_on_cooldown)
            .expect("outpost raid spawned");
        let gid = first_raid.gate_id;
        let amb = st.mining_ambushes.values().next();
        (
            gid,
            amb.and_then(|a| a.pirate_tackler_ids.first().copied()),
            amb.map(|a| a.rorqual_ball_id),
        )
    });
    let gate_pos = ctx
        .systems
        .get_or_spawn(sys_id)
        .get_ball_position_sync(gate_id)
        .unwrap_or(DVec3::ZERO);

    assert!(ctx.db.with_snapshot(|s| is_outpost_raid_gate(s, gate_id)));
    assert!(!ctx.db.with_snapshot(|s| is_outpost_raid_gate(s, gate_id + 999_999)));

    let frig_tid = 587_u32;
    spawn_test_ship(&ctx, cid, ship_id, 670, sys_id, gate_pos).await;

    // 1. Not enlisted -> NotEnlistedInMilitia
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.war_faction_id = None;
        }
    });
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, cid, ship_id, sys_id, gate_id)
            .await
            .unwrap_err()
            .code,
        "NotEnlistedInMilitia"
    );

    // 2. Capsule (670) -> ShipRestrictedForComplex
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.war_faction_id = Some(500_010);
        }
    });
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, cid, ship_id, sys_id, gate_id)
            .await
            .unwrap_err()
            .code,
        "ShipRestrictedForComplex"
    );

    // 3. Warp Core Stabilizer (group 315) fitted in slot 11..=19 -> WarpCoreStabilizerRestricted
    let wcs_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 315)
        .map(|t| t.type_id)
        .unwrap_or(11_563);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.type_id = frig_tid;
        }
        snap.items.insert(
            9_500_050,
            InventoryItemRecord {
                item_id: 9_500_050,
                type_id: wcs_tid,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        activate_outpost_raid_gate_in_space(&ctx, cid, ship_id, sys_id, gate_id)
            .await
            .unwrap_err()
            .code,
        "WarpCoreStabilizerRestricted"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&9_500_050);
    });

    // 4. TooFarFromGate (> 2500m)
    let _ = ctx.systems.get_or_spawn(sys_id).remove_ball(ship_id).await;
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: frig_tid,
            position: gate_pos + DVec3::new(100_000.0, 0.0, 0.0),
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
        activate_outpost_raid_gate_in_space(&ctx, cid, ship_id, sys_id, gate_id)
            .await
            .unwrap_err()
            .code,
        "TooFarFromGate"
    );

    // 5. Move within gate range -> activation warps pilot inside and spawns defenders
    let _ = ctx.systems.get_or_spawn(sys_id).remove_ball(ship_id).await;
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: frig_tid,
            position: gate_pos + DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert!(activate_outpost_raid_gate_in_space(&ctx, cid, ship_id, sys_id, gate_id)
        .await
        .is_ok());

    // 6. Test mining ambush NPC destruction & tick completion + orphan cleanup
    if let (Some(tackler), Some(rorq)) = (ambush_tackler, ambush_rorq) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                9_500_082,
                InventoryItemRecord {
                    item_id: 9_500_082,
                    type_id: 26_557,
                    owner_id: cid,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        on_insurgency_npc_destroyed(&ctx, sys_id, tackler, 587, 9_500_081, cid).await;
        on_insurgency_npc_destroyed(&ctx, sys_id, rorq, TYPE_ORE_RORQUAL, 9_500_082, cid).await;
        assert!(ctx.db.with_snapshot(|snap| {
            snap.items.values().any(|i| i.type_id == TYPE_ORE_RORQUAL_WRECK)
        }));
    }

    tick_insurgency_complex_ops(&ctx, now + 5).await;
    despawn_mining_ambush_in_system(&ctx, sys_id).await;
    cleanup_orphan_insurgency_complex_ops(&ctx, &[sys_id]).await;
}

#[test]
fn laundering_noops_and_roamer_wreck_loot_dedup_edges() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let unknown_char = 99_777_666_i64;

    on_insurgency_laundering_trade_completed(&ctx, unknown_char, 30_002_537, 78_868, 1);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.war_faction_id = None;
        }
    });
    on_insurgency_laundering_trade_completed(&ctx, cid, 30_002_537, 78_868, 1);

    assert!(check_insurgency_laundering_access(&ctx, cid, 30_000_142, 34).is_ok());
    assert_eq!(
        check_insurgency_laundering_access(&ctx, cid, 30_000_142, TYPE_GURISTAS_LAUNDERING_FACILITY)
            .unwrap_err()
            .code,
        "FactionNotAligned"
    );

    let mut loot = Vec::new();
    append_insurgency_npc_wreck_loot(79_330, &mut loot);
    let len_after_angel = loot.len();
    assert!(loot.iter().any(|&(tid, _, _)| tid == 78_865));
    // Duplicate Angel call does not append duplicate 78_865
    append_insurgency_npc_wreck_loot(79_330, &mut loot);
    assert_eq!(loot.len(), len_after_angel);
    append_insurgency_npc_wreck_loot(79_358, &mut loot);
    assert!(loot.iter().any(|&(tid, _, _)| tid == 78_859));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raid_pocket_warp_dbuff_latch_gate_response_and_ambush_despawn_kill_mutants() {
    let ctx = test_ctx();
    let sys_a = 30_002_537_u32;
    let sys_b = 30_002_538_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 9_510_001_i64;
    let gate_id = 9_510_100_i64;
    let beacon_id = 9_510_101_i64;

    let bc_tid = ctx
        .sde
        .types
        .values()
        .find(|t| is_battlecruiser_group(t.group_id) && !t.name.contains("Blueprint"))
        .map(|t| t.type_id)
        .unwrap_or(16_227);

    // 1. save_ops_state / load_ops_state exact roundtrip
    let pocket = OutpostRaidSize::Moderate.pocket_position();
    let gate_pos = OutpostRaidSize::Moderate.gate_position();
    let raid = OutpostRaidInstance {
        gate_id,
        beacon_id,
        campaign_id: 700_001,
        solar_system_id: sys_a,
        pirate_faction_id: FACTION_GURISTAS,
        target_faction_id: 500_001,
        size: OutpostRaidSize::Moderate,
        is_static: false,
        occupants: Vec::new(),
        mordus_defender_ball_id: None,
        pirate_defender_ball_id: None,
        mordus_defender_cleared: false,
        pirate_defender_cleared: false,
        weighted_elapsed_secs: 0.0,
        last_step_unix: 0,
        completed_on_cooldown: false,
        respawn_at_unix: 0,
    };
    ctx.db.with_mut_snapshot(|snap| {
        let mut ops = InsurgencyComplexOpsState::default();
        ops.subpoint_remainders.insert("700001:30002537:1".into(), 47);
        ops.outpost_raids.insert(gate_id, raid.clone());
        ops.named_roamer_rolls.insert(format!("{sys_a}:4637"), true);
        ops.named_roamer_rolls.insert(format!("{sys_a}:4638"), false);
        save_ops_state(snap, &ops);
        assert_eq!(snap.insurgency_complex_ops.subpoint_remainders.get("700001:30002537:1"), Some(&47));
        let loaded = load_ops_state(snap);
        assert_eq!(loaded.subpoint_remainders.get("700001:30002537:1"), Some(&47));
        assert!(loaded.outpost_raids.contains_key(&gate_id));
    });

    // 2. is_warp_into_outpost_raid_pocket_blocked: outside->pocket blocked; inside->pocket & outside->outside allowed
    let far = [0.0, 0.0, 0.0];
    let p_arr = [pocket.x, pocket.y, pocket.z];
    assert!(is_warp_into_outpost_raid_pocket_blocked(&ctx, sys_a, far, p_arr, far));
    assert!(is_warp_into_outpost_raid_pocket_blocked(&ctx, sys_a, far, far, p_arr));
    assert!(!is_warp_into_outpost_raid_pocket_blocked(&ctx, sys_a, p_arr, p_arr, p_arr));
    assert!(!is_warp_into_outpost_raid_pocket_blocked(&ctx, sys_a, far, far, far));
    assert!(!is_warp_into_outpost_raid_pocket_blocked(&ctx, sys_b, far, p_arr, p_arr));

    // 3. roll_or_read_named_roamer_latch: cached true, cached false, active=false deletion
    assert!(roll_or_read_named_roamer_latch(&ctx, sys_a, 4637, true));
    assert!(!roll_or_read_named_roamer_latch(&ctx, sys_a, 4638, true));
    assert!(!roll_or_read_named_roamer_latch(&ctx, sys_a, 4637, false));
    assert!(!ctx.db.with_snapshot(|s| s.insurgency_complex_ops.named_roamer_rolls.contains_key(&format!("{sys_a}:4637"))));

    // 4. active_insurgency_pocket_dbuffs empty for non-BC & before gate activation; non-empty inside Moderate pocket after gate entry
    spawn_test_ship(&ctx, char_id, ship_id, bc_tid, sys_a, gate_pos + DVec3::new(100.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(FACTION_GURISTAS);
        }
    });
    let _ = ctx
        .systems
        .get_or_spawn(sys_a)
        .spawn_ball(Ball {
            id: gate_id,
            type_id: TYPE_OUTPOST_RAID_GATE,
            position: gate_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 250.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert!(active_insurgency_pocket_dbuffs(&ctx, char_id, ship_id, 587).is_empty());
    assert!(active_insurgency_pocket_dbuffs(&ctx, char_id, ship_id, bc_tid).is_empty());

    // 5. activate_outpost_raid_gate_in_space full return PyRep & side effects
    let gate_res = activate_outpost_raid_gate_in_space(&ctx, char_id, ship_id, sys_a, gate_id)
        .await
        .expect("gate activation ok");
    assert_eq!(gate_res.dict_get("gateID"), Some(&PyRep::Integer(gate_id)));
    assert_eq!(gate_res.dict_get("activated"), Some(&PyRep::Bool(true)));
    assert_eq!(
        gate_res.dict_get("pocketPosition"),
        Some(&PyRep::List(vec![
            PyRep::Real(pocket.x),
            PyRep::Real(pocket.y),
            PyRep::Real(pocket.z),
        ]))
    );
    let dbuffs = active_insurgency_pocket_dbuffs(&ctx, char_id, ship_id, bc_tid);
    assert_eq!(dbuffs.len(), 1);
    assert_eq!(dbuffs[0].source_item_id, beacon_id);
    assert_eq!(dbuffs[0].dbuff_id, Some(MODERATE_OUTPOST_BC_SKILL_DBUFF_ID));
    assert!((dbuffs[0].value - MODERATE_OUTPOST_BC_SKILL_BONUS_PCT).abs() < 1e-9);

    // 6. despawn_mining_ambush_in_system: no-op on sys_b preserves sys_a; real call on sys_a removes DB & balls
    let amb_beacon = 9_510_200_i64;
    let amb_rorq = 9_510_201_i64;
    let amb_tackler = 9_510_202_i64;
    let amb_def = 9_510_203_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ops = load_ops_state(snap);
        ops.mining_ambushes.insert(
            sys_a,
            MiningAmbushInstance {
                campaign_id: 700_001,
                solar_system_id: sys_a,
                pirate_faction_id: FACTION_GURISTAS,
                target_faction_id: 500_001,
                beacon_id: amb_beacon,
                rorqual_ball_id: amb_rorq,
                pirate_tackler_ids: vec![amb_tackler],
                ore_defender_ids: vec![amb_def],
                ransom_elapsed_secs: 0,
                last_step_unix: 0,
                resolved: false,
                rorqual_warping_off_until: None,
                cooldown_until_unix: 0,
            },
        );
        save_ops_state(snap, &ops);
    });
    let sys_h = ctx.systems.get_or_spawn(sys_a);
    for bid in [amb_beacon, amb_rorq, amb_tackler, amb_def] {
        let _ = sys_h
            .spawn_ball(Ball {
                id: bid,
                type_id: TYPE_ORE_RORQUAL,
                position: DVec3::new(1000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    despawn_mining_ambush_in_system(&ctx, sys_b).await;
    assert!(ctx.db.with_snapshot(|s| s.insurgency_complex_ops.mining_ambushes.contains_key(&sys_a)));

    despawn_mining_ambush_in_system(&ctx, sys_a).await;
    assert!(!ctx.db.with_snapshot(|s| s.insurgency_complex_ops.mining_ambushes.contains_key(&sys_a)));
    assert!(sys_h.get_ball_snapshot(amb_beacon).await.unwrap().is_none());
    assert!(sys_h.get_ball_snapshot(amb_rorq).await.unwrap().is_none());
    assert!(sys_h.get_ball_snapshot(amb_tackler).await.unwrap().is_none());
    assert!(sys_h.get_ball_snapshot(amb_def).await.unwrap().is_none());
}
