#![forbid(unsafe_code)]
#[allow(unused_imports)]
use super::*;

pub(crate) mod parity_notify_capture_batch {
    #[allow(unused_imports)]
    use super::*;

// End-to-end integration tests for `notify:OnCaptureChanged`.


use super::super::discovery_campaigns_and_cluster_notices::parity_notify_batch::{
    drain_events, new_ctx, rpc, JITA_SYSTEM, PILOT_A, PILOT_B,
};
use crate::common;
use eve_destiny::{Ball, DVec3, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::services::fw_gameplay::get_system_frontline_rules;
use eve_server::sp6_lifecycle::tick_background_cluster;

#[tokio::test]
async fn test_parity_notify_criminal_timer_and_fw_capture_changed() {
    let ctx = new_ctx();
    let unix_to_ft = |u: u64| -> i64 { (u as i64 + 11_644_473_600) * 10_000_000 };

    // 1. OnCriminalTimerUpdate via unprovoked high-sec attack (301 -> 303 -> 300)
    let mut s1 = SessionState::new(92_005);
    common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
    rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)])
        .await
        .expect("select PILOT_A");
    let ship_a = ctx
        .db
        .with_snapshot(|s| s.characters.get(&PILOT_A).map(|c| c.active_ship_id))
        .expect("PILOT_A ship");
    let gun_a = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let gid = s.next_entity_id;
        s.items.insert(
            gid,
            eve_db::InventoryItemRecord {
                item_id: gid,
                type_id: 484,
                owner_id: PILOT_A,
                location_id: ship_a,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.module_online_states.insert(gid, true);
        gid
    });
    ctx.invalidate_ship_fitting(ship_a);
    ctx.advance_secs(11);
    rpc(&ctx, &mut s1, "beyonce", "Undock", &[])
        .await
        .expect("undock PILOT_A");

    let mut s2 = SessionState::new(92_006);
    common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
    rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_B)])
        .await
        .expect("select PILOT_B");
    let ship_b = ctx
        .db
        .with_snapshot(|s| s.characters.get(&PILOT_B).map(|c| c.active_ship_id))
        .expect("PILOT_B ship");
    ctx.advance_secs(11);
    rpc(&ctx, &mut s2, "beyonce", "Undock", &[])
        .await
        .expect("undock PILOT_B");

    let sys_jita = ctx.systems.get_or_spawn(JITA_SYSTEM);
    sys_jita
        .add_target_lock(
            ship_a,
            TargetLock {
                target_id: ship_b,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .expect("lock ship_b");
    let _ = drain_events(&ctx, &mut s1, PILOT_A);

    let attack_now = ctx.now_unix();
    rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_a),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(ship_b),
        ],
    )
    .await
    .expect("high-sec attack");

    let crim_active: Vec<Vec<PyRep>> = drain_events(&ctx, &mut s1, PILOT_A)
        .into_iter()
        .filter(|(e, _)| e == "OnCriminalTimerUpdate")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(
        crim_active,
        vec![vec![PyRep::Integer(301), PyRep::Integer(unix_to_ft(attack_now + 900))]],
        "unprovoked high-sec attack must emit OnCriminalTimerUpdate(301, expiry)"
    );

    // Advance 2s + tick -> countdown Timer state 303
    ctx.advance_secs(2);
    tick_background_cluster(&ctx).await;
    let crim_cd: Vec<Vec<PyRep>> = drain_events(&ctx, &mut s1, PILOT_A)
        .into_iter()
        .filter(|(e, _)| e == "OnCriminalTimerUpdate")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(
        crim_cd,
        vec![vec![PyRep::Integer(303), PyRep::Integer(unix_to_ft(attack_now + 900))]],
        "tick after criminal flag must emit countdown state 303"
    );

    // Advance past 900s + tick -> Idle state 300
    ctx.advance_secs(905);
    tick_background_cluster(&ctx).await;
    let crim_idle: Vec<Vec<PyRep>> = drain_events(&ctx, &mut s1, PILOT_A)
        .into_iter()
        .filter(|(e, _)| e == "OnCriminalTimerUpdate")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(
        crim_idle,
        vec![vec![PyRep::Integer(300), PyRep::None]],
        "expired criminal flag must emit Idle state 300"
    );

    // Suspect branch (302 -> 304 -> 300) via looting PILOT_A's jetcan in high-sec
    let (can_id, loot_id) = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 2;
        let cid = s.next_entity_id - 1;
        let lid = s.next_entity_id;
        s.items.insert(
            cid,
            eve_db::InventoryItemRecord {
                item_id: cid,
                type_id: 23,
                owner_id: PILOT_A,
                location_id: i64::from(JITA_SYSTEM),
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            lid,
            eve_db::InventoryItemRecord {
                item_id: lid,
                type_id: 34,
                owner_id: PILOT_A,
                location_id: cid,
                flag: eve_db::InventoryFlag::None,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        (cid, lid)
    });
    let _ = drain_events(&ctx, &mut s2, PILOT_B);
    let susp_now = ctx.now_unix();
    rpc(
        &ctx,
        &mut s2,
        "invbroker",
        "Add",
        &[PyRep::Integer(loot_id), PyRep::Integer(can_id)],
    )
    .await
    .expect("loot jetcan");
    let susp_active: Vec<Vec<PyRep>> = drain_events(&ctx, &mut s2, PILOT_B)
        .into_iter()
        .filter(|(e, _)| e == "OnCriminalTimerUpdate")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(
        susp_active,
        vec![vec![PyRep::Integer(302), PyRep::Integer(unix_to_ft(susp_now + 900))]],
        "looting another pilot's jetcan must emit OnCriminalTimerUpdate(302, expiry)"
    );

    ctx.advance_secs(2);
    tick_background_cluster(&ctx).await;
    let susp_cd: Vec<Vec<PyRep>> = drain_events(&ctx, &mut s2, PILOT_B)
        .into_iter()
        .filter(|(e, _)| e == "OnCriminalTimerUpdate")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(
        susp_cd,
        vec![vec![PyRep::Integer(304), PyRep::Integer(unix_to_ft(susp_now + 900))]]
    );

    // 2. OnCaptureChanged via real FW complex spawn, gate activation, guard NPC kill, and capture tick
    let (fw_sys, occ_fac) = ctx
        .db
        .with_snapshot(|s| {
            s.fw_system_occupations
                .iter()
                .find(|(&sys_id, occ)| {
                    get_system_frontline_rules(&ctx, sys_id).0 == 1
                        && occ.occupier_faction_id != 500_001
                })
                .map(|(&sys_id, occ)| (sys_id, occ.occupier_faction_id))
        })
        .expect("frontline FW system");
    let attacker_fac = if occ_fac == 500_002 { 500_001 } else { 500_002 };

    // Spawn warzone sites via real background cluster tick
    let sys_fw = ctx.systems.get_or_spawn(fw_sys);
    tick_background_cluster(&ctx).await;

    let (gate_id, gate_pos) = ctx
        .db
        .with_snapshot(|s| {
            let (&gid, _) = s
                .fw_runtime
                .fw_complexes
                .iter()
                .find(|(_, (csys, _, _, _, done))| *csys == fw_sys && !*done)
                .expect("spawned FW complex");
            let pos = s.item_positions.get(&gid).copied().unwrap_or([0.0, 0.0, 0.0]);
            (gid, DVec3::from_array(pos))
        });

    let mut fw_sess = s2;
    let fw_char = PILOT_B;
    let fw_ship = 90_000_177_i64;
    let fw_gun = 90_000_277_i64;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&fw_char) {
            ch.corporation_id = 1_000_180;
            ch.war_faction_id = Some(attacker_fac);
            ch.solar_system_id = fw_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = fw_ship;
        }
        s.items.insert(
            fw_ship,
            eve_db::InventoryItemRecord {
                item_id: fw_ship,
                type_id: 603,
                owner_id: fw_char,
                location_id: i64::from(fw_sys),
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (slot_idx, flag) in [
            eve_db::InventoryFlag::HiSlot0,
            eve_db::InventoryFlag::HiSlot1,
            eve_db::InventoryFlag::HiSlot2,
        ]
        .into_iter()
        .enumerate()
        {
            let gid = fw_gun + (slot_idx as i64) * 2;
            s.items.insert(
                gid,
                eve_db::InventoryItemRecord {
                    item_id: gid,
                    type_id: 484,
                    owner_id: fw_char,
                    location_id: fw_ship,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            s.module_online_states.insert(gid, true);
            let aid = gid + 1;
            s.items.insert(
                aid,
                eve_db::InventoryItemRecord {
                    item_id: aid,
                    type_id: 180,
                    owner_id: fw_char,
                    location_id: fw_ship,
                    flag: eve_db::InventoryFlag::Cargo,
                    quantity: 500,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(fw_ship);
    rpc(&ctx, &mut fw_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(fw_char)])
        .await
        .expect("reselect fw_char in fw_sys");

    sys_fw
        .spawn_ball_with_meta(
            Ball {
                id: fw_ship,
                type_id: 603,
                position: gate_pos + DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            ctx.build_ship_combat_state(fw_char, fw_ship, 603),
            eve_destiny::SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: fw_char,
                char_id: Some(fw_char),
                corp_id: 1_000_180,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await
        .expect("spawn fw_ship");

    rpc(
        &ctx,
        &mut fw_sess,
        "dogmaIM",
        "LoadAmmo",
        &[
            PyRep::Integer(fw_ship),
            PyRep::List(vec![
                PyRep::Integer(fw_gun),
                PyRep::Integer(fw_gun + 2),
                PyRep::Integer(fw_gun + 4),
            ]),
            PyRep::List(vec![
                PyRep::Integer(fw_gun + 1),
                PyRep::Integer(fw_gun + 3),
                PyRep::Integer(fw_gun + 5),
            ]),
        ],
    )
    .await
    .expect("LoadAmmo");
    ctx.advance_secs(11);

    // Activate acceleration gate via keeper.ActivateAccelerationGate
    rpc(
        &ctx,
        &mut fw_sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(gate_id)],
    )
    .await
    .expect("ActivateAccelerationGate");

    let _ = drain_events(&ctx, &mut fw_sess, fw_char);

    // Negative: while guard NPC is alive, capture does not progress
    ctx.advance_secs(5);
    tick_background_cluster(&ctx).await;
    assert!(
        !drain_events(&ctx, &mut fw_sess, fw_char)
            .iter()
            .any(|(e, _)| e == "OnCaptureChanged"),
        "capture must not progress while guard NPC is alive"
    );

    // Kill guard NPC via target lock + dogmaIM.Activate + Destiny system ticks
    let guard_id = gate_id + 500_000_000;
    sys_fw
        .add_target_lock(
            fw_ship,
            TargetLock {
                target_id: guard_id,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .expect("lock guard_id");
    for gid in [fw_gun, fw_gun + 2, fw_gun + 4] {
        rpc(
            &ctx,
            &mut fw_sess,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(gid),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(guard_id),
                PyRep::Integer(1_000),
            ],
        )
        .await
        .expect("activate gun on guard NPC");
    }
    for _ in 0..600 {
        if sys_fw.get_ball_snapshot(guard_id).await.ok().flatten().is_none() {
            break;
        }
        let _ = sys_fw.tick().await;
    }
    assert!(
        sys_fw.get_ball_snapshot(guard_id).await.ok().flatten().is_none(),
        "guard NPC must be destroyed"
    );

    // Positive: tick_background_cluster accrues capture and emits 6-arg OnCaptureChanged
    let _ = drain_events(&ctx, &mut fw_sess, fw_char);
    ctx.advance_secs(5);
    tick_background_cluster(&ctx).await;

    let cap_notifs: Vec<Vec<PyRep>> = drain_events(&ctx, &mut fw_sess, fw_char)
        .into_iter()
        .filter(|(e, _)| e == "OnCaptureChanged")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(cap_notifs.len(), 1, "must emit OnCaptureChanged after guard cleared");
    assert_eq!(cap_notifs[0].len(), 6, "OnCaptureChanged expects 6 args");
    assert_eq!(cap_notifs[0][0], PyRep::Integer(gate_id));
    assert_eq!(cap_notifs[0][1], PyRep::Integer(attacker_fac));
    assert!(matches!(cap_notifs[0][2], PyRep::Integer(_)));
    assert!(cap_notifs[0][3].as_f64().unwrap_or(0.0) > 0.0);
    assert!(cap_notifs[0][4].as_f64().unwrap_or(0.0) > 0.0);
    assert_eq!(cap_notifs[0][5], PyRep::Integer(attacker_fac));
}
}

