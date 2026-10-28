//! Part 4 of Equinox Sovereignty closure tests: Dev Indices, ADM Vulnerability, Live Entosis & Command Nodes (`SO-5`).

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::structure;
use crate::sovereignty_equinox_gameplay_closure::{
    drain_notif_events, find_nullsec_planets, fit_siege_artillery, lock_target_via_rpc, make_ball,
    open_sqlite_ctx, register_online_pilot, rpc_call, temp_sqlite_path, ALLIANCE, CORP, PILOT,
    PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::PyRep;
use eve_server::services::sov_gameplay::{
    ENTOSIS_HUB_BASE_CAPTURE_SECS, ENTOSIS_NODE_BASE_CAPTURE_SECS,
};
use eve_server::services::sov_services::{claim_system_sovereignty, TYPE_SOVEREIGNTY_HUB};
use glam::DVec3;

#[tokio::test]
async fn test_sov_dev_indices_adm_vulnerability_live_entosis_and_command_nodes() {
    // parity: feat:sovereignty.sov-dev-indices
    // parity: feat:sovereignty.sov-vulnerability
    // parity: feat:sovereignty.sov-entosis
    // parity: feat:sovereignty.sov-command-nodes
    eve_testkit::attest(&[
        "feat:sovereignty.sov-dev-indices",
        "feat:sovereignty.sov-vulnerability",
        "feat:sovereignty.sov-entosis",
        "feat:sovereignty.sov-command-nodes",
    ]);

    let db_path = temp_sqlite_path("sov_entosis_nodes");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, _, _, _, _) = find_nullsec_planets(&ctx);
    let hub_id = 1_070_000_001i64;
    let hub_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_def = register_online_pilot(
        &ctx, 74_001, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 639, hub_pos + DVec3::new(2_500.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.system_states.entry(null_sys).or_default();
        entry.solar_system_id = null_sys;
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(hub_id, TYPE_SOVEREIGNTY_HUB, hub_pos)).await;
    claim_system_sovereignty(&ctx, null_sys, ALLIANCE, CORP, hub_id).expect("claim sov");

    // 1. Live Military (NPC rat kill), Industrial (asteroid mining), and Strategic (claimed days) Indices + ADM!
    fit_siege_artillery(&ctx, PILOT, PILOT_SHIP, 996_000);
    ctx.db.with_mut_snapshot(|snap| {
        // Free `HiSlot2` turret hardpoint on Naglfar (3 turret hardpoints) so `Miner I` (`483`) stays online.
        snap.items.remove(&996_004);
        snap.items.remove(&996_005);
        snap.module_online_states.remove(&996_004);
        snap.skills.entry(PILOT).or_default().insert(
            3_386,
            TrainedSkillRecord { type_id: 3_386, level: 5, skill_points: 256_000.0 },
        );
        snap.items.insert(
            996_050,
            InventoryItemRecord {
                item_id: 996_050,
                type_id: 483, // Miner I
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::HiSlot2,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(996_050, true);
    });
    ctx.invalidate_ship_fitting(PILOT_SHIP);
    let cs_def = ctx.build_ship_combat_state(PILOT, PILOT_SHIP, 19_722);
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(make_ball(PILOT_SHIP, 19_722, hub_pos + DVec3::new(2_500.0, 0.0, 0.0)), cs_def).await;

    let rat_id = 880_001i64;
    let mut rat_cs = ctx.build_type_combat_state(24_690);
    rat_cs.shield_hp = 100.0;
    rat_cs.armor_hp = 100.0;
    rat_cs.structure_hp = 100.0;
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(make_ball(rat_id, 24_690, hub_pos + DVec3::new(3_500.0, 0.0, 0.0)), rat_cs).await;
    lock_target_via_rpc(&ctx, &mut s_def, null_sys, PILOT_SHIP, rat_id).await;
    ctx.set_invention_roll_override(Some(0.50));
    let _ = drain_notif_events(&ctx, PILOT);
    rpc_call(&ctx, &mut s_def, "dogmaIM", "Activate", &[PyRep::Integer(996_000), PyRep::Integer(rat_id)])
        .await.expect("destroy NPC rat to advance Military Index");
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].military_index), 1);

    let rock_id = 880_002i64;
    ctx.systems
        .get_or_spawn(null_sys)
        .spawn_asteroid(
            eve_server::system_actor::AsteroidState {
                asteroid_id: rock_id,
                belt_id: 0,
                type_id: 1_228, // Scordite
                initial_units: 50_000,
                remaining_units: 50_000,
                unit_volume: 0.15,
                depleted: false,
                respawn_at_tick: None,
            },
            make_ball(rock_id, 1_228, hub_pos + DVec3::new(3_000.0, 0.0, 0.0)),
        )
        .await
        .expect("spawn Scordite asteroid");
    lock_target_via_rpc(&ctx, &mut s_def, null_sys, PILOT_SHIP, rock_id).await;
    rpc_call(&ctx, &mut s_def, "dogmaIM", "Activate", &[PyRep::Integer(996_050), PyRep::String("miningLaser".into()), PyRep::Integer(rock_id)])
        .await.expect("activate Miner I on Scordite");
    ctx.advance_secs(65);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].industrial_index >= 1));
    let _ = rpc_call(&ctx, &mut s_def, "dogmaIM", "Deactivate", &[PyRep::Integer(996_050), PyRep::String("miningLaser".into())]).await;
    let _ = drain_notif_events(&ctx, PILOT);

    ctx.advance_secs(8 * 86_400);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].strategic_index), 1);
    let dev_notifs = ctx.drain_notifications_for_char(PILOT);
    let dev_notif = dev_notifs.iter().find(|n| n.event == "OnSolarSystemDevIndexChanged").expect("OnSolarSystemDevIndexChanged");
    assert_eq!(dev_notif.args, vec![PyRep::Integer(null_sys as i64)]);

    let sov_info = rpc_call(&ctx, &mut s_def, "sovMgr", "GetSystemSovereigntyInfo", &[PyRep::Integer(null_sys as i64)])
        .await.expect("GetSystemSovereigntyInfo");
    let adm = sov_info.dict_get("vulnerabilityOccupancyLevel").and_then(|v| v.as_f64()).unwrap();
    assert!(adm > 1.0);

    // 2. ADM-scaled vulnerability window via `structureVulnerability.UpdateReinforceTimesFromClient`.
    rpc_call(&ctx, &mut s_def, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(hub_id), PyRep::Integer(12), PyRep::Integer(0)])
        .await.expect("UpdateReinforceTimesFromClient");
    let (v_start, v_end) = ctx.db.with_snapshot(|s| s.sov_runtime.adm_vulnerability_windows[&hub_id]);
    assert!(v_end - v_start < 18 * 3_600);

    let cur_sec = ctx.now_unix() % 86_400;
    ctx.advance_secs((86_400 + 3_600 - cur_sec) % 86_400);

    let atk_cid = 90_000_077i64;
    let atk_corp = 98_000_077i64;
    let atk_ally = 99_000_077i64;
    let atk_ship = 100_000_077i64;
    let entosis_mod = 996_200i64;
    let stront_item = 996_201i64;
    let mut s_atk = register_online_pilot(
        &ctx, 74_010, "demo2", "demo2-secret", atk_cid, atk_corp, Some(atk_ally),
        null_sys, atk_ship, 621, hub_pos + DVec3::new(5_000.0, 0.0, 0.0), 0,
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(atk_cid).or_default().insert(
            3_426,
            TrainedSkillRecord { type_id: 3_426, level: 5, skill_points: 256_000.0 },
        );
        snap.items.insert(
            entosis_mod,
            InventoryItemRecord {
                item_id: entosis_mod,
                type_id: 34_593,
                owner_id: atk_cid,
                location_id: atk_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            stront_item,
            InventoryItemRecord {
                item_id: stront_item,
                type_id: 16_275,
                owner_id: atk_cid,
                location_id: atk_ship,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(entosis_mod, true);
    });
    ctx.invalidate_ship_fitting(atk_ship);
    lock_target_via_rpc(&ctx, &mut s_atk, null_sys, atk_ship, hub_id).await;

    assert_eq!(
        rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(entosis_mod), PyRep::String("default".into()), PyRep::Integer(hub_id)])
            .await.unwrap_err().code,
        "SovHubInvulnerableOutsideWindow"
    );

    ctx.advance_secs(11 * 3_600);
    let act1 = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(entosis_mod), PyRep::String("default".into()), PyRep::Integer(hub_id)])
        .await.expect("activate Entosis Link I inside vulnerability window");
    assert_eq!(act1.dict_get("cycleIndex").and_then(|v| v.as_i64()), Some(1));
    assert!(ctx.db.with_snapshot(|s| s.items[&stront_item].quantity) < 100);

    let hub_capture_wait = 320 + ((ENTOSIS_HUB_BASE_CAPTURE_SECS * adm).ceil() as u64) + 30;
    ctx.advance_secs(hub_capture_wait);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "HullReinforced");
    let nodes: Vec<(i64, u32, [f64; 3])> = ctx.db.with_snapshot(|s| {
        let mut v: Vec<_> = s.sov_runtime.command_nodes.iter().map(|(&nid, &(_, nsys, _))| (nid, nsys, s.item_positions[&nid])).collect();
        v.sort_by_key(|x| x.0);
        v
    });
    assert_eq!(nodes.len(), 5);

    // 3. Live Entosis capture of 4 Constellation Command Nodes (`0.25 * 4 = 1.0`) flips `sov_alliance_id`!
    let node_wait = 320 + ((ENTOSIS_NODE_BASE_CAPTURE_SECS * adm).ceil() as u64) + 30;
    for &(node_id, nsys, npos) in nodes.iter().take(4) {
        s_atk.solar_system_id = Some(nsys);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&atk_cid) {
                ch.solar_system_id = nsys;
            }
            if let Some(it) = snap.items.get_mut(&atk_ship) {
                it.location_id = nsys as i64;
            }
        });
        let cs_atk = ctx.build_ship_combat_state(atk_cid, atk_ship, 621);
        let _ = ctx.systems.get_or_spawn(nsys).spawn_ball_with_combat(make_ball(atk_ship, 621, DVec3::from_array(npos) + DVec3::new(4_000.0, 0.0, 0.0)), cs_atk).await;
        lock_target_via_rpc(&ctx, &mut s_atk, nsys, atk_ship, node_id).await;
        rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(entosis_mod), PyRep::String("default".into()), PyRep::Integer(node_id)])
            .await.expect("activate Entosis Link on Command Node");
        ctx.advance_secs(node_wait);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }

    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].sov_alliance_id), Some(atk_ally));

    drop(ctx);
    let ctx_restarted = open_sqlite_ctx(&db_path, false);
    assert_eq!(ctx_restarted.db.with_snapshot(|s| s.system_states[&null_sys].sov_alliance_id), Some(atk_ally));
    let _ = std::fs::remove_file(&db_path);
}
