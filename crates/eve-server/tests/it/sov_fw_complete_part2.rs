//! Behavioural parity tests for  /  rows (Part 2).

use crate::common;

use eve_db::{CorporationRecord, IskCents, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::fw_services::{self, HQ_SYSTEM_AMO, HQ_SYSTEM_INTAKI, HQ_SYSTEM_MEHATOOR, HQ_SYSTEM_ONNAMON};
use eve_server::services::insurgency_services;
use eve_server::services::sov_services::{self, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB, TYPE_TENEBREX_CYNO_JAMMER};
use std::collections::{HashMap, HashSet};

const PILOT: i64 = 90_000_001;
const OTHER: i64 = 90_000_002;
const CALDARI: i64 = 500_001;

async fn select(ctx: &ServerContext, sess: &mut SessionState, user: &str, pass: &str, cid: i64) {
    common::login_pilot(ctx, sess, user, pass).await;
    ctx.dispatch_rpc(sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(cid)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
}

fn make_system_dynamic_state(sys_id: u32) -> eve_db::SystemDynamicState {
    eve_db::SystemDynamicState {
        solar_system_id: sys_id,
        dbs_multiplier: 1.0,
        ess_main_bank: eve_db::IskCents(0),
        ess_reserve_bank: eve_db::IskCents(0),
        ess_linked_character_id: None,
        ess_link_started_tick: None,
        sov_alliance_id: None,
        military_index: 0,
        industrial_index: 0,
        strategic_index: 0,
        fw_contested_pct: 0.0,
        insurgency_corruption_stage: 1,
        insurgency_suppression_stage: 0,
        wormhole_remaining_mass_kg: 0.0,
        wormhole_destination_system_id: None,
        signature_position: None,
        fw_complex_position: None,
        fw_capture_seconds_elapsed: 0,
        fw_capture_duration_seconds: 0,
        last_fw_capture_tick: 0,
        last_sov_claim_tick: 0,
    }
}

/// `(solarsystemID, occupationState)` as unpacked by `FwWarzoneSvc.GetLocalOccupationState`.
fn local_occupation_state(rep: &PyRep, sys_id: u32) -> &PyRep {
    let PyRep::Tuple(t) = rep else { panic!("GetLocalOccupationState must return a 2-tuple, got {rep:?}") };
    assert_eq!(t.len(), 2);
    assert_eq!(t[0], PyRep::Integer(i64::from(sys_id)));
    &t[1]
}

async fn sov_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

#[tokio::test]
async fn sov_hub_vulnerability_fuel_jammer_sites_and_hacking() {
    // parity: feat:sovereignty.sov-cyno-jammer-upgrade
    eve_testkit::attest(&[
        "feat:sovereignty.sov-cyno-jammer-upgrade",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (sys_id, _, _, _, _) = crate::sovereignty_equinox_gameplay_closure::find_nullsec_planets(&ctx);
    let corp_id: i64 = 98_000_100;
    let alliance_id: i64 = 99_000_200;
    let ship_id: i64 = 100_090_001;
    let hub_pos = glam::DVec3::new(45_000_000.0, 45_000_000.0, 45_000_000.0);

    let mut sess = crate::sovereignty_equinox_gameplay_closure::register_online_pilot(
        &ctx, 42_003, "demo", "demo-secret", PILOT, corp_id, Some(alliance_id),
        sys_id, ship_id, 11_957, hub_pos, eve_db::corp_roles::ROLE_DIRECTOR,
    ).await;

    ctx.db.with_mut_snapshot(|s| {
        let sk = s.skills.entry(PILOT).or_default();
        for sid in [21_603u32, 3_335, 12_092, 22_761] {
            sk.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        for (iid, tid, flg, qty) in [
            (990_010i64, TYPE_SOVEREIGNTY_HUB, eve_db::InventoryFlag::Cargo, 1),
            (990_011, TYPE_TENEBREX_CYNO_JAMMER, eve_db::InventoryFlag::Cargo, 1),
            (990_012, 4_247, eve_db::InventoryFlag::Cargo, 500),
            (990_001, sov_services::TYPE_MAGMATIC_GAS, eve_db::InventoryFlag::Cargo, 5_000),
            (990_002, sov_services::TYPE_SUPERIONIC_ICE, eve_db::InventoryFlag::Cargo, 5_000),
            (990_003, eve_server::services::sov_gameplay::UPGRADE_CYNOSURAL_SUPPRESSION, eve_db::InventoryFlag::Cargo, 1),
            (990_004, eve_server::services::sov_gameplay::UPGRADE_MAJOR_THREAT_DETECTION, eve_db::InventoryFlag::Cargo, 1),
            (990_005, 88_227, eve_db::InventoryFlag::Cargo, 1),
            (990_006, 88_229, eve_db::InventoryFlag::Cargo, 1),
            (990_200, 21_096, eve_db::InventoryFlag::HiSlot0, 1),
            (990_201, 16_273, eve_db::InventoryFlag::Cargo, 2_000),
        ] {
            s.items.insert(
                iid,
                eve_db::InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: PILOT,
                    location_id: ship_id,
                    flag: flg,
                    quantity: qty,
                    singleton: qty == 1,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        s.module_online_states.insert(990_200, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    // 1. Anchor Sovereignty Hub via live `structureDeployment.Anchor` and complete onlining + sovereignty claim!
    let anchor_hub = sov_call(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(990_010), PyRep::String("Sov Hub".into())])
        .await
        .expect("anchor Sov Hub via structureDeployment.Anchor");
    let hub_id = anchor_hub.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_virtual_clock(905);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&sys_id].sov_alliance_id), Some(alliance_id));

    // Advance 36 days so Strategic Index reaches 3 naturally via `tick_background_cluster` + configure vulnerability window via client RPC!
    ctx.advance_virtual_clock(36 * 86_400);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(ctx.db.with_snapshot(|s| s.system_states[&sys_id].strategic_index) >= 3);
    sov_call(
        &ctx,
        &mut sess,
        "structureVulnerability",
        "UpdateReinforceTimesFromClient",
        &[PyRep::Integer(hub_id), PyRep::Integer(18), PyRep::Integer(3)],
    )
    .await
    .expect("UpdateReinforceTimesFromClient");

    let access_group_id: i64 = 77_889;
    sov_call(&ctx, &mut sess, "sovMgr", "SetSovHubFuelAccessGroup", &[PyRep::Integer(hub_id), PyRep::Integer(access_group_id)])
        .await
        .expect("SetSovHubFuelAccessGroup");
    let got_group = sov_call(&ctx, &mut sess, "sovMgr", "GetSovHubFuelAccessGroup", &[PyRep::Integer(hub_id)])
        .await
        .expect("GetSovHubFuelAccessGroup");
    assert_eq!(got_group.as_i64(), Some(access_group_id));
    assert_eq!(
        sov_call(&ctx, &mut sess, "sovMgr", "IsOnLocalSovHubFuelAccessGroup", &[PyRep::Integer(hub_id)]).await.unwrap(),
        PyRep::Bool(true)
    );

    let mut line_sess = SessionState::new(41_099);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&OTHER).unwrap().corporation_id = corp_id;
    });
    select(&ctx, &mut line_sess, "other", "other-secret", OTHER).await;
    assert_eq!(
        sov_call(&ctx, &mut line_sess, "sovMgr", "SetSovHubFuelAccessGroup", &[PyRep::Integer(hub_id), PyRep::Integer(access_group_id)])
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    let jammer_state_no_upgrade = sov_call(&ctx, &mut sess, "beyonce", "GetCynoJammerState", &[])
        .await
        .expect("GetCynoJammerState before Cynosural Suppression upgrade online");
    assert_eq!(jammer_state_no_upgrade.dict_get("isJammed"), Some(&PyRep::Bool(false)));

    // 2. Fuel Sov Hub (`AddRequest`) and install + online `UPGRADE_CYNOSURAL_SUPPRESSION` + `UPGRADE_MAJOR_THREAT_DETECTION`!
    for (fuel_iid, amt) in [(990_001u64, 5_000u64), (990_002u64, 5_000u64)] {
        let mut add = crate::quasar_sov_equinox_hub::id_req(1, fuel_iid);
        eve_proto::quasar::encode_varint_field(2, amt, &mut add);
        eve_proto::quasar::encode_bytes_field(3, &crate::quasar_sov_equinox_hub::idm(hub_id as u64), &mut add);
        assert_eq!(
            crate::quasar_sov_equinox_hub::rpc(&ctx, &mut sess, "eve_public.sovereignty.hub.fuel.api.AddRequest", add)
                .await
                .status_code,
            eve_server::quasar_gateway::STATUS_OK
        );
    }
    let mut proc = Vec::new();
    for iid in [990_005u64, 990_006, 990_003, 990_004] {
        eve_proto::quasar::encode_bytes_field(1, &crate::quasar_sov_equinox_hub::idm(iid), &mut proc);
    }
    for tid in [
        88_227u64,
        88_229,
        eve_server::services::sov_gameplay::UPGRADE_CYNOSURAL_SUPPRESSION as u64,
        eve_server::services::sov_gameplay::UPGRADE_MAJOR_THREAT_DETECTION as u64,
    ] {
        let mut cfg = Vec::new();
        eve_proto::quasar::encode_bytes_field(1, &crate::quasar_sov_equinox_hub::idm(tid), &mut cfg);
        eve_proto::quasar::encode_varint_field(2, 1, &mut cfg);
        eve_proto::quasar::encode_bytes_field(2, &cfg, &mut proc);
    }
    eve_proto::quasar::encode_bytes_field(3, &crate::quasar_sov_equinox_hub::idm(hub_id as u64), &mut proc);
    assert_eq!(
        crate::quasar_sov_equinox_hub::rpc(&ctx, &mut sess, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc)
            .await
            .status_code,
        eve_server::quasar_gateway::STATUS_OK
    );

    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // 3. Anchor Tenebrex Cyno Jammer (`37534`) via `structureDeployment.Anchor` & load fuel blocks via `invbroker.Add`!
    let jam_pos = hub_pos + glam::DVec3::new(2_000_000.0, 0.0, 0.0);
    let _ = ctx.systems.get_or_spawn(sys_id).spawn_ball(crate::sovereignty_equinox_gameplay_closure::make_ball(ship_id, 11_957, jam_pos)).await;
    let anchor_jam = sov_call(&ctx, &mut sess, "structureDeployment", "Anchor", &[PyRep::Integer(990_011), PyRep::String("Cyno Jammer".into())])
        .await
        .expect("anchor Tenebrex");
    let jammer_id = anchor_jam.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    sov_call(
        &ctx,
        &mut sess,
        "invbroker",
        "Add",
        &[PyRep::Integer(990_012), PyRep::Integer(jammer_id), PyRep::Integer(133), PyRep::Integer(500)],
    )
    .await
    .expect("fuel Tenebrex via invbroker.Add");
    ctx.advance_virtual_clock(15 * 60 + 24 * 3_600 + 15 * 60 + 10);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);
    ctx.drain_notifications_for_char(PILOT);
    ctx.advance_virtual_clock(eve_server::services::sov_gameplay::TENEBREX_SPOOL_UP_SECS + 5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(ctx.drain_notifications_for_char(PILOT).iter().any(|n| n.event == "OnCynoJammerChanged"));

    let jammer_state_fueled = sov_call(&ctx, &mut sess, "beyonce", "GetCynoJammerState", &[])
        .await
        .expect("GetCynoJammerState with Cynosural Suppression upgrade + Tenebrex online");
    assert_eq!(jammer_state_fueled.dict_get("isJammed"), Some(&PyRep::Bool(true)));
    assert_eq!(
        sov_call(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(990_200), PyRep::String("cynosuralGeneration".into())])
            .await
            .unwrap_err()
            .code,
        "CynoJammed"
    );

    let has_anom = ctx.db.with_snapshot(|s| {
        s.cosmic_anomalies
            .get(&sys_id)
            .is_some_and(|list| list.iter().any(|a| a.name.contains("Major Threat Detection")))
    });
    assert!(has_anom, "combat anomaly spawned by online Major Threat Detection upgrade");
    let sys_balls = ctx.systems.get_or_spawn(sys_id).list_balls().await.unwrap();
    assert!(sys_balls.iter().any(|b| b.type_id == 37_453));

    // 4. Hack Sov Hub via live `Data Analyzer I` (`22175`) + `hackingMgr.ClickedOnTile` -> `OnSovHubHacked`, jam disabled, cyno allowed!
    let hacker_cid = 90_000_088i64;
    let hacker_ship = 100_090_088i64;
    let mut s_hacker = crate::sovereignty_equinox_gameplay_closure::register_online_pilot(
        &ctx, 42_088, "other", "other-secret", hacker_cid, 98_000_188, Some(99_000_288),
        sys_id, hacker_ship, 621, hub_pos + glam::DVec3::new(2_500.0, 0.0, 0.0), 0,
    ).await;
    ctx.db.with_mut_snapshot(|s| {
        let sk = s.skills.entry(hacker_cid).or_default();
        for sid in [3_405u32, 21_718] {
            sk.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        s.items.insert(
            990_300,
            eve_db::InventoryItemRecord {
                item_id: 990_300,
                type_id: 22_175,
                owner_id: hacker_cid,
                location_id: hacker_ship,
                flag: eve_db::InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.module_online_states.insert(990_300, true);
    });
    ctx.invalidate_ship_fitting(hacker_ship);
    crate::sovereignty_equinox_gameplay_closure::lock_target_via_rpc(&ctx, &mut s_hacker, sys_id, hacker_ship, hub_id).await;
    ctx.drain_notifications_for_char(PILOT);
    sov_call(&ctx, &mut s_hacker, "dogmaIM", "Activate", &[PyRep::Integer(990_300), PyRep::String("dataHacking".into()), PyRep::Integer(hub_id)])
        .await
        .expect("activate Data Analyzer I");
    sov_call(&ctx, &mut s_hacker, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(hub_id)])
        .await
        .expect("StartNewGameInstance");
    let mut won = false;
    for _pass in 0..8 {
        for tile in 1..24 {
            for _ in 0..5 {
                if let Ok(step) = sov_call(&ctx, &mut s_hacker, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(hub_id), PyRep::Integer(tile)]).await {
                    if step.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
                        won = true;
                        break;
                    }
                } else {
                    break;
                }
            }
            if won { break; }
        }
        if won { break; }
    }
    assert!(won, "live hacking minigame must complete with Won");
    assert!(sov_services::is_hub_hacked(&ctx, hub_id));
    let notifs = ctx.drain_notifications_for_char(PILOT);
    let hacked_notif = notifs.iter().find(|n| n.event == "OnSovHubHacked").expect("OnSovHubHacked");
    assert_eq!(hacked_notif.args.first().and_then(|a| a.as_i64()), Some(hub_id));

    let jammer_state_hacked = sov_call(&ctx, &mut sess, "beyonce", "GetCynoJammerState", &[])
        .await
        .expect("GetCynoJammerState after hub hack");
    assert_eq!(jammer_state_hacked.dict_get("isJammed"), Some(&PyRep::Bool(false)));
    assert!(sov_call(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(990_200), PyRep::String("cynosuralGeneration".into())]).await.is_ok());

    ctx.advance_virtual_clock(3_605);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!sov_services::is_hub_hacked(&ctx, hub_id));
    let jammer_state_restored = sov_call(&ctx, &mut sess, "beyonce", "GetCynoJammerState", &[]).await.unwrap();
    assert_eq!(jammer_state_restored.dict_get("isJammed"), Some(&PyRep::Bool(true)));
}

#[tokio::test]
async fn skyhook_acquisition_theft_and_reagent_loss() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let sys_id: u32 = 30000208;
    let corp_id: i64 = 98_000_100;
    let alliance_id: i64 = 99_000_200;
    let skyhook_id: i64 = 600_000_020;

    ctx.db.with_mut_snapshot(|s| {
        s.alliances.insert(
            alliance_id,
            eve_db::AllianceRecord {
                alliance_id,
                name: "Test Sov Alliance".into(),
                short_name: "TSA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
        s.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                alliance_id: Some(alliance_id),
                name: "Test Sov Corp".into(),
                ticker: "TSC".into(),
                ceo_id: PILOT,
                members: HashSet::from([PILOT]),
                master_wallet: IskCents::from_isk(0),
                ..Default::default()
            },
        );
        s.characters.get_mut(&PILOT).unwrap().corporation_id = corp_id;
        s.characters.get_mut(&PILOT).unwrap().alliance_id = Some(alliance_id);
        s.characters.get_mut(&PILOT).unwrap().solar_system_id = sys_id;

        let st = s.system_states.entry(sys_id).or_insert_with(|| make_system_dynamic_state(sys_id));
        st.sov_alliance_id = Some(alliance_id);

        s.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Planet I Skyhook".into(),
                position: [1000.0, 1000.0, 1000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 0,
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
    });
    sov_services::set_skyhook_reagents(&ctx, skyhook_id, sov_services::TYPE_MAGMATIC_GAS, 100);

    let mut sess = SessionState::new(42_004);
    select(&ctx, &mut sess, "demo", "demo-secret", PILOT).await;
    sess.solar_system_id = Some(sys_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(alliance_id);

    let skyhooks = sov_call(&ctx, &mut sess, "colonyResourcesMgr", "GetMyCorporationSkyhooks", &[])
        .await
        .expect("GetMyCorporationSkyhooks");
    match skyhooks {
        PyRep::List(list) => {
            assert_eq!(list.len(), 1);
            let sk = &list[0];
            assert_eq!(sk.dict_get("skyhookID").and_then(|v| v.as_i64()), Some(skyhook_id));
            assert_eq!(sk.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(sys_id as i64));
            assert_eq!(sk.dict_get("reagentTypeID").and_then(|v| v.as_i64()), Some(sov_services::TYPE_MAGMATIC_GAS as i64));
            assert!(sk.dict_get("reagentQuantity").and_then(|v| v.as_i64()).unwrap() > 0);
        }
        _other => panic!("expected List, got {_other:?}"),
    }

    ctx.drain_notifications_for_char(PILOT);
    sov_services::set_skyhook_hackable(&ctx, sys_id, skyhook_id, true);

    let notifs = ctx.drain_notifications_for_char(PILOT);
    let hack_notif = notifs.iter().find(|n| n.event == "OnSkyhookCanBeHacked").expect("OnSkyhookCanBeHacked");
    assert_eq!(hack_notif.args.first().and_then(|a| a.as_i64()), Some(sys_id as i64));
    assert_eq!(hack_notif.args.get(1).and_then(|a| a.as_i64()), Some(skyhook_id));

    // Verify non-director line member without CEO/director/station manager role is denied
    let mut line_sess = SessionState::new(41_099);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&OTHER).unwrap().corporation_id = corp_id;
    });
    select(&ctx, &mut line_sess, "other", "other-secret", OTHER).await;
    let destroy_denied = sov_call(&ctx, &mut line_sess, "sovMgr", "DestroySkyhooks", &[PyRep::Integer(sys_id as i64)])
        .await
        .unwrap_err();
    assert_eq!(destroy_denied.code, "CorpRoleDenied");

    let destroyed = sov_call(&ctx, &mut sess, "sovMgr", "DestroySkyhooks", &[PyRep::Integer(sys_id as i64)])
        .await
        .expect("DestroySkyhooks");
    assert_eq!(destroyed.as_i64(), Some(1));

    assert!(ctx.db.with_snapshot(|s| s.structures.get(&skyhook_id).cloned()).is_none());
}

