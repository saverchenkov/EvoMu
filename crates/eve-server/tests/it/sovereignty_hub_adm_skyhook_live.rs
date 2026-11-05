//! Live Sovereignty Hub, ADM, Upgrade Sites, Dev Indices, Sov Change Notifications, and Skyhook Reagents suite.
//!
//! Covers:
//! - `feat:sovereignty.sov-hub-anchor`
//! - `feat:sovereignty.sov-adm`
//! - `feat:sovereignty.sov-dev-indices`
//! - `feat:sovereignty.sov-upgrade-sites`
//! - `feat:sovereignty.sov-changed-notifications`
//! - `feat:sovereignty.skyhook-reagent-maturity`

#![forbid(unsafe_code)]

use crate::esi_fw_sov_incursions_live::{create_corp_and_alliance, rpc, select_gm_pilot, slash};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::sov_gameplay::{
    ENTOSIS_HUB_BASE_CAPTURE_SECS, ENTOSIS_NODE_BASE_CAPTURE_SECS,
    UPGRADE_MAJOR_THREAT_DETECTION, UPGRADE_PROSPECTING_ARRAY,
};
use eve_server::services::sov_services::{
    calculate_adm, scale_entosis_capture_seconds,
    TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB,
};
use glam::DVec3;
use std::sync::Arc;

const PILOT_1: i64 = 90_000_001;

fn keyval_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

async fn give_item(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, qty: i32) -> i64 {
    slash(ctx, sess, &format!("/giveitem {type_id} {qty}")).await.dict_get("itemID").and_then(|v| v.as_i64()).expect("itemID")
}

fn find_nullsec_system_with_power(ctx: &ServerContext) -> (u32, i64) {
    let mut sys_ids: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| eve_server::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, s.solar_system_id))
        .map(|s| s.solar_system_id)
        .collect();
    sys_ids.sort_unstable();
    for sys in sys_ids {
        let cels = ctx.sde.celestials_in_system(sys);
        let star_p: u64 = cels
            .iter()
            .filter(|c| c.group_id == 6)
            .map(|c| eve_server::services::quasar_sov_hub::star_power(&ctx.sde, c.type_id))
            .sum();
        let temp = cels.iter().find(|c| c.type_id == 11).map(|c| c.celestial_id);
        if star_p >= 700 && temp.is_some() {
            return (sys, temp.unwrap());
        }
    }
    let (s, t, _, _, _) = crate::sovereignty_equinox_gameplay::find_nullsec_planets(ctx);
    (s, t)
}

#[tokio::test]
async fn test_live_sov_hub_anchor_and_claim() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let (sys_id, _, _, _, _) = crate::sovereignty_equinox_gameplay::find_nullsec_planets(&ctx);
    let mut sess = select_gm_pilot(&ctx, 88_001, "demo", "demo-secret", PILOT_1).await;
    let (_corp, ally_opt) = create_corp_and_alliance(&ctx, &mut sess, "Sov Wardens", "SWARD", Some(("Sov Warden Alliance", "SWA"))).await;
    let ally = ally_opt.expect("alliance");

    let ship_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    slash(&ctx, &mut sess, &format!("/tr {sys_id}")).await;
    let ship_pos = DVec3::new(100_000.0, 0.0, 0.0);
    let _ = ctx.systems.get_or_spawn(sys_id).set_ball_position(ship_id, ship_pos).await;

    // Give Sovereignty Hub item to active ship cargo
    let hub_item = give_item(&ctx, &mut sess, TYPE_SOVEREIGNTY_HUB, 1).await;

    // Negative 1: Highsec anchoring rejection
    let highsec_sys = 30_000_142u32; // Jita
    slash(&ctx, &mut sess, &format!("/tr {highsec_sys}")).await;
    let err = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(hub_item), PyRep::String("Sov Hub Bad".into())]).await.unwrap_err();
    assert_eq!(err.code, "CannotAnchorSovHubOutsideNullSec");

    // Return to nullsec
    slash(&ctx, &mut sess, &format!("/tr {sys_id}")).await;
    let _ = ctx.systems.get_or_spawn(sys_id).set_ball_position(ship_id, ship_pos).await;

    // Live Anchor RPC
    let anchor_rep = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(hub_item), PyRep::String("Prime Sov Hub".into())]).await.expect("anchor Sov Hub");
    let hub_id = anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()).expect("hub_id");
    assert_eq!(anchor_rep.dict_get("state").and_then(|v| v.as_str()), Some("Onlining"));

    // Verify snapshot state during anchoring
    ctx.db.with_snapshot(|s| {
        let st = &s.structures[&hub_id];
        assert_eq!(st.state, "Onlining");
        assert_eq!(st.type_id, TYPE_SOVEREIGNTY_HUB);
        assert!(st.reinforced_until_unix > ctx.now_unix());
    });

    // Negative 2: Cannot anchor a second Sov Hub in the same system
    let hub_item2 = give_item(&ctx, &mut sess, TYPE_SOVEREIGNTY_HUB, 1).await;
    let err2 = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(hub_item2), PyRep::String("Dup Sov Hub".into())]).await.unwrap_err();
    assert_eq!(err2.code, "SovHubAlreadyPresent");

    // Advance clock past 900s onlining timer
    ctx.advance_secs(905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // Verify Sov Hub is now ShieldVulnerable and sovereignty is claimed by the alliance
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.structures[&hub_id].state, "ShieldVulnerable");
        assert_eq!(s.system_states.get(&sys_id).and_then(|st| st.sov_alliance_id), Some(ally));
    });
}

