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
use eve_machonet::SessionState;
use eve_server::router::ServerContext;
use eve_server::services::sov_gameplay::{
    ENTOSIS_HUB_BASE_CAPTURE_SECS, ENTOSIS_NODE_BASE_CAPTURE_SECS,
};
use eve_server::services::sov_services::{claim_system_sovereignty, TYPE_SOVEREIGNTY_HUB};
use glam::DVec3;

fn pick_active_node(ctx: &ServerContext, hub_id: i64) -> (i64, u32, [f64; 3]) {
    ctx.db.with_snapshot(|s| {
        let (&nid, &(_, nsys, _)) = s
            .sov_runtime
            .command_nodes
            .iter()
            .find(|(_, &(hid, _, _))| hid == hub_id)
            .expect("active command node");
        (nid, nsys, s.item_positions[&nid])
    })
}

async fn entosis_node(
    ctx: &ServerContext,
    sess: &mut SessionState,
    cid: i64,
    ship_id: i64,
    ship_type: u32,
    mod_id: i64,
    node_id: i64,
    nsys: u32,
    npos: [f64; 3],
    wait_secs: u64,
) {
    let prev_sys = sess.solar_system_id.unwrap_or(nsys);
    if prev_sys != nsys {
        let _ = ctx.systems.get_or_spawn(prev_sys).remove_ball(ship_id).await;
    }
    sess.solar_system_id = Some(nsys);
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.solar_system_id = nsys;
            ch.active_ship_id = ship_id;
        }
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.location_id = nsys as i64;
        }
    });
    let cs = ctx.build_ship_combat_state(cid, ship_id, ship_type);
    let _ = ctx
        .systems
        .get_or_spawn(nsys)
        .spawn_ball_with_combat(make_ball(ship_id, ship_type, DVec3::from_array(npos) + DVec3::new(4_000.0, 0.0, 0.0)), cs)
        .await;
    lock_target_via_rpc(ctx, sess, nsys, ship_id, node_id).await;
    rpc_call(
        ctx,
        sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_id), PyRep::String("default".into()), PyRep::Integer(node_id)],
    )
    .await
    .expect("activate Entosis Link on Command Node");
    ctx.advance_secs(wait_secs);
    eve_server::sp6_lifecycle::tick_background_cluster(ctx).await;
}

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
        // Fit Expanded Cargohold II (`1319`) so Naglfar cargo hold exceeds 1,500 m3 for >= 1,000 m3 ore threshold
        for (slot_idx, flag) in [InventoryFlag::LoSlot0, InventoryFlag::LoSlot1].into_iter().enumerate() {
            let mod_id = 996_060 + slot_idx as i64;
            snap.items.insert(
                mod_id,
                InventoryItemRecord {
                    item_id: mod_id,
                    type_id: 1_319,
                    owner_id: PILOT,
                    location_id: PILOT_SHIP,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mod_id, true);
        }
    });
    ctx.invalidate_ship_fitting(PILOT_SHIP);
    let cs_def = ctx.build_ship_combat_state(PILOT, PILOT_SHIP, 19_722);
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(make_ball(PILOT_SHIP, 19_722, hub_pos + DVec3::new(2_500.0, 0.0, 0.0)), cs_def).await;

    ctx.set_invention_roll_override(Some(0.50));
    let _ = drain_notif_events(&ctx, PILOT);

    // Negative check: ejecting from player ships (`ship.Eject`, `category_id == 6`) in sovereign nullsec
    // and destroying the empty hulls grants ZERO Military Index points!
    for ej_idx in 0..5i64 {
        let alt_cid = 90_090_001 + ej_idx;
        let alt_ship = 879_001 + ej_idx;
        let alt_pos = hub_pos + DVec3::new(3_200.0 + (ej_idx as f64) * 80.0, 0.0, 0.0);
        let mut s_alt = register_online_pilot(
            &ctx, (74_500 + ej_idx) as u64, &format!("ej{ej_idx}"), "ej-secret", alt_cid, CORP, Some(ALLIANCE),
            null_sys, alt_ship, 24_690, alt_pos, 0,
        ).await;
        rpc_call(&ctx, &mut s_alt, "ship", "Eject", &[]).await.expect("eject from player ship in nullsec");
        let mut hull_cs = ctx.build_type_combat_state(24_690);
        hull_cs.signature_radius = 40_000.0;
        hull_cs.max_shield_hp = 10.0;
        hull_cs.shield_hp = 10.0;
        hull_cs.max_armor_hp = 10.0;
        hull_cs.armor_hp = 10.0;
        hull_cs.max_structure_hp = 10.0;
        hull_cs.structure_hp = 10.0;
        let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(make_ball(alt_ship, 24_690, alt_pos), hull_cs).await;
        lock_target_via_rpc(&ctx, &mut s_def, null_sys, PILOT_SHIP, alt_ship).await;
        let gun_id = if ej_idx % 2 == 0 { 996_000 } else { 996_002 };
        rpc_call(&ctx, &mut s_def, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(alt_ship)])
            .await.expect("destroy ejected empty player hull");
        crate::sovereignty_equinox_gameplay_closure::step_weapon_ticks(&ctx, null_sys).await;
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.sov_npc_kills.get(&null_sys).copied().unwrap_or(0)), 0, "destroying 5 ejected player hulls must credit 0 NPC kills");
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].military_index), 0, "destroying 5 ejected player hulls must not raise Military Index");

    // Positive check: destroying 5 genuine SDE `category_id == 11` Guristas rats (`17005` Pith Extinguisher) advances Military Index to 1!
    for idx in 0..5i64 {
        let rat_id = 880_001 + idx;
        let mut rat_cs = ctx.build_type_combat_state(17_005);
        rat_cs.signature_radius = 40_000.0;
        rat_cs.max_shield_hp = 10.0;
        rat_cs.shield_hp = 10.0;
        rat_cs.max_armor_hp = 10.0;
        rat_cs.armor_hp = 10.0;
        rat_cs.max_structure_hp = 10.0;
        rat_cs.structure_hp = 10.0;
        let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(make_ball(rat_id, 17_005, hub_pos + DVec3::new(3_500.0 + (idx as f64) * 100.0, 0.0, 0.0)), rat_cs).await;
        lock_target_via_rpc(&ctx, &mut s_def, null_sys, PILOT_SHIP, rat_id).await;
        let gun_id = if idx % 2 == 0 { 996_000 } else { 996_002 };
        rpc_call(&ctx, &mut s_def, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(rat_id)])
            .await.expect("destroy NPC rat to advance Military Index");
        if idx == 0 {
            assert_eq!(
                ctx.db.with_snapshot(|s| s.system_states[&null_sys].military_index),
                0,
                "single NPC kill must not advance Military Index 1 (threshold is 5 kills)"
            );
        }
        crate::sovereignty_equinox_gameplay_closure::step_weapon_ticks(&ctx, null_sys).await;
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].military_index), 1);

    let rock_id = 880_010i64;
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
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states[&null_sys].industrial_index),
        0,
        "single 50 m3 mining cycle must not advance Industrial Index 1 (threshold is 1,000 m3)"
    );
    ctx.advance_secs(25 * 60);
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

    // 2. ADM-scaled midnight-wrapping vulnerability window (`center_hour_utc = 23`) via `structureVulnerability.UpdateReinforceTimesFromClient`.
    rpc_call(&ctx, &mut s_def, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(hub_id), PyRep::Integer(23), PyRep::Integer(0)])
        .await.expect("UpdateReinforceTimesFromClient");
    let (v_start, v_end) = ctx.db.with_snapshot(|s| s.sov_runtime.adm_vulnerability_windows[&hub_id]);
    assert!(
        v_start > v_end,
        "23:00 UTC center with ADM > 1.0 must wrap across midnight (start_sec={v_start} > end_sec={v_end})"
    );

    // Advance to 10:00 UTC (outside the wrapped [~17:22 .. ~04:37] window)
    let cur_sec = ctx.now_unix() % 86_400;
    ctx.advance_secs((86_400 + 10 * 3_600 - cur_sec) % 86_400);

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
    let atk2_cid = 90_000_078i64;
    let atk2_corp = 98_000_078i64;
    let atk2_ally = 99_000_078i64;
    let atk2_ship = 100_000_078i64;
    let entosis_mod2 = 996_202i64;
    let stront_item2 = 996_203i64;
    let mut s_atk2 = register_online_pilot(
        &ctx, 74_011, "demo3", "demo3-secret", atk2_cid, atk2_corp, Some(atk2_ally),
        null_sys, atk2_ship, 621, hub_pos + DVec3::new(6_000.0, 0.0, 0.0), 0,
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(atk2_cid).or_default().insert(
            3_426,
            TrainedSkillRecord { type_id: 3_426, level: 5, skill_points: 256_000.0 },
        );
        snap.items.insert(
            entosis_mod2,
            InventoryItemRecord {
                item_id: entosis_mod2,
                type_id: 34_593,
                owner_id: atk2_cid,
                location_id: atk2_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            stront_item2,
            InventoryItemRecord {
                item_id: stront_item2,
                type_id: 16_275,
                owner_id: atk2_cid,
                location_id: atk2_ship,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(entosis_mod2, true);
    });
    ctx.invalidate_ship_fitting(atk2_ship);
    let def_ship = 100_000_002i64;
    let def_entosis_mod = 996_070i64;
    let def_stront = 996_071i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            def_ship,
            InventoryItemRecord {
                item_id: def_ship,
                type_id: 639,
                owner_id: PILOT,
                location_id: null_sys as i64,
                flag: InventoryFlag::None,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            def_entosis_mod,
            InventoryItemRecord {
                item_id: def_entosis_mod,
                type_id: 34_593,
                owner_id: PILOT,
                location_id: def_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            def_stront,
            InventoryItemRecord {
                item_id: def_stront,
                type_id: 16_275,
                owner_id: PILOT,
                location_id: def_ship,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(def_entosis_mod, true);
    });
    ctx.invalidate_ship_fitting(def_ship);
    ctx.invalidate_ship_fitting(atk_ship);
    lock_target_via_rpc(&ctx, &mut s_atk, null_sys, atk_ship, hub_id).await;

    assert_eq!(
        rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(entosis_mod), PyRep::String("default".into()), PyRep::Integer(hub_id)])
            .await.unwrap_err().code,
        "SovHubInvulnerableOutsideWindow"
    );

    // Advance 15 hours to ~01:00 UTC (after midnight, inside the wrapped [~17:22 .. ~04:37] window)
    ctx.advance_secs(15 * 3_600);
    let act1 = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(entosis_mod), PyRep::String("default".into()), PyRep::Integer(hub_id)])
        .await.expect("activate Entosis Link I inside midnight-wrapped vulnerability window");
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

    let init_prog = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!((init_prog - 0.60).abs() < 1e-5, "initial campaign progress must start at 0.60 (60%)");

    // 3. Contested Command Node captures & continuous respawn preventing deadlock!
    // Defender captures 2 nodes (+0.07 each -> 0.74), Attacker captures 2 nodes (-0.07 each -> 0.60).
    // Active nodes remain 5 after every capture.
    let node_wait = 320 + ((ENTOSIS_NODE_BASE_CAPTURE_SECS * adm).ceil() as u64) + 30;

    let (n1, sys1, pos1) = pick_active_node(&ctx, hub_id);
    entosis_node(&ctx, &mut s_def, PILOT, def_ship, 639, def_entosis_mod, n1, sys1, pos1, node_wait).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 5, "defender capture 1 respawns node");
    let p1 = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!((p1 - 0.67).abs() < 1e-4);

    let (n2, sys2, pos2) = pick_active_node(&ctx, hub_id);
    entosis_node(&ctx, &mut s_atk, atk_cid, atk_ship, 621, entosis_mod, n2, sys2, pos2, node_wait).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 5, "attacker capture 1 respawns node");
    let p2 = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!((p2 - 0.60).abs() < 1e-4);

    let (n3, sys3, pos3) = pick_active_node(&ctx, hub_id);
    entosis_node(&ctx, &mut s_def, PILOT, def_ship, 639, def_entosis_mod, n3, sys3, pos3, node_wait).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 5, "defender capture 2 respawns node");
    let p3 = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!((p3 - 0.67).abs() < 1e-4);

    let (n4, sys4, pos4) = pick_active_node(&ctx, hub_id);
    entosis_node(&ctx, &mut s_atk, atk_cid, atk_ship, 621, entosis_mod, n4, sys4, pos4, node_wait).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 5, "attacker capture 2 respawns node (2-2 split)");
    let p4 = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!((p4 - 0.60).abs() < 1e-4);

    // Defender captures 6 nodes to push progress from 0.60 to >= 1.0 (0.60 + 6 * 0.07 = 1.02 >= 1.0)
    for cap_idx in 0..5 {
        let (nid, nsys, npos) = pick_active_node(&ctx, hub_id);
        entosis_node(&ctx, &mut s_def, PILOT, def_ship, 639, def_entosis_mod, nid, nsys, npos, node_wait).await;
        assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 5, "defender intermediate capture {cap_idx}");
    }
    let (n_final, sys_final, pos_final) = pick_active_node(&ctx, hub_id);
    entosis_node(&ctx, &mut s_def, PILOT, def_ship, 639, def_entosis_mod, n_final, sys_final, pos_final, node_wait).await;

    // Defender victory assertion: hub restored to "Online" (invulnerable), sovereign alliance remains ALLIANCE
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "Online");
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].sov_alliance_id), Some(ALLIANCE));
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 0, "all command nodes despawned on resolution");

    // 4. Attackers re-entosis Sov Hub and capture 9 nodes unified under Attacker side to claim system sovereignty!
    s_atk.solar_system_id = Some(null_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&atk_cid) { ch.solar_system_id = null_sys; }
        if let Some(it) = snap.items.get_mut(&atk_ship) { it.location_id = null_sys as i64; }
    });
    let cs_atk_re = ctx.build_ship_combat_state(atk_cid, atk_ship, 621);
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(make_ball(atk_ship, 621, hub_pos + DVec3::new(5_000.0, 0.0, 0.0)), cs_atk_re).await;
    lock_target_via_rpc(&ctx, &mut s_atk, null_sys, atk_ship, hub_id).await;
    let cur_sec = ctx.now_unix() % 86_400;
    ctx.advance_secs((86_400 + 23 * 3_600 - cur_sec) % 86_400);
    rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(entosis_mod), PyRep::String("default".into()), PyRep::Integer(hub_id)])
        .await.expect("activate Entosis Link on Sov Hub for second campaign");
    ctx.advance_secs(hub_capture_wait);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "HullReinforced");
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 5);

    // Multi-attacker unification: Attacker 1 captures 5 nodes (-0.35, prog drops 0.60 -> 0.25).
    // Attacker 2 captures 4 nodes (-0.28, prog drops 0.25 -> 0.0, attackers win!).
    // Attacker 1 has majority captures (5 vs 4) -> Attacker 1's alliance wins sovereignty.
    for _ in 0..5 {
        let (nid, nsys, npos) = pick_active_node(&ctx, hub_id);
        entosis_node(&ctx, &mut s_atk, atk_cid, atk_ship, 621, entosis_mod, nid, nsys, npos, node_wait).await;
    }
    let p_mid = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!((p_mid - 0.25).abs() < 1e-4, "progress drops to 0.25 after 5 attacker-1 captures");

    for _ in 0..4 {
        let (nid, nsys, npos) = pick_active_node(&ctx, hub_id);
        entosis_node(&ctx, &mut s_atk2, atk2_cid, atk2_ship, 621, entosis_mod2, nid, nsys, npos, node_wait).await;
    }

    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "ShieldVulnerable");
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].sov_alliance_id), Some(atk_ally));
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].owner_corp_id), atk_corp);
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 0);

    drop(ctx);
    let ctx_restarted = open_sqlite_ctx(&db_path, false);
    assert_eq!(ctx_restarted.db.with_snapshot(|s| s.system_states[&null_sys].sov_alliance_id), Some(atk_ally));
    let _ = std::fs::remove_file(&db_path);
}