#[tokio::test]
async fn fw_warzones_victory_points_and_advantage_scaling() {
    let ctx = ServerContext::new();
    let sys_id: u32 = 30000012; // Asabona (0.32 low-sec FW system)

    let mut sess = SessionState::new(42_005);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT).unwrap().solar_system_id = sys_id;
        s.system_states.entry(sys_id).or_insert_with(|| make_system_dynamic_state(sys_id));
        s.fw_system_occupations.insert(sys_id, eve_db::snapshot_ext::FwSystemOccupationRecord { solar_system_id: sys_id, owner_faction_id: fw_services::FACTION_AMARR, occupier_faction_id: fw_services::FACTION_AMARR, victory_points: 0, victory_points_threshold: 3000 });
    });
    select(&ctx, &mut sess, "demo", "demo-secret", PILOT).await;
    sess.solar_system_id = Some(sys_id);

    let hqs = sov_call(&ctx, &mut sess, "fwWarzoneSolarsystem", "GetHQSystemIDs", &[])
        .await
        .expect("GetHQSystemIDs");
    match hqs {
        PyRep::List(list) => {
            assert_eq!(list.len(), 4);
            let ids: HashSet<i64> = list.iter().filter_map(|v| v.as_i64()).collect();
            assert!(ids.contains(&(HQ_SYSTEM_MEHATOOR as i64)));
            assert!(ids.contains(&(HQ_SYSTEM_ONNAMON as i64)));
            assert!(ids.contains(&(HQ_SYSTEM_INTAKI as i64)));
            assert!(ids.contains(&(HQ_SYSTEM_AMO as i64)));
        }
        _other => panic!("expected List, got {_other:?}"),
    }

    let warzones = sov_call(&ctx, &mut sess, "fwWarzoneSolarsystem", "GetAllWarzones", &[])
        .await
        .expect("GetAllWarzones");
    match warzones {
        PyRep::List(list) => {
            assert!(list.len() >= 2);
            assert_eq!(list[0].dict_get("warzoneID").and_then(|v| v.as_i64()), Some(1));
        }
        _other => panic!("expected warzone list"),
    }

    let (vp, contested) = fw_services::add_victory_points(&ctx, sys_id, 1500);
    assert_eq!(vp, 1500);
    assert!((contested - 50.0).abs() < 0.1);

    let vp_state = sov_call(&ctx, &mut sess, "fwVictoryPointMgr", "GetLocalVictoryPointState", &[])
        .await
        .expect("GetLocalVictoryPointState");
    assert_eq!(vp_state.dict_get("victoryPoints").and_then(|v| v.as_i64()), Some(1500));
    assert_eq!(vp_state.dict_get("threshold").and_then(|v| v.as_i64()), Some(3000));
    let pct = vp_state.dict_get("contestedPercentage").and_then(|v| v.as_f64()).unwrap();
    assert!((pct - 50.0).abs() < 0.1);

    let occ_rep = sov_call(&ctx, &mut sess, "fwWarzoneSolarsystem", "GetLocalOccupationState", &[])
        .await
        .expect("GetLocalOccupationState");
    let occ = local_occupation_state(&occ_rep, sys_id);
    assert_eq!(occ.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(sys_id as i64));
    assert_eq!(occ.dict_get("frontlineStatus").and_then(|v| v.as_i64()), Some(fw_services::STATUS_REARGUARD));

    let (_, cont75) = fw_services::add_victory_points(&ctx, sys_id, 1000);
    assert!(cont75 > 75.0);

    // Adjacent system 30000013 occupied by enemy Minmatar transitions Asabona to Frontline
    ctx.db.with_mut_snapshot(|s| {
        s.fw_system_occupations.insert(30000013, eve_db::snapshot_ext::FwSystemOccupationRecord {
            solar_system_id: 30000013,
            owner_faction_id: fw_services::FACTION_MINMATAR,
            occupier_faction_id: fw_services::FACTION_MINMATAR,
            victory_points: 0,
            victory_points_threshold: 3000,
        });
    });
    let occ_front_rep = sov_call(&ctx, &mut sess, "fwWarzoneSolarsystem", "GetLocalOccupationState", &[])
        .await
        .expect("GetLocalOccupationState Frontline");
    let occ_front = local_occupation_state(&occ_front_rep, sys_id);
    assert_eq!(occ_front.dict_get("frontlineStatus").and_then(|v| v.as_i64()), Some(fw_services::STATUS_FRONTLINE));

    ctx.drain_notifications_for_char(PILOT);
    fw_services::update_system_advantage(&ctx, sys_id, 40.0, 10.0);

    let mult = fw_services::calculate_capture_speed_multiplier(40.0, 10.0);
    assert!((mult - 1.15).abs() < 0.001);
}