#[tokio::test]
async fn test_live_sov_adm_and_entosis_scaling() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let (sys_id, _, _, _, _) = crate::sovereignty_equinox_gameplay::find_nullsec_planets(&ctx);
    let mut sess = select_gm_pilot(&ctx, 88_002, "demo", "demo-secret", PILOT_1).await;
    let (_corp, _ally) = create_corp_and_alliance(&ctx, &mut sess, "ADM Corps", "ADMC", Some(("ADM Alliance", "ADMA"))).await;

    let ship_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    slash(&ctx, &mut sess, &format!("/tr {sys_id}")).await;
    let _ = ctx.systems.get_or_spawn(sys_id).set_ball_position(ship_id, DVec3::new(50_000.0, 0.0, 0.0)).await;

    let hub_item = give_item(&ctx, &mut sess, TYPE_SOVEREIGNTY_HUB, 1).await;
    let rep = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(hub_item), PyRep::String("ADM Hub".into())]).await.expect("anchor hub");
    let _hub_id = rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // Check base ADM is 1.0 via GetAllDevelopmentIndices
    let all_idx = rpc(&ctx, &mut sess, "devIndexManager", "GetAllDevelopmentIndices", &[]).await.expect("GetAllDevelopmentIndices");
    if let PyRep::Dict(entries) = &all_idx {
        let entry = entries.iter().find(|(k, _)| k.as_i64() == Some(i64::from(sys_id))).map(|(_, v)| v).expect("entry for sys_id");
        assert_eq!(entry.dict_get("adm").and_then(|v| v.as_f64()), Some(1.0));
    }

    // Advance 7 days to grow strategic index to 1
    ctx.advance_secs(7 * 86_400);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // Grow dev indices: 15 kills (mil = 2) and 15,000 m3 mined (ind = 3)
    eve_server::services::sov_gameplay::record_sov_system_activity(&ctx, sys_id, 15, 15_000.0);

    // Formula check: mil=2 (1.2) + ind=3 (1.7) + strat=1 (0.4) + base 1.0 = 4.3
    let expected_adm = calculate_adm(2, 3, 1);
    assert_eq!(expected_adm, 4.3);

    // Verify GetAllDevelopmentIndices returns ADM 4.3
    let all_idx_after = rpc(&ctx, &mut sess, "devIndexManager", "GetAllDevelopmentIndices", &[]).await.expect("GetAllDevelopmentIndices");
    if let PyRep::Dict(entries) = &all_idx_after {
        let entry = entries.iter().find(|(k, _)| k.as_i64() == Some(i64::from(sys_id))).map(|(_, v)| v).expect("entry for sys_id");
        assert_eq!(entry.dict_get("adm").and_then(|v| v.as_f64()), Some(4.3));
        assert_eq!(entry.dict_get("military").and_then(|v| v.as_i64()), Some(2));
        assert_eq!(entry.dict_get("industrial").and_then(|v| v.as_i64()), Some(3));
        assert_eq!(entry.dict_get("strategic").and_then(|v| v.as_i64()), Some(1));
    }

    // Entosis scaling check: hub base 600s * ADM, node base 240s * ADM
    let hub_scaled = scale_entosis_capture_seconds(600, expected_adm);
    assert_eq!(hub_scaled, 2580);
    assert_eq!(ENTOSIS_HUB_BASE_CAPTURE_SECS * expected_adm, 2580.0);

    let node_scaled = scale_entosis_capture_seconds(240, expected_adm);
    assert_eq!(node_scaled, 1032);
    assert_eq!(ENTOSIS_NODE_BASE_CAPTURE_SECS * expected_adm, 1032.0);
}

