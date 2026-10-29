//! Part 3 of Equinox Sovereignty closure tests: Hub Fuel, Power, Workforce, Upgrades, Sites, Hacking, Flex & Tenebrex (`SO-5`, `Q-8`).

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::{field, fields, id_req, idm, rpc, structure, subscribe, varint};
use crate::sovereignty_equinox_gameplay_closure::{
    drain_notif_events, drain_quasar, find_nullsec_planets, fit_siege_artillery,
    lock_target_via_rpc, make_ball, open_sqlite_ctx, register_online_pilot, rpc_call,
    step_weapon_ticks, temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::services::sov_gameplay::{
    TENEBREX_SPOOL_UP_SECS, TYPE_ANSIBLEX_JUMP_BRIDGE, TYPE_TENEBREX_CYNO_JAMMER,
    UPGRADE_ADVANCED_LOGISTICS_NETWORK, UPGRADE_CYNOSURAL_NAVIGATION,
    UPGRADE_CYNOSURAL_SUPPRESSION, UPGRADE_MAJOR_THREAT_DETECTION, UPGRADE_PROSPECTING_ARRAY,
};
use eve_server::services::sov_services::{
    claim_system_sovereignty, is_hub_hacked, TYPE_MAGMATIC_GAS, TYPE_PHAROLUX_CYNO_BEACON,
    TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE,
};
use glam::DVec3;

#[tokio::test]
async fn test_sov_hub_fuel_power_workforce_upgrades_sites_hacking_flex_and_tenebrex() {
    // parity: feat:sovereignty.sov-hub-fuel
    // DATA GAP: feat:sovereignty.sov-power (open until real SDE tables imported)
    // DATA GAP: feat:sovereignty.sov-workforce (open until real SDE tables imported)
    // DATA GAP: feat:sovereignty.sov-hub-upgrades (open until real SDE tables imported)
    // parity: feat:sovereignty.sov-upgrade-sites
    // parity: feat:sovereignty.sov-hub-hacking
    // parity: feat:sovereignty.sov-structures-flex
    // parity: feat:modules.cynosural-field-jammer-structure
    // DATA GAP: quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj (open until real SDE tables imported)
    // DATA GAP: quasar:eveProto/generated/eve_public/sovereignty/resource/star/api/requests_pb2.pyj (open until real SDE tables imported)
    // parity: notify:OnSovHubHacked
    // parity: notify:OnCynoJammerChanged
    eve_testkit::attest(&[
        "feat:sovereignty.sov-hub-fuel",
        "feat:sovereignty.sov-upgrade-sites",
        "feat:sovereignty.sov-hub-hacking",
        "feat:sovereignty.sov-structures-flex",
        "feat:modules.cynosural-field-jammer-structure",
        "notify:OnSovHubHacked",
        "notify:OnCynoJammerChanged",
    ]);

    let db_path = temp_sqlite_path("sov_hub_flex");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, _, _, _, _) = find_nullsec_planets(&ctx);
    let neighbor_sys = ctx
        .sde
        .jumps
        .values()
        .find(|j| {
            j.from_solar_system_id == null_sys
                && ctx.sde.solar_systems.get(&j.to_solar_system_id).is_some_and(|s| s.security_status <= 0.0)
        })
        .map(|j| j.to_solar_system_id)
        .expect("adjacent null-sec system");

    let hub_a = 1_060_000_001i64;
    let hub_b = 1_060_000_002i64;
    let hub_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_dir = register_online_pilot(
        &ctx, 73_001, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 11_957, hub_pos + DVec3::new(40_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let mut rx = subscribe(&ctx, &mut s_dir).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default();
        snap.system_states.entry(neighbor_sys).or_default();
        snap.structures.insert(hub_a, structure(hub_a, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(hub_b, structure(hub_b, CORP, neighbor_sys, TYPE_SOVEREIGNTY_HUB));
        for (iid, tid, loc, qty) in [
            (995_001, TYPE_MAGMATIC_GAS, hub_a, 5_000),
            (995_002, TYPE_SUPERIONIC_ICE, hub_a, 5_000),
            (995_003, UPGRADE_MAJOR_THREAT_DETECTION, hub_a, 1),
            (995_004, UPGRADE_PROSPECTING_ARRAY, hub_a, 1),
            (995_005, UPGRADE_CYNOSURAL_NAVIGATION, hub_a, 1),
            (995_006, UPGRADE_CYNOSURAL_SUPPRESSION, hub_a, 1),
            (995_007, UPGRADE_ADVANCED_LOGISTICS_NETWORK, hub_a, 1),
            (995_008, 88_227, hub_a, 1), // Power Monitoring Division 3 (+750 power)
            (995_009, 88_229, hub_a, 1), // Workforce Mecha-Tooling 3 (+6000 workforce)
            (995_020, TYPE_ANSIBLEX_JUMP_BRIDGE, PILOT_SHIP, 1),
            (995_021, TYPE_PHAROLUX_CYNO_BEACON, PILOT_SHIP, 1),
            (995_022, TYPE_TENEBREX_CYNO_JAMMER, PILOT_SHIP, 1),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: PILOT,
                    location_id: loc,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    ..Default::default()
                },
            );
        }
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(hub_a, TYPE_SOVEREIGNTY_HUB, hub_pos)).await;
    claim_system_sovereignty(&ctx, null_sys, ALLIANCE, CORP, hub_a).expect("claim null_sys");
    claim_system_sovereignty(&ctx, neighbor_sys, ALLIANCE, CORP, hub_b).expect("claim neighbor_sys");

    // 1. Star power configuration (`resource.star.api.GetAllConfigurationsRequest`), Fuel Access Group, & Flex upgrade gating.
    let star_rsp = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest", Vec::new()).await;
    assert_eq!(star_rsp.status_code, STATUS_OK);
    let star_cfgs = fields(&star_rsp.payload.unwrap().value, 1);
    assert!(!star_cfgs.is_empty() && varint(&star_cfgs[0], 2).unwrap_or(0) >= 200);

    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "sovMgr", "SetSovHubFuelAccessGroup", &[PyRep::Integer(hub_a), PyRep::Integer(42_001)])
            .await.expect("SetSovHubFuelAccessGroup"),
        PyRep::Integer(42_001)
    );
    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(995_020), PyRep::String("Ansiblex".into())])
            .await.unwrap_err().code,
        "RequiredSovUpgradeOffline"
    );

    // 2. Cross-system Workforce import/export (`hub.workforce.api.ConfigureRequest`) + `hub.api.GetResourcesRequest`.
    let wf_before = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    let base_wf_a = varint(&field(&wf_before, 2).unwrap(), 1).unwrap_or(0);

    let (mut exp, mut exp_cfg, mut cfg_b) = (Vec::new(), Vec::new(), id_req(1, hub_b as u64));
    encode_bytes_field(3, &idm(null_sys as u64), &mut exp);
    encode_varint_field(2, 500, &mut exp);
    encode_bytes_field(3, &exp, &mut exp_cfg);
    encode_bytes_field(2, &exp_cfg, &mut cfg_b);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_b).await.status_code, STATUS_OK);

    let (mut src, mut imp, mut imp_cfg, mut cfg_a) = (Vec::new(), Vec::new(), Vec::new(), id_req(1, hub_a as u64));
    encode_bytes_field(1, &idm(neighbor_sys as u64), &mut src);
    encode_bytes_field(1, &src, &mut imp);
    encode_bytes_field(2, &imp, &mut imp_cfg);
    encode_bytes_field(2, &imp_cfg, &mut cfg_a);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_a).await.status_code, STATUS_OK);

    let wf_after = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&wf_after, 2).unwrap(), 1).unwrap_or(0), base_wf_a + 500);

    // 3. Fuel Hub, install + online upgrades, and verify Sovereign Combat + Prospecting anomalies spawn via `tick_background_cluster`!
    for (fuel_iid, amt) in [(995_001i64, 3_000u64), (995_002i64, 3_000u64)] {
        let mut add = id_req(1, fuel_iid as u64);
        encode_varint_field(2, amt, &mut add);
        encode_bytes_field(3, &idm(hub_a as u64), &mut add);
        assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.fuel.api.AddRequest", add).await.status_code, STATUS_OK);
    }
    let mut proc = Vec::new();
    for iid in [995_008u64, 995_009, 995_003, 995_004, 995_005, 995_006, 995_007] {
        encode_bytes_field(1, &idm(iid), &mut proc);
    }
    for tid in [88_227u64, 88_229, UPGRADE_MAJOR_THREAT_DETECTION as u64, UPGRADE_PROSPECTING_ARRAY as u64] {
        let mut cfg = Vec::new();
        encode_bytes_field(1, &idm(tid), &mut cfg);
        encode_varint_field(2, 1, &mut cfg);
        encode_bytes_field(2, &cfg, &mut proc);
    }
    encode_bytes_field(3, &idm(hub_a as u64), &mut proc);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc).await.status_code, STATUS_OK);

    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let anoms = ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&null_sys).cloned().unwrap_or_default());
    assert!(anoms.iter().any(|a| a.anomaly_type == "Combat" && a.name.contains("Major Threat")));
    assert!(anoms.iter().any(|a| a.anomaly_type == "Ore" && a.name.contains("Prospecting Array")));
    let sys_balls = ctx.systems.get_or_spawn(null_sys).list_balls().await.unwrap();
    assert!(
        sys_balls.iter().any(|b| b.type_id == 1_230 || b.type_id == 1_228),
        "Ore upgrade anomaly must spawn real mineable asteroid balls in space"
    );
    assert!(
        sys_balls.iter().any(|b| b.type_id == 37_453),
        "Combat upgrade anomaly must spawn real combat-capable NPC rat balls in space"
    );

    // 4. Hostile Data Analyzer (`22175`) hacking on Sov Hub emits `OnSovHubHacked` and disables upgrades!
    let hacker_cid = 90_000_077i64;
    let hacker_ship = 100_000_077i64;
    let mut s_hacker = register_online_pilot(
        &ctx, 73_010, "demo2", "demo2-secret", hacker_cid, 98_000_077, Some(99_000_077),
        null_sys, hacker_ship, 621, hub_pos + DVec3::new(2_500.0, 0.0, 0.0), 0,
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(hacker_cid).or_default();
        for sid in [3_405u32, 21_718] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        snap.items.insert(
            995_100,
            InventoryItemRecord {
                item_id: 995_100,
                type_id: 22_175,
                owner_id: hacker_cid,
                location_id: hacker_ship,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(995_100, true);
    });
    ctx.invalidate_ship_fitting(hacker_ship);
    lock_target_via_rpc(&ctx, &mut s_hacker, null_sys, hacker_ship, hub_a).await;
    let _ = drain_notif_events(&ctx, PILOT);
    let hack_rep = rpc_call(&ctx, &mut s_hacker, "dogmaIM", "Activate", &[PyRep::Integer(995_100), PyRep::String("dataHacking".into()), PyRep::Integer(hub_a)])
        .await.expect("hack Sov Hub via Data Analyzer I");
    assert_eq!(hack_rep.dict_get("containerUnlocked"), Some(&PyRep::Bool(false)));
    assert!(!is_hub_hacked(&ctx, hub_a));
    rpc_call(&ctx, &mut s_hacker, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(hub_a)])
        .await.expect("start live hacking minigame on Sov Hub");
    let mut won = false;
    for _pass in 0..8 {
        for tile in 1..24 {
            for _ in 0..5 {
                if let Ok(step) = rpc_call(&ctx, &mut s_hacker, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(hub_a), PyRep::Integer(tile)]).await {
                    if step.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
                        won = true;
                        break;
                    }
                } else {
                    break;
                }
            }
            if won {
                break;
            }
        }
        if won {
            break;
        }
    }
    assert!(won, "Data Analyzer hacking minigame on Sov Hub must complete with Won");
    assert!(is_hub_hacked(&ctx, hub_a));
    assert!(drain_notif_events(&ctx, PILOT).iter().any(|e| e == "OnSovHubHacked"));
    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(995_020), PyRep::String("Ansiblex".into())])
            .await.unwrap_err().code,
        "SovHubHacked"
    );

    ctx.advance_secs(3_605);
    ctx.db.with_mut_snapshot(|snap| {
        for u in &mut snap.sov_runtime.sov_hubs.get_mut(&hub_a).unwrap().upgrades {
            u.low_power = false;
            if [UPGRADE_ADVANCED_LOGISTICS_NETWORK, UPGRADE_CYNOSURAL_NAVIGATION, UPGRADE_CYNOSURAL_SUPPRESSION].contains(&u.type_id) {
                u.online = false;
            }
        }
    });

    // R-SOVFW-05: Verify 35840, 35841, 37534 all require their respective Sov Hub upgrades online
    for (flex_iid, flex_label) in [
        (995_020, "Ansiblex"),
        (995_021, "Pharolux"),
        (995_022, "Tenebrex"),
    ] {
        assert_eq!(
            rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(flex_iid), PyRep::String(flex_label.into())])
                .await.unwrap_err().code,
            "RequiredSovUpgradeOffline"
        );
    }

    // Online Cynosural Suppression upgrade via authentic Quasar ProcessConfigurationRequest
    let mut on_proc = Vec::new();
    let mut cfg = Vec::new();
    encode_bytes_field(1, &idm(UPGRADE_CYNOSURAL_SUPPRESSION as u64), &mut cfg);
    encode_varint_field(2, 1, &mut cfg);
    encode_bytes_field(2, &cfg, &mut on_proc);
    encode_bytes_field(3, &idm(hub_a as u64), &mut on_proc);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", on_proc).await.status_code, STATUS_OK);

    // 5. Tenebrex Cyno Jammer (`37534`): 5m spool-up, `OnCynoJammerChanged`, `CynoJammed`, & immediate jam stop on reinforcement!
    let deep_space = DVec3::new(50_000_000.0, 50_000_000.0, 50_000_000.0);
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(PILOT_SHIP, 11_957, deep_space)).await;
    let tenebrex_rep = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(995_022), PyRep::String("Tenebrex Jammer".into())])
        .await.expect("anchor Tenebrex");
    let tenebrex_id = tenebrex_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&tenebrex_id) {
            st.fuel_blocks = 500;
        }
    });
    // Advance through anchoring (15m) + anchoring timer (24h) + onlining (15m) naturally:
    ctx.advance_secs(15 * 60 + 24 * 3_600 + 15 * 60 + 10);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&tenebrex_id].state.clone()), "ShieldVulnerable");
    assert!(ctx.db.with_snapshot(|s| s.sov_runtime.tenebrex_spool_ready_unix.contains_key(&tenebrex_id)));
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(PILOT).or_default();
        for sid in [21_603u32, 3_335, 12_092, 22_761] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        snap.items.insert(
            995_200,
            InventoryItemRecord {
                item_id: 995_200,
                type_id: 21_096,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            995_201,
            InventoryItemRecord {
                item_id: 995_201,
                type_id: 16_273,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 2_000,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(995_200, true);
    });
    ctx.invalidate_ship_fitting(PILOT_SHIP);

    let jam_during_spool = rpc_call(&ctx, &mut s_dir, "beyonce", "GetCynoJammerState", &[]).await.expect("GetCynoJammerState");
    assert_eq!(jam_during_spool.dict_get("isJammed"), Some(&PyRep::Bool(false)));

    let _ = drain_notif_events(&ctx, PILOT);
    ctx.advance_secs(TENEBREX_SPOOL_UP_SECS + 5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(drain_notif_events(&ctx, PILOT).iter().any(|e| e == "OnCynoJammerChanged"));
    let jam_ready = rpc_call(&ctx, &mut s_dir, "beyonce", "GetCynoJammerState", &[]).await.expect("GetCynoJammerState");
    assert_eq!(jam_ready.dict_get("isJammed"), Some(&PyRep::Bool(true)));
    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "dogmaIM", "Activate", &[PyRep::Integer(995_200), PyRep::String("cynosuralGeneration".into())])
            .await.unwrap_err().code,
        "CynoJammed"
    );

    ctx.db.with_mut_snapshot(|s| {
        if let Some(st) = s.structures.get_mut(&tenebrex_id) {
            st.shield_hp = 1_500.0;
        }
    });
    fit_siege_artillery(&ctx, hacker_cid, hacker_ship, 995_300);
    let t_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures[&tenebrex_id].position));
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(hacker_ship, 19_722, t_pos + DVec3::new(2_500.0, 0.0, 0.0))).await;
    lock_target_via_rpc(&ctx, &mut s_hacker, null_sys, hacker_ship, tenebrex_id).await;
    for _ in 0..4 {
        rpc_call(&ctx, &mut s_hacker, "dogmaIM", "Activate", &[
            PyRep::Integer(995_300),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(tenebrex_id),
        ]).await.expect("attack Tenebrex");
        step_weapon_ticks(&ctx, null_sys).await;
        ctx.advance_secs(25);
        if ctx.db.with_snapshot(|s| s.structures[&tenebrex_id].state != "ShieldVulnerable") {
            break;
        }
    }
    let jam_reinf = rpc_call(&ctx, &mut s_dir, "beyonce", "GetCynoJammerState", &[]).await.expect("GetCynoJammerState");
    assert_eq!(jam_reinf.dict_get("isJammed"), Some(&PyRep::Bool(false)));
    assert!(rpc_call(&ctx, &mut s_dir, "dogmaIM", "Activate", &[PyRep::Integer(995_200), PyRep::String("cynosuralGeneration".into())]).await.is_ok());

    let _ = drain_quasar(&mut rx);
    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}
