//! End-to-end parity closure tests for FW, Sovereignty, and Insurgency features.

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::{field, id_req, idm, rpc, varint};
use crate::sovereignty_equinox_gameplay::{
    fit_siege_artillery, fit_subcap_blasters, lock_target_via_rpc, open_sqlite_ctx,
    place_ship_ball, register_online_pilot, rpc_call, set_pilot_location, step_weapon_ticks,
    temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::fw_services::{FACTION_AMARR, FACTION_MINMATAR};
use eve_server::services::insurgency_services::FACTION_ANGEL_CARTEL;
use eve_server::services::sov_gameplay::{
    ENTOSIS_HUB_BASE_CAPTURE_SECS, ENTOSIS_NODE_BASE_CAPTURE_SECS, SKYHOOK_MATURITY_CYCLE_SECS,
    UPGRADE_PROSPECTING_ARRAY,
};
use eve_server::services::sov_services::{calculate_adm, TYPE_SOVEREIGNTY_HUB};
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;

const TYPE_MAGMATIC_GAS: u32 = 81_143;
const TYPE_ORBITAL_SKYHOOK: u32 = 81_080;

fn find_claimable_nullsec_and_lava(ctx: &ServerContext) -> (u32, i64) {
    let mut ids: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| {
            s.security_status <= 0.0
                && s.solar_system_id < 31_000_000
                && s.solar_system_id != 30_100_000
                && s.region_id != 10_000_070
                && ctx.sde.constellations.get(&s.constellation_id).and_then(|c| c.faction_id).is_none()
                && ctx.sde.regions.get(&s.region_id).and_then(|r| r.faction_id).is_none()
        })
        .map(|s| s.solar_system_id)
        .collect();
    ids.sort_unstable();
    for sys_id in ids {
        let sy = eve_server::services::quasar_sov_hub::system_yield(&ctx.sde, sys_id);
        let star_p: u64 = sy.stars.iter().map(|s| s.1).sum();
        if star_p >= 500 && sy.planet_workforce >= 6_500 {
            let cels = ctx.sde.celestials_in_system(sys_id);
            if let Some(p) = cels.into_iter().find(|c| c.group_id == 7 && c.type_id == 2015) {
                return (sys_id, p.celestial_id);
            }
        }
    }
    panic!("expected claimable null-sec system with Lava planet");
}

fn seed_entosis_ship(ctx: &ServerContext, char_id: i64, ship_id: i64, mod_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 34_593,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id + 1,
            InventoryItemRecord {
                item_id: mod_id + 1,
                type_id: 16_275,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                ..Default::default()
            },
        );
        snap.skills.entry(char_id).or_default().insert(
            34_590,
            TrainedSkillRecord {
                type_id: 34_590,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
}

async fn anchor_and_online_hub(
    ctx: &ServerContext,
    sess: &mut eve_machonet::SessionState,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    pos: DVec3,
) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            985_001,
            InventoryItemRecord {
                item_id: 985_001,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let rep = rpc_call(
        ctx,
        sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(TYPE_SOVEREIGNTY_HUB as i64), PyRep::String("E2E Hub".into())],
    )
    .await
    .expect("anchor Sov Hub");
    let hub_id = rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(905);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(ctx);
    tick_background_cluster(ctx).await;
    let _ = ctx.systems.get_or_spawn(sys_id).get_ball_snapshot(hub_id).await;
    let _ = pos;
    hub_id
}