#[tokio::test]
async fn test_live_sov_dev_indices_progression_and_notifications() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let (sys_id, _, _, _, _) = crate::sovereignty_equinox_gameplay::find_nullsec_planets(&ctx);
    let mut sess = select_gm_pilot(&ctx, 88_003, "demo", "demo-secret", PILOT_1).await;
    let (_corp, _ally) = create_corp_and_alliance(&ctx, &mut sess, "Dev Corps", "DEVC", Some(("Dev Alliance", "DEVA"))).await;

    let ship_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    slash(&ctx, &mut sess, &format!("/tr {sys_id}")).await;
    let _ = ctx.systems.get_or_spawn(sys_id).set_ball_position(ship_id, DVec3::new(50_000.0, 0.0, 0.0)).await;

    let hub_item = give_item(&ctx, &mut sess, TYPE_SOVEREIGNTY_HUB, 1).await;
    let rep = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(hub_item), PyRep::String("Dev Hub".into())]).await.expect("anchor hub");
    let _hub_id = rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // Drain initial notifications
    let _ = ctx.drain_notifications_for_char(PILOT_1);

    // Kills progression: 5 kills -> mil level 1, 15 kills -> mil level 2
    eve_server::services::sov_gameplay::record_sov_system_activity(&ctx, sys_id, 5, 0.0);
    ctx.db.with_snapshot(|s| assert_eq!(s.system_states[&sys_id].military_index, 1));
    let notifs1 = ctx.drain_notifications_for_char(PILOT_1);
    assert!(notifs1.iter().any(|n| n.event == "OnSolarSystemDevIndexChanged"));

    eve_server::services::sov_gameplay::record_sov_system_activity(&ctx, sys_id, 10, 0.0); // 15 total
    ctx.db.with_snapshot(|s| assert_eq!(s.system_states[&sys_id].military_index, 2));

    // Mining progression: 1,000 m3 -> ind level 1, 15,000 m3 -> ind level 3
    eve_server::services::sov_gameplay::record_sov_system_activity(&ctx, sys_id, 0, 1_000.0);
    ctx.db.with_snapshot(|s| assert_eq!(s.system_states[&sys_id].industrial_index, 1));
    let notifs2 = ctx.drain_notifications_for_char(PILOT_1);
    assert!(notifs2.iter().any(|n| n.event == "OnSolarSystemDevIndexChanged"));

    eve_server::services::sov_gameplay::record_sov_system_activity(&ctx, sys_id, 0, 14_000.0); // 15,000 total
    ctx.db.with_snapshot(|s| assert_eq!(s.system_states[&sys_id].industrial_index, 3));

    // Strategic index: 0 days -> 0, 7 days -> 1
    ctx.advance_secs(7 * 86_400);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    ctx.db.with_snapshot(|s| assert_eq!(s.system_states[&sys_id].strategic_index, 1));
}