#[tokio::test]
async fn pirate_insurgency_campaigns_corruption_suppression_and_instances() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let sys_id: u32 = 30000012; // Asabona
    let campaign_id: i64 = 880_001;

    let mut sess = SessionState::new(42_006);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT).unwrap().solar_system_id = sys_id;
        s.system_states.entry(sys_id).or_insert_with(|| make_system_dynamic_state(sys_id));
        s.fw_system_occupations.insert(sys_id, eve_db::snapshot_ext::FwSystemOccupationRecord { solar_system_id: sys_id, owner_faction_id: fw_services::FACTION_AMARR, occupier_faction_id: fw_services::FACTION_AMARR, victory_points: 0, victory_points_threshold: 3000 });
    });
    select(&ctx, &mut sess, "demo", "demo-secret", PILOT).await;
    sess.solar_system_id = Some(sys_id);

    ctx.drain_notifications_for_char(PILOT);
    insurgency_services::init_insurgency_campaign(
        &ctx,
        campaign_id,
        insurgency_services::FACTION_GURISTAS,
        CALDARI,
        sys_id,
        vec![sys_id, 30000013, 30000014, 30000015, 30000016],
    );

    let notifs = ctx.drain_notifications_for_char(PILOT);
    assert!(notifs.iter().any(|n| n.event == "OnInsurgencyCampaignStartedForLocation"));

    let camps = sov_call(&ctx, &mut sess, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
        .await
        .expect("GetAllVisibleCampaigns");
    match camps {
        PyRep::List(list) => {
            assert_eq!(list.len(), 1);
            let c = &list[0];
            assert_eq!(c.dict_get("campaignID").and_then(|v| v.as_i64()), Some(campaign_id));
            assert_eq!(c.dict_get("phase").and_then(|v| v.as_str()), Some("Forecasting"));
            assert_eq!(c.dict_get("isFobVulnerable"), Some(&PyRep::Bool(false)));
        }
        _other => panic!("expected List, got {_other:?}"),
    }

    ctx.advance_virtual_clock(insurgency_services::FORECASTING_DURATION_SECS + 1);

    let camps_active = sov_call(&ctx, &mut sess, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
        .await
        .expect("GetAllVisibleCampaigns Active");
    if let PyRep::List(list) = camps_active {
        assert_eq!(list[0].dict_get("phase").and_then(|v| v.as_str()), Some("Active"));
        assert_eq!(list[0].dict_get("isFobVulnerable"), Some(&PyRep::Bool(false)));
    }

    ctx.drain_notifications_for_char(PILOT);
    let (c_pts, c_stage) = insurgency_services::add_system_corruption(&ctx, campaign_id, sys_id, 250);
    assert_eq!(c_pts, 250);
    assert_eq!(c_stage, 2);

    let (s_pts, s_stage) = insurgency_services::add_system_suppression(&ctx, campaign_id, sys_id, 450);
    assert_eq!(s_pts, 450);
    assert_eq!(s_stage, 3);

    let notifs2 = ctx.drain_notifications_for_char(PILOT);
    assert!(notifs2.iter().any(|n| n.event == "OnInsurgencyCampaignUpdatedForLocation"));

    let snap_tuple = sov_call(&ctx, &mut sess, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[])
        .await
        .expect("GetLocalCampaignClientSnapshot");
    let PyRep::Tuple(snap_items) = snap_tuple else {
        panic!("expected 2-tuple (solarsystemID, campaignSnapshot), got {snap_tuple:?}");
    };
    assert_eq!(snap_items.len(), 2);
    assert_eq!(snap_items[0].as_i64(), Some(i64::from(sys_id)));
    let snap = &snap_items[1];
    assert_eq!(snap.dict_get("campaignID").and_then(|v| v.as_i64()), Some(campaign_id));
    assert_eq!(snap.dict_get("corruptionStage").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(snap.dict_get("suppressionStage").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(snap.dict_get("corruptionPoints").and_then(|v| v.as_i64()), Some(250));
    assert_eq!(snap.dict_get("suppressionPoints").and_then(|v| v.as_i64()), Some(450));

    for s in [sys_id, 30000013, 30000014, 30000015, 30000016] {
        let _ = insurgency_services::add_system_suppression(&ctx, campaign_id, s, 1_000);
    }
    let camps_vuln = sov_call(&ctx, &mut sess, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[])
        .await
        .expect("GetAllVisibleCampaigns FOB Vulnerable");
    if let PyRep::List(list) = camps_vuln {
        assert_eq!(list[0].dict_get("isFobVulnerable"), Some(&PyRep::Bool(true)));
    }
    let PyRep::Tuple(local_vuln) = sov_call(&ctx, &mut sess, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[]).await.unwrap() else {
        panic!("expected tuple");
    };
    assert_eq!(local_vuln[1].dict_get("isFobVulnerable"), Some(&PyRep::Bool(true)));
    ctx.db.with_mut_snapshot(|s| {
        s.insurgency_runtime.empire_ambition_modifier.insert(insurgency_services::FACTION_GURISTAS, 1);
    });
    let PyRep::Tuple(local_amb) = sov_call(&ctx, &mut sess, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[]).await.unwrap() else {
        panic!("expected tuple");
    };
    assert_eq!(local_amb[1].dict_get("isFobVulnerable"), Some(&PyRep::Bool(false)));

    let ice_id = insurgency_services::spawn_ice_heist(&ctx, sys_id);
    assert!(ice_id > 0);
    let ice_instances = sov_call(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", &[])
        .await
        .expect("GetPirateInsurgencyIceHeistInstances");
    if let PyRep::List(list) = ice_instances {
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].dict_get("instanceID").and_then(|v| v.as_i64()), Some(ice_id));
    }
}

#[tokio::test]
async fn test_fw_allied_militia_kill_awards_zero_lp() {
    let ctx = ServerContext::new();
    let sys_id = 30002813u32;
    ctx.db.with_mut_snapshot(|s| {
        s.fw_system_occupations.insert(sys_id, eve_db::FwSystemOccupationRecord {
            solar_system_id: sys_id, owner_faction_id: 500001, occupier_faction_id: 500001, victory_points: 100, victory_points_threshold: 3000,
        });
        s.characters.get_mut(&PILOT).unwrap().war_faction_id = Some(500001);
        s.characters.get_mut(&OTHER).unwrap().war_faction_id = Some(500003);
    });

    let lp_init: i64 = ctx.db.with_snapshot(|s| s.characters[&PILOT].loyalty_points.values().copied().sum());
    // 1. Caldari kills Amarr (Allies: State Protectorate & 24th Imperial Crusade) -> 0 LP
    eve_server::services::fw_gameplay::on_player_ship_destroyed_in_combat(&ctx, sys_id, PILOT, OTHER, 601);
    let lp_ally: i64 = ctx.db.with_snapshot(|s| s.characters[&PILOT].loyalty_points.values().copied().sum());
    assert_eq!(lp_ally, lp_init, "Allied militia kill must award 0 LP");

    // 2. Caldari kills Gallente (Opposing militia: Federal Defence Union) -> Awards LP
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&OTHER).unwrap().war_faction_id = Some(500004);
    });
    eve_server::services::fw_gameplay::on_player_ship_destroyed_in_combat(&ctx, sys_id, PILOT, OTHER, 601);
    let lp_enemy: i64 = ctx.db.with_snapshot(|s| s.characters[&PILOT].loyalty_points.values().copied().sum());
    assert!(lp_enemy > lp_init, "Opposing militia kill must award LP");
}