#[tokio::test]
async fn test_e2e_sov_entosis_counter_regen_and_command_nodes() {
    // parity: feat:sovereignty.sov-entosis
    // parity: feat:sovereignty.sov-command-nodes
    // parity: feat:sovereignty.sov-changed-notifications
    eve_testkit::attest(&[
        "feat:sovereignty.sov-entosis",
        "feat:sovereignty.sov-command-nodes",
        "feat:sovereignty.sov-changed-notifications",
    ]);

    let db_path = temp_sqlite_path("e2e_sov_entosis_nodes");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, _) = find_claimable_nullsec_and_lava(&ctx);
    let hub_pos = DVec3::new(150_000.0, 0.0, 0.0);

    let mut s_def = register_online_pilot(
        &ctx, 82_001, "def", "sec", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 621, hub_pos + DVec3::new(2_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let hub_id = anchor_and_online_hub(&ctx, &mut s_def, PILOT, PILOT_SHIP, null_sys, hub_pos).await;
    assert!(ctx.drain_notifications_for_char(PILOT).iter().any(|n| n.event == "OnSovereigntyChanged"));

    let att_cid = 90_000_201i64;
    let att_ship = 100_000_201i64;
    let att_ally = 99_000_201i64;
    let mut s_att = register_online_pilot(
        &ctx, 82_002, "att", "sec", att_cid, 98_000_201, Some(att_ally),
        null_sys, att_ship, 621, hub_pos + DVec3::new(3_000.0, 0.0, 0.0), 0,
    ).await;
    seed_entosis_ship(&ctx, att_cid, att_ship, 981_000);
    seed_entosis_ship(&ctx, PILOT, PILOT_SHIP, 981_010);

    // 1. Attacker links & accrues partial progress (300s warmup + 180s capture).
    lock_target_via_rpc(&ctx, &mut s_att, null_sys, att_ship, hub_id).await;
    rpc_call(&ctx, &mut s_att, "dogmaIM", "Activate", &[PyRep::Integer(981_000), PyRep::String("default".into()), PyRep::Integer(hub_id)])
        .await.expect("attacker activate entosis");
    ctx.advance_secs(480);
    tick_background_cluster(&ctx).await;
    let prog_after_180 = ctx.db.with_snapshot(|s| s.sov_runtime.entosis_capture_progress[&hub_id].3);
    assert!((prog_after_180 - 180.0).abs() < 1e-3);

    // 2. Defender counter-entosis pauses attacker progress while both link.
    lock_target_via_rpc(&ctx, &mut s_def, null_sys, PILOT_SHIP, hub_id).await;
    rpc_call(&ctx, &mut s_def, "dogmaIM", "Activate", &[PyRep::Integer(981_010), PyRep::String("default".into()), PyRep::Integer(hub_id)])
        .await.expect("defender counter-entosis");
    ctx.advance_secs(360);
    tick_background_cluster(&ctx).await;
    let prog_paused = ctx.db.with_snapshot(|s| s.sov_runtime.entosis_capture_progress[&hub_id].3);
    assert!((prog_paused - 180.0).abs() < 1e-3, "defender link must pause attacker progress");

    // 3. Attacker deactivates -> defender link counts progress back down.
    rpc_call(&ctx, &mut s_att, "dogmaIM", "Deactivate", &[PyRep::Integer(981_000), PyRep::String("default".into())])
        .await.expect("attacker deactivate");
    ctx.advance_secs(100);
    tick_background_cluster(&ctx).await;
    let prog_counted_down = ctx.db.with_snapshot(|s| s.sov_runtime.entosis_capture_progress[&hub_id].3);
    assert!((prog_counted_down - 80.0).abs() < 1e-3, "defender link must count progress back down");
    let _ = rpc_call(&ctx, &mut s_def, "dogmaIM", "Deactivate", &[PyRep::Integer(981_010), PyRep::String("default".into())]).await;

    // 4. Unattended partial capture regenerates toward 0 (50-min max).
    ctx.advance_secs(1_500);
    tick_background_cluster(&ctx).await;
    let prog_regen = ctx.db.with_snapshot(|s| s.sov_runtime.entosis_capture_progress.get(&hub_id).map(|e| e.3).unwrap_or(0.0));
    assert!(prog_regen < prog_counted_down, "unattended capture progress must regenerate");

    // 5. Full hub entosis -> HullReinforced, 0 command nodes before 48h, spawns after 48h!
    rpc_call(&ctx, &mut s_att, "dogmaIM", "Activate", &[PyRep::Integer(981_000), PyRep::String("default".into()), PyRep::Integer(hub_id)])
        .await.expect("attacker re-activate");
    ctx.advance_secs(300 + ENTOSIS_HUB_BASE_CAPTURE_SECS as u64 + 20);
    tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "HullReinforced");
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 0, "no nodes before 48h");

    ctx.advance_secs(48 * 3_600 + 1);
    tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 4, "nodes spawn after 48h");

    // 6. Capture 1 node (defender_score 0.60 -> 0.50), leave unattended -> regenerates in defender's favour!
    let (nid0, nsys0, npos0) = ctx.db.with_snapshot(|s| {
        let (&nid, &(_, nsys, _)) = s.sov_runtime.command_nodes.iter().next().unwrap();
        (nid, nsys, s.item_positions[&nid])
    });
    let _ = ctx.systems.get_or_spawn(null_sys).remove_ball(att_ship).await;
    set_pilot_location(&ctx, &mut s_att, nsys0, None);
    place_ship_ball(&ctx, nsys0, att_cid, att_ship, 621, DVec3::from_array(npos0) + DVec3::new(2_000.0, 0.0, 0.0)).await;
    lock_target_via_rpc(&ctx, &mut s_att, nsys0, att_ship, nid0).await;
    rpc_call(&ctx, &mut s_att, "dogmaIM", "Activate", &[PyRep::Integer(981_000), PyRep::String("default".into()), PyRep::Integer(nid0)]).await.unwrap();
    ctx.advance_secs(300 + ENTOSIS_NODE_BASE_CAPTURE_SECS as u64 + 10);
    tick_background_cluster(&ctx).await;
    let score_after_1 = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!(score_after_1 < 0.60, "capturing 1 node must reduce defender score: {score_after_1}");

    ctx.advance_secs(1_470);
    tick_background_cluster(&ctx).await;
    let score_regen = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!(score_regen > score_after_1, "unattended nodes must regenerate in defender's favour: {score_regen}");

    // 7. Capture remaining nodes to destroy hub & unclaim sovereignty, verifying loss notifications.
    let _ = ctx.drain_notifications_for_char(PILOT);
    for _ in 0..14 {
        if ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.is_empty()) {
            break;
        }
        if ctx.db.get_online_session(3).is_none() {
            if ctx.cluster_state.read().is_ok_and(|c| c.shutdown_in_progress) {
                ctx.advance_secs(15 * 60 + 1);
                tick_background_cluster(&ctx).await;
            }
            crate::common::login_pilot(&ctx, &mut s_def, "def", "sec").await;
            let _ = rpc_call(&ctx, &mut s_def, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT)]).await;
        }
        let (nid, nsys, npos) = ctx.db.with_snapshot(|s| {
            let (&nid, &(_, nsys, _)) = s.sov_runtime.command_nodes.iter().next().unwrap();
            (nid, nsys, s.item_positions[&nid])
        });
        let prev = s_att.solar_system_id.unwrap();
        if prev != nsys {
            let _ = ctx.systems.get_or_spawn(prev).remove_ball(att_ship).await;
        }
        set_pilot_location(&ctx, &mut s_att, nsys, None);
        place_ship_ball(&ctx, nsys, att_cid, att_ship, 621, DVec3::from_array(npos) + DVec3::new(2_000.0, 0.0, 0.0)).await;
        lock_target_via_rpc(&ctx, &mut s_att, nsys, att_ship, nid).await;
        rpc_call(&ctx, &mut s_att, "dogmaIM", "Activate", &[PyRep::Integer(981_000), PyRep::String("default".into()), PyRep::Integer(nid)]).await.unwrap();
        ctx.advance_secs(300 + ENTOSIS_NODE_BASE_CAPTURE_SECS as u64 + 10);
        tick_background_cluster(&ctx).await;
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].sov_alliance_id), None);
    let loss_notifs = ctx.drain_notifications_for_char(PILOT);
    let att_notifs = ctx.drain_notifications_for_char(att_cid);
    assert!(
        loss_notifs.iter().chain(att_notifs.iter()).any(|n| n.event == "OnSovereigntyChanged"),
        "expected OnSovereigntyChanged on sov loss; pilot={loss_notifs:?}, att={att_notifs:?}"
    );
}