#[tokio::test]
async fn test_live_sov_upgrade_sites_spawning() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let (sys_id, temp_planet) = find_nullsec_system_with_power(&ctx);
    let mut sess = select_gm_pilot(&ctx, 88_004, "demo", "demo-secret", PILOT_1).await;
    let (_corp, _ally) = create_corp_and_alliance(&ctx, &mut sess, "Upg Corps", "UPGC", Some(("Upg Alliance", "UPGA"))).await;

    let ship_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    slash(&ctx, &mut sess, &format!("/tr {sys_id}")).await;
    let _ = ctx.systems.get_or_spawn(sys_id).set_ball_position(ship_id, DVec3::new(100_000.0, 0.0, 0.0)).await;

    let hub_item = give_item(&ctx, &mut sess, TYPE_SOVEREIGNTY_HUB, 1).await;
    let rep = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(hub_item), PyRep::String("Upg Hub".into())]).await.expect("anchor hub");
    let hub_id = rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // Anchor Skyhook at Temperate planet (+1,500 planetary workforce)
    let cels = ctx.sde.celestials_in_system(sys_id);
    let temp_pos = cels.iter().find(|c| c.celestial_id == temp_planet).unwrap().position;
    let _ = ctx.systems.get_or_spawn(sys_id).set_ball_position(ship_id, temp_pos + DVec3::new(100_000.0, 0.0, 0.0)).await;
    let skyhook_item = give_item(&ctx, &mut sess, TYPE_ORBITAL_SKYHOOK, 1).await;
    let _ = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(skyhook_item), PyRep::Integer(temp_planet)]).await.expect("anchor Skyhook");

    // Return to hub area
    let _ = ctx.systems.get_or_spawn(sys_id).set_ball_position(ship_id, DVec3::new(100_000.0, 0.0, 0.0)).await;

    // Fuel the hub with Superionic Ice
    let ice_item = give_item(&ctx, &mut sess, 81144, 200).await;
    let mut add_fuel = Vec::new();
    eve_proto::quasar::encode_bytes_field(1, &crate::quasar_sov_equinox_hub::idm(ice_item as u64), &mut add_fuel);
    eve_proto::quasar::encode_varint_field(2, 200, &mut add_fuel);
    eve_proto::quasar::encode_bytes_field(3, &crate::quasar_sov_equinox_hub::idm(hub_id as u64), &mut add_fuel);
    let fuel_res = crate::quasar_sov_equinox_hub::rpc(&ctx, &mut sess, "eve_public.sovereignty.hub.fuel.api.AddRequest", add_fuel).await;
    assert_eq!(fuel_res.status_code, eve_server::quasar_gateway::STATUS_OK);

    // Provide workforce and upgrade items into active ship cargo at the hub
    let wf_item = give_item(&ctx, &mut sess, 87_710, 1).await; // Workforce Mecha-Tooling 1 (+5,000 workforce)
    let threat_item = give_item(&ctx, &mut sess, UPGRADE_MAJOR_THREAT_DETECTION, 1).await;

    // Step 1: Install & Online Major Threat Detection
    let mut proc = Vec::new();
    for iid in [wf_item as u64, threat_item as u64] {
        eve_proto::quasar::encode_bytes_field(1, &crate::quasar_sov_equinox_hub::idm(iid), &mut proc);
    }
    for tid in [87_710u64, UPGRADE_MAJOR_THREAT_DETECTION as u64] {
        let mut cfg = Vec::new();
        eve_proto::quasar::encode_bytes_field(1, &crate::quasar_sov_equinox_hub::idm(tid), &mut cfg);
        eve_proto::quasar::encode_varint_field(2, 1, &mut cfg);
        eve_proto::quasar::encode_bytes_field(2, &cfg, &mut proc);
    }
    eve_proto::quasar::encode_bytes_field(3, &crate::quasar_sov_equinox_hub::idm(hub_id as u64), &mut proc);

    let res = crate::quasar_sov_equinox_hub::rpc(&ctx, &mut sess, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc).await;
    assert_eq!(res.status_code, eve_server::quasar_gateway::STATUS_OK);

    // Tick cluster: spawns combat anomaly and combat NPCs
    ctx.advance_secs(5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let anoms = ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&sys_id).cloned().unwrap_or_default());
    assert!(anoms.iter().any(|a| a.anomaly_type == "Combat"), "combat anomaly spawned");

    let combat_npc_count = ctx.db.with_snapshot(|s| s.sov_runtime.sov_upgrade_combat_npcs.len());
    assert!(combat_npc_count > 0, "combat wave NPCs spawned for threat detection upgrade");

    // Step 2: Install Prospecting Array, toggle Major Threat Detection offline, online Prospecting Array
    let prosp_item = give_item(&ctx, &mut sess, UPGRADE_PROSPECTING_ARRAY, 1).await;
    let mut proc2 = Vec::new();
    eve_proto::quasar::encode_bytes_field(1, &crate::quasar_sov_equinox_hub::idm(prosp_item as u64), &mut proc2);
    for (tid, online) in [
        (87_710u64, 1u64),
        (UPGRADE_MAJOR_THREAT_DETECTION as u64, 0u64),
        (UPGRADE_PROSPECTING_ARRAY as u64, 1u64),
    ] {
        let mut cfg = Vec::new();
        eve_proto::quasar::encode_bytes_field(1, &crate::quasar_sov_equinox_hub::idm(tid), &mut cfg);
        eve_proto::quasar::encode_varint_field(2, online, &mut cfg);
        eve_proto::quasar::encode_bytes_field(2, &cfg, &mut proc2);
    }
    eve_proto::quasar::encode_bytes_field(3, &crate::quasar_sov_equinox_hub::idm(hub_id as u64), &mut proc2);

    let res2 = crate::quasar_sov_equinox_hub::rpc(&ctx, &mut sess, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc2).await;
    assert_eq!(res2.status_code, eve_server::quasar_gateway::STATUS_OK);

    ctx.advance_secs(5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let anoms2 = ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&sys_id).cloned().unwrap_or_default());
    assert!(anoms2.iter().any(|a| a.anomaly_type == "Ore"), "ore anomaly spawned");

    let ore_anom = anoms2.iter().find(|a| a.anomaly_type == "Ore").unwrap();
    assert!(!ore_anom.ore_types.is_empty(), "ore types configured");
    assert!(ore_anom.remaining_volume > 0.0, "ore volume configured");
}

#[tokio::test]
async fn test_live_sov_changed_broadcasts_on_capture_and_loss() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let (sys_id, _, _, _, _) = crate::sovereignty_equinox_gameplay::find_nullsec_planets(&ctx);
    let mut sess = select_gm_pilot(&ctx, 88_005, "demo", "demo-secret", PILOT_1).await;
    let (_corp, ally_opt) = create_corp_and_alliance(&ctx, &mut sess, "Notif Corps", "NOTC", Some(("Notif Alliance", "NOTA"))).await;
    let ally = ally_opt.expect("alliance");

    let ship_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    slash(&ctx, &mut sess, &format!("/tr {sys_id}")).await;
    let _ = ctx.systems.get_or_spawn(sys_id).set_ball_position(ship_id, DVec3::new(100_000.0, 0.0, 0.0)).await;

    // Drain setup notifications
    let _ = ctx.drain_notifications_for_char(PILOT_1);

    // 1. CAPTURE CASE: Anchor Sov Hub and advance past onlining timer
    let hub_item = give_item(&ctx, &mut sess, TYPE_SOVEREIGNTY_HUB, 1).await;
    let rep = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(hub_item), PyRep::String("Broadcast Hub".into())]).await.expect("anchor hub");
    let hub_id = rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // Verify OnSovereigntyChanged and OnSolarSystemSovStructuresUpdated were broadcast on capture
    let capture_notifs = ctx.drain_notifications_for_char(PILOT_1);
    let sov_changed_cap = capture_notifs.iter().find(|n| n.event == "OnSovereigntyChanged");
    assert!(sov_changed_cap.is_some(), "OnSovereigntyChanged fired on capture");
    let cap_args = &sov_changed_cap.unwrap().args;
    assert_eq!(cap_args.get(0).and_then(|a| a.as_i64()), Some(i64::from(sys_id)));
    assert_eq!(cap_args.get(1).and_then(|a| a.as_i64()), Some(ally));

    let sov_structs_cap = capture_notifs.iter().find(|n| n.event == "OnSolarSystemSovStructuresUpdated");
    assert!(sov_structs_cap.is_some(), "OnSolarSystemSovStructuresUpdated fired on capture");

    // 2. LOSS CASE: Unanchor Sov Hub via structureDeployment.Unanchor RPC
    let unanchor_rep = rpc(&ctx, &mut sess, "structureDeployment", "Unanchor", &[PyRep::Integer(hub_id)]).await.expect("unanchor hub");
    assert_eq!(unanchor_rep.dict_get("state").and_then(|v| v.as_str()), Some("Unanchoring"));

    // Advance past 7-day unanchoring countdown
    ctx.advance_secs(7 * 86_400 + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // Verify hub is packaged/removed and sov dropped
    ctx.db.with_snapshot(|s| {
        assert!(s.structures.get(&hub_id).is_none(), "hub removed from space");
        assert_eq!(s.system_states.get(&sys_id).and_then(|st| st.sov_alliance_id), None, "sov dropped");
    });

    // Verify OnSovereigntyChanged and OnSolarSystemSovStructuresUpdated were broadcast on loss
    let loss_notifs = ctx.drain_notifications_for_char(PILOT_1);
    let sov_changed_loss = loss_notifs.iter().find(|n| n.event == "OnSovereigntyChanged");
    assert!(sov_changed_loss.is_some(), "OnSovereigntyChanged fired on loss");
    let loss_args = &sov_changed_loss.unwrap().args;
    assert_eq!(loss_args.get(0).and_then(|a| a.as_i64()), Some(i64::from(sys_id)));
    assert!(matches!(loss_args.get(1), Some(PyRep::None)), "allianceID is None on loss");

    let sov_structs_loss = loss_notifs.iter().find(|n| n.event == "OnSolarSystemSovStructuresUpdated");
    assert!(sov_structs_loss.is_some(), "OnSolarSystemSovStructuresUpdated fired on loss");
}