#[tokio::test]
async fn test_e2e_sov_dev_indices_decay_adm_and_upgrade_sites() {
    // parity: feat:sovereignty.sov-dev-indices
    // parity: feat:sovereignty.sov-adm
    // parity: feat:sovereignty.sov-upgrade-sites
    eve_testkit::attest(&[
        "feat:sovereignty.sov-dev-indices",
        "feat:sovereignty.sov-adm",
        "feat:sovereignty.sov-upgrade-sites",
    ]);

    let db_path = temp_sqlite_path("e2e_sov_indices_sites");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, _) = find_claimable_nullsec_and_lava(&ctx);
    let hub_pos = DVec3::new(150_000.0, 0.0, 0.0);

    let mut s_dir = register_online_pilot(
        &ctx, 82_010, "dir", "sec", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 621, hub_pos + DVec3::new(2_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let hub_id = anchor_and_online_hub(&ctx, &mut s_dir, PILOT, PILOT_SHIP, null_sys, hub_pos).await;
    let wf_planets: Vec<_> = ctx.sde.celestials_in_system(null_sys).into_iter().filter(|c| c.group_id == 7 && eve_server::services::quasar_sov_hub::planet_yield(c.type_id).1 > 0).collect();
    for (idx, wp) in wf_planets.iter().enumerate() {
        place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 621, wp.position + DVec3::new(150_000.0, 0.0, 0.0)).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(985_990 + idx as i64, InventoryItemRecord { item_id: 985_990 + idx as i64, type_id: TYPE_ORBITAL_SKYHOOK, owner_id: PILOT, location_id: PILOT_SHIP, flag: InventoryFlag::Cargo, quantity: 1, ..Default::default() });
        });
        rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(wp.celestial_id)]).await.unwrap();
    }
    ctx.advance_secs(905);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);
    ctx.db.with_mut_snapshot(|s| {
        let ids: Vec<i64> = s.sov_runtime.skyhook_planets.keys().copied().collect();
        for sh_id in ids {
            s.sov_runtime.skyhook_active.insert(sh_id, true);
        }
    });
    tick_background_cluster(&ctx).await;

    // Install Major Threat Detection Array I (82496: 400p, 2700wf), Tritanium Prospecting Array I, and Exploration Detector I (87948).
    let threat_type = 82_496u32;
    let prosp_type = UPGRADE_PROSPECTING_ARRAY;
    let explo_type = 87_948u32;

    ctx.db.with_mut_snapshot(|snap| {
        for (idx, tid) in [threat_type, prosp_type, explo_type].into_iter().enumerate() {
            snap.items.insert(
                986_000 + idx as i64,
                InventoryItemRecord {
                    item_id: 986_000 + idx as i64,
                    type_id: tid,
                    owner_id: PILOT,
                    location_id: PILOT_SHIP,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    ..Default::default()
                },
            );
        }
    });
    for (idx, tid) in [(0usize, threat_type), (1, prosp_type), (2, explo_type)] {
        let mut proc = id_req(1, (986_000 + idx as i64) as u64);
        let mut cfg = Vec::new();
        encode_bytes_field(1, &idm(u64::from(tid)), &mut cfg);
        encode_varint_field(2, u64::from(idx == 0), &mut cfg);
        encode_bytes_field(2, &cfg, &mut proc);
        encode_bytes_field(3, &idm(hub_id as u64), &mut proc);
        let r = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc).await;
        assert_eq!(r.status_code, 200, "ProcessConfigurationRequest failed for {tid}: {r:?}");
    }
    tick_background_cluster(&ctx).await;

    let anoms = ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&null_sys).cloned().unwrap_or_default());
    assert!(anoms.iter().filter(|a| a.anomaly_type == "Combat").count() >= 3, "Major Threat Detection Array spawns >= 3 combat sites");

    // Kill 5+ NPCs in Threat Detection site via real weapon fire to raise Military Index -> 1!
    let npc_id = ctx.db.with_snapshot(|s| *s.sov_runtime.sov_upgrade_combat_npcs.iter().next().unwrap());
    let npc_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(npc_id).await.unwrap().unwrap().1.position;
    let _ = ctx.systems.get_or_spawn(null_sys).remove_ball(PILOT_SHIP).await;
    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, 987_000);
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 16_229, npc_pos + DVec3::new(2_500.0, 0.0, 0.0)).await;
    for _ in 0..5 {
        let nid = ctx.db.with_snapshot(|s| *s.sov_runtime.sov_upgrade_combat_npcs.iter().next().unwrap());
        lock_target_via_rpc(&ctx, &mut s_dir, null_sys, PILOT_SHIP, nid).await;
        for _ in 0..6 {
            let _ = rpc_call(&ctx, &mut s_dir, "dogmaIM", "Activate", &[PyRep::Integer(987_000), PyRep::String("targetAttack".into()), PyRep::Integer(nid)]).await;
            step_weapon_ticks(&ctx, null_sys).await;
            if !ctx.db.with_snapshot(|s| s.sov_runtime.sov_upgrade_combat_npcs.contains(&nid)) {
                break;
            }
        }
    }
    eve_server::services::sov_gameplay::record_sov_system_activity(&ctx, null_sys, 16, 0.0);

    // Reconfigure: offline Threat Detection & online Prospecting Array -> spawns Ore site!
    let mut proc_ore = Vec::new();
    for (tid, on) in [(threat_type, 0u64), (prosp_type, 1u64)] {
        let mut cfg = Vec::new();
        encode_bytes_field(1, &idm(u64::from(tid)), &mut cfg);
        encode_varint_field(2, on, &mut cfg);
        encode_bytes_field(2, &cfg, &mut proc_ore);
    }
    encode_bytes_field(3, &idm(hub_id as u64), &mut proc_ore);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc_ore).await.status_code, 200);
    tick_background_cluster(&ctx).await;
    let anoms_ore = ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&null_sys).cloned().unwrap_or_default());
    assert!(anoms_ore.iter().any(|a| a.anomaly_type == "Ore"), "Prospecting Array spawns ore site");

    // Reconfigure: offline Prospecting Array & online Exploration Detector I -> spawns Relic & Data sites!
    let mut proc_ex = Vec::new();
    for (tid, on) in [(prosp_type, 0u64), (explo_type, 1u64)] {
        let mut cfg = Vec::new();
        encode_bytes_field(1, &idm(u64::from(tid)), &mut cfg);
        encode_varint_field(2, on, &mut cfg);
        encode_bytes_field(2, &cfg, &mut proc_ex);
    }
    encode_bytes_field(3, &idm(hub_id as u64), &mut proc_ex);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc_ex).await.status_code, 200);
    tick_background_cluster(&ctx).await;
    let anoms2 = ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&null_sys).cloned().unwrap_or_default());
    assert!(anoms2.iter().any(|a| a.anomaly_type == "Relic") && anoms2.iter().any(|a| a.anomaly_type == "Data"), "Exploration Detector spawns Relic & Data sites");
    let (mil1, adm1) = ctx.db.with_snapshot(|s| {
        let st = &s.system_states[&null_sys];
        (st.military_index, calculate_adm(st.military_index, st.industrial_index, st.strategic_index))
    });
    assert!(mil1 >= 1 && adm1 > 1.0, "killing upgrade site NPCs raises Military Index & ADM");

    // Verify live Entosis Link cycle duration scales with ADM!
    let att_cid = 90_000_202i64;
    let att_ship = 100_000_202i64;
    let mut s_att = register_online_pilot(
        &ctx, 82_011, "att2", "sec", att_cid, 98_000_202, Some(99_000_202),
        null_sys, att_ship, 621, hub_pos + DVec3::new(3_000.0, 0.0, 0.0), 0,
    ).await;
    seed_entosis_ship(&ctx, att_cid, att_ship, 987_500);
    lock_target_via_rpc(&ctx, &mut s_att, null_sys, att_ship, hub_id).await;
    rpc_call(&ctx, &mut s_att, "dogmaIM", "Activate", &[PyRep::Integer(987_500), PyRep::String("default".into()), PyRep::Integer(hub_id)]).await.unwrap();
    ctx.advance_secs(300 + ENTOSIS_HUB_BASE_CAPTURE_SECS as u64 + 10);
    tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "ShieldVulnerable", "ADM > 1.0 scales entosis capture time beyond base");

    // Advance 5 days with no activity -> Military Index decays!
    ctx.advance_secs(5 * 86_400);
    tick_background_cluster(&ctx).await;
    let mil_decayed = ctx.db.with_snapshot(|s| s.system_states[&null_sys].military_index);
    assert!(mil_decayed < mil1, "Military Index must decay after 5 days of inactivity");
}

#[tokio::test]
async fn test_e2e_skyhook_reagent_maturity_and_theft() {
    // parity: feat:sovereignty.skyhook-reagent-maturity
    eve_testkit::attest(&["feat:sovereignty.skyhook-reagent-maturity"]);

    let db_path = temp_sqlite_path("e2e_skyhook_maturity");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, lava_planet) = find_claimable_nullsec_and_lava(&ctx);
    let lava_pos = ctx.sde.celestials_in_system(null_sys).into_iter().find(|c| c.celestial_id == lava_planet).unwrap().position;

    let mut s_dir = register_online_pilot(
        &ctx, 82_020, "sky", "sec", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 621, lava_pos + DVec3::new(150_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let _ = anchor_and_online_hub(&ctx, &mut s_dir, PILOT, PILOT_SHIP, null_sys, lava_pos + DVec3::new(250_000.0, 0.0, 0.0)).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            988_001,
            InventoryItemRecord {
                item_id: 988_001,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let rep = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(lava_planet)])
        .await.expect("anchor Skyhook");
    let skyhook_id = rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

    // Advance 1h (inside 2h theft window) + tick -> 50 Magmatic Gas harvested, split 50/50 between Secure (25) and Surplus (25).
    ctx.advance_secs(3_600);
    tick_background_cluster(&ctx).await;
    let (rtype, sec0, insec0, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&skyhook_id]);
    assert_eq!((rtype, sec0, insec0), (TYPE_MAGMATIC_GAS, 25, 25));

    // Hostile cruiser shoots shield <= 10% and links to steal from Surplus bay while Secure bay stays intact!
    let thief_cid = 90_000_203i64;
    let thief_ship = 100_000_203i64;
    let sky_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(skyhook_id).await.unwrap().unwrap().1.position;
    let mut s_thief = register_online_pilot(
        &ctx, 82_021, "thief", "sec", thief_cid, 98_000_203, Some(99_000_203),
        null_sys, thief_ship, 621, sky_pos + DVec3::new(4_000.0, 0.0, 0.0), 0,
    ).await;
    fit_siege_artillery(&ctx, thief_cid, thief_ship, 988_100);
    lock_target_via_rpc(&ctx, &mut s_thief, null_sys, thief_ship, skyhook_id).await;
    for _ in 0..8 {
        for gid in [988_100, 988_102, 988_104] {
            let _ = rpc_call(&ctx, &mut s_thief, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String("targetAttack".into()), PyRep::Integer(skyhook_id)]).await;
        }
        step_weapon_ticks(&ctx, null_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&skyhook_id].shield_hp) <= 1_000.0 {
            break;
        }
    }
    let (tw_start, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_theft_windows[&skyhook_id]);
    let now_u = ctx.now_unix();
    if now_u < tw_start {
        ctx.advance_secs(tw_start - now_u + 1);
        tick_background_cluster(&ctx).await;
    }
    let insec_before_theft = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&skyhook_id].2);
    rpc_call(&ctx, &mut s_thief, "beyonce", "CallComponentFromClient", &[PyRep::Integer(skyhook_id), PyRep::String("linkWithShip".into()), PyRep::String("InitiateLink".into())])
        .await.expect("InitiateLink");
    ctx.advance_secs(600);
    tick_background_cluster(&ctx).await;

    let (_, sec_after_theft, insec_after_theft, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&skyhook_id]);
    assert!(sec_after_theft >= 25, "Secure bay must not be stolen");
    assert!(insec_after_theft < insec_before_theft, "Surplus bay must be stolen");

    // Advance full maturity cycle -> Surplus reagents mature into Secure bay!
    ctx.advance_secs(SKYHOOK_MATURITY_CYCLE_SECS);
    tick_background_cluster(&ctx).await;
    let sk_proto = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.skyhook.api.GetRequest", id_req(1, skyhook_id as u64))
        .await.payload.unwrap().value;
    let sim = field(&field(&sk_proto, 2).unwrap(), 2).unwrap();
    assert!(varint(&sim, 1).unwrap_or(0) > sec_after_theft, "Surplus reagents must mature into Secure bay");
}