#[tokio::test]
async fn test_live_skyhook_harvest_split_and_reagent_maturity() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let (sys_id, _, lava_planet, _ice_planet, _) = crate::sovereignty_equinox_gameplay::find_nullsec_planets(&ctx);
    let mut sess = select_gm_pilot(&ctx, 88_006, "demo", "demo-secret", PILOT_1).await;
    let (_corp, _ally) = create_corp_and_alliance(&ctx, &mut sess, "Sky Corps", "SKYC", Some(("Sky Alliance", "SKYA"))).await;

    // Anchor Sov Hub first so system has sovereign owner
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    slash(&ctx, &mut sess, &format!("/tr {sys_id}")).await;
    let hub_item = give_item(&ctx, &mut sess, TYPE_SOVEREIGNTY_HUB, 1).await;
    let _ = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(hub_item), PyRep::String("Sov Hub Sky".into())]).await.expect("anchor hub");
    ctx.advance_secs(905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // Warp near the lava planet (produces Magmatic Gas 81143)
    let cels = ctx.sde.celestials_in_system(sys_id);
    let lava_pos = cels.iter().find(|c| c.celestial_id == lava_planet).unwrap().position;
    let _ = ctx.systems.get_or_spawn(sys_id).set_ball_position(ship_id, lava_pos + DVec3::new(100_000.0, 0.0, 0.0)).await;

    // Anchor Skyhook at lava planet
    let skyhook_item = give_item(&ctx, &mut sess, TYPE_ORBITAL_SKYHOOK, 1).await;
    let sky_rep = rpc(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(skyhook_item), PyRep::Integer(lava_planet)]).await.expect("anchor Skyhook");
    let skyhook_id = sky_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

    // Verify initial bays: 0 secure, 0 insecure
    ctx.db.with_snapshot(|s| {
        let (reagent_tid, sec, insec, _) = s.sov_runtime.skyhook_reagent_bays[&skyhook_id];
        assert_eq!(reagent_tid, TYPE_MAGMATIC_GAS);
        assert_eq!(sec, 0);
        assert_eq!(insec, 0);
    });

    // Advance 2 hours: yields 2 * 50 = 100 units total (50 secure, 50 insecure)
    ctx.advance_secs(2 * 3600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    ctx.db.with_snapshot(|s| {
        let (_, sec, insec, _) = s.sov_runtime.skyhook_reagent_bays[&skyhook_id];
        assert_eq!(sec, 50, "50% harvested directly into secure bay");
        assert_eq!(insec, 50, "50% harvested into insecure bay");
    });

    // Advance 12 hours: 25 matured from earlier 50 insecure, plus 300 secure + 300 insecure from new harvest
    ctx.advance_secs(12 * 3600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    ctx.db.with_snapshot(|s| {
        let (_, sec, insec, _) = s.sov_runtime.skyhook_reagent_bays[&skyhook_id];
        assert_eq!(sec, 375, "25 matured + 300 new secure");
        assert_eq!(insec, 325, "25 remaining + 300 new insecure");
    });

    // Place ship within docking/linking range of the Skyhook (within 1,000m)
    let skyhook_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures[&skyhook_id].position));
    let _ = ctx.systems.get_or_spawn(sys_id).set_ball_position(ship_id, skyhook_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;

    // Live Quasar component extraction to active ship
    let ext_call = eve_server::services::sov_gameplay::handle_skyhook_component_call(
        &ctx, &sess, sys_id, PILOT_1, ship_id, skyhook_id, "orbitalSkyhook", "ExtractReagentsToShip", &[],
    ).await.expect("ExtractReagentsToShip");

    assert_eq!(keyval_get(&ext_call, "extractedQuantity").and_then(|v| v.as_i64()), Some(375));

    // Verify reagents now in ship cargo and secure bay is 0
    ctx.db.with_snapshot(|s| {
        let (_, sec, insec, _) = s.sov_runtime.skyhook_reagent_bays[&skyhook_id];
        assert_eq!(sec, 0, "secure bay empty after extraction");
        assert_eq!(insec, 325);
        let cargo_reagent = s.items.values().find(|i| i.location_id == ship_id && i.type_id == TYPE_MAGMATIC_GAS);
        assert!(cargo_reagent.is_some(), "Magmatic Gas present in ship cargo");
        assert_eq!(cargo_reagent.unwrap().quantity, 375);
    });
}