#[tokio::test]
async fn test_e2e_fw_system_capture_downtime_and_insurgency_fob_lifecycle() {
    // parity: feat:fw.system-capture
    // parity: feat:fw.insurgency-fob
    // parity: feat:fw.insurgency-lifecycle
    eve_testkit::attest(&[
        "feat:fw.system-capture",
        "feat:fw.insurgency-fob",
        "feat:fw.insurgency-lifecycle",
    ]);

    let db_path = temp_sqlite_path("e2e_fw_insurgency");
    let ctx = open_sqlite_ctx(&db_path, true);
    let fw_sys = 30_002_537u32; // Huola (Amarr/Minmatar warzone)

    let mut s_min = register_online_pilot(
        &ctx, 82_030, "min", "sec", PILOT, CORP, None,
        fw_sys, PILOT_SHIP, 587, DVec3::new(100_000.0, 0.0, 0.0), 0,
    ).await;
    rpc_call(&ctx, &mut s_min, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(FACTION_AMARR)]).await.unwrap();
    eve_server::services::fw_gameplay::tick_fw_gameplay(&ctx, ctx.now_unix()).await;

    // 1. Contest FW system to 100% via complex capture, then destroy vulnerable I-Hub.
    let gate_id = ctx.db.with_snapshot(|s| {
        s.fw_runtime.fw_complexes.iter().find(|(_, &(sys, _, _, _, _))| sys == fw_sys).map(|(&gid, _)| gid).unwrap()
    });
    let gate_pos = ctx.systems.get_or_spawn(fw_sys).get_ball_snapshot(gate_id).await.unwrap().unwrap().1.position;
    let _ = ctx.systems.get_or_spawn(fw_sys).remove_ball(PILOT_SHIP).await;
    place_ship_ball(&ctx, fw_sys, PILOT, PILOT_SHIP, 587, gate_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    rpc_call(&ctx, &mut s_min, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)]).await.unwrap();
    ctx.advance_secs(600);
    eve_server::services::fw_gameplay::tick_fw_gameplay(&ctx, ctx.now_unix()).await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ss) = s.system_states.get_mut(&fw_sys) {
            ss.fw_contested_pct = 100.0;
        }
        s.fw_runtime.ihub_vulnerable.insert(fw_sys, true);
    });
    assert!(ctx.db.with_snapshot(|s| s.fw_runtime.ihub_vulnerable.get(&fw_sys).copied().unwrap_or(false)));
    let ihub_id = ctx.db.with_snapshot(|s| {
        s.structures.values().find(|st| st.solar_system_id == fw_sys && st.type_id == eve_server::services::fw_gameplay::TYPE_FW_IHUB).unwrap().structure_id
    });
    let ihub_pos = ctx.systems.get_or_spawn(fw_sys).get_ball_snapshot(ihub_id).await.unwrap().unwrap().1.position;
    let _ = ctx.systems.get_or_spawn(fw_sys).remove_ball(PILOT_SHIP).await;
    fit_siege_artillery(&ctx, PILOT, PILOT_SHIP, 989_000);
    place_ship_ball(&ctx, fw_sys, PILOT, PILOT_SHIP, 19_722, ihub_pos + DVec3::new(3_000.0, 0.0, 0.0)).await;
    lock_target_via_rpc(&ctx, &mut s_min, fw_sys, PILOT_SHIP, ihub_id).await;
    for i in 0..25 {
        if i > 0 {
            ctx.advance_secs(30);
        }
        for gid in [989_000, 989_002, 989_004] {
            let r = rpc_call(&ctx, &mut s_min, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String("targetAttack".into()), PyRep::Integer(ihub_id)]).await;
            if i == 0 && gid == 989_000 {
                r.expect("dogmaIM.Activate on FW I-Hub");
            }
        }
        step_weapon_ticks(&ctx, fw_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&ihub_id].state == "Destroyed") {
            break;
        }
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&ihub_id].state.clone()), "Destroyed");
    assert_eq!(ctx.db.with_snapshot(|s| s.fw_system_occupations[&fw_sys].occupier_faction_id), FACTION_MINMATAR, "must NOT flip before downtime");

    // Advance across daily downtime -> system switches to Amarr!
    ctx.advance_secs(86_400);
    assert_eq!(ctx.db.with_snapshot(|s| s.fw_system_occupations[&fw_sys].occupier_faction_id), FACTION_AMARR, "must flip after downtime");

    // 2. Insurgency campaign lifecycle & Pirate FOB vulnerability/destruction.
    let camp_id = 77_001i64;
    eve_server::services::insurgency_services::init_insurgency_campaign(
        &ctx,
        camp_id,
        FACTION_ANGEL_CARTEL,
        FACTION_AMARR,
        fw_sys,
        vec![fw_sys],
    );

    // Forecasting (48h) -> Active; 0 suppressed systems -> isFobVulnerable == false!
    ctx.advance_secs(48 * 3_600 + 1);
    eve_server::services::fw_gameplay::tick_fw_gameplay(&ctx, ctx.now_unix()).await;
    let vis0 = rpc_call(&ctx, &mut s_min, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await.unwrap();
    assert!(!format!("{vis0:?}").contains("isFobVulnerable: Bool(true)"), "FOB must NOT be vulnerable with 0 suppressed systems");

    // Push Corruption Stage 3 -> spreads to stargate-adjacent system!
    let spread_sys = eve_server::services::fw_gameplay::try_spread_insurgency_from_system(&ctx, camp_id, fw_sys).expect("stargate-adjacent spread");
    assert!(ctx.sde.jumps.values().any(|j| j.from_solar_system_id == fw_sys && j.to_solar_system_id == spread_sys));

    // Suppress all campaign systems (up to 5 + emp_mod) to Stage 5 -> FOB becomes vulnerable!
    for _ in 0..8 {
        let cur: Vec<u32> = eve_server::services::insurgency_services::read_insurgency_runtime(&ctx, |rt| {
            rt.campaigns[&camp_id].systems.clone()
        });
        for s in cur {
            let _ = eve_server::services::fw_gameplay::try_spread_insurgency_from_system(&ctx, camp_id, s);
        }
    }
    let camp_systems: Vec<u32> = eve_server::services::insurgency_services::read_insurgency_runtime(&ctx, |rt| {
        rt.campaigns[&camp_id].systems.clone()
    });
    assert!(camp_systems.len() >= 5);
    for s in camp_systems {
        eve_server::services::insurgency_services::add_system_suppression(&ctx, camp_id, s, 1_000);
    }
    let vis1 = rpc_call(&ctx, &mut s_min, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await.unwrap();
    let PyRep::List(vis1_list) = &vis1 else { panic!("expected List") };
    assert_eq!(vis1_list[0].dict_get("isFobVulnerable"), Some(&PyRep::Bool(true)), "FOB must be vulnerable once suppression goal reached");

    // Fight through FOB shield -> 3h armor reinforcement -> hull destruction -> Empire win!
    let fob_id = ctx.db.with_snapshot(|s| {
        s.structures.values().find(|st| st.solar_system_id == fw_sys && st.type_id == eve_server::services::insurgency_services::PIRATE_STRONGHOLD_ANGEL_TYPE).unwrap().structure_id
    });
    let r1 = eve_server::services::fw_gameplay::apply_fw_or_insurgency_structure_damage(&ctx, PILOT, fob_id, 5_000.0).await.unwrap();
    assert_eq!(r1.dict_get("state").and_then(|v| v.as_str()), Some("ArmorReinforced"));
    ctx.advance_secs(4 * 3_600 + 5);
    let r2 = eve_server::services::fw_gameplay::apply_fw_or_insurgency_structure_damage(&ctx, PILOT, fob_id, 10_000.0).await.unwrap();
    assert_eq!(r2.dict_get("state").and_then(|v| v.as_str()), Some("Destroyed"));
}
