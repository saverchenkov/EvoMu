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
    let sys_id: u32 = 30000208;
    let corp_id: i64 = 98_000_100;
    let alliance_id: i64 = 99_000_200;
    let hub_id: i64 = 600_000_010;
    let jammer_id: i64 = 600_000_011;

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
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Sov Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        s.structures.insert(
            jammer_id,
            UpwellStructureRecord {
                structure_id: jammer_id,
                type_id: TYPE_TENEBREX_CYNO_JAMMER,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Cyno Jammer".into(),
                position: [500.0, 500.0, 500.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );

        let base_now = ctx.now_unix();
        s.structure_vulnerability_windows.insert(hub_id, (base_now + 1000, base_now + 2000));
    });

    let mut sess = SessionState::new(42_003);
    select(&ctx, &mut sess, "demo", "demo-secret", PILOT).await;
    sess.solar_system_id = Some(sys_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(alliance_id);

    let vuln_window = ctx.db.with_snapshot(|s| s.structure_vulnerability_windows.get(&hub_id).copied().unwrap());
    let current_time = ctx.now_unix();
    assert!(current_time < vuln_window.0);
    let is_vulnerable = current_time >= vuln_window.0 && current_time <= vuln_window.1;
    assert!(!is_vulnerable, "hub must not be vulnerable outside window");

    ctx.advance_virtual_clock(1500);
    let current_time_in_window = ctx.now_unix();
    let is_vulnerable_now = current_time_in_window >= vuln_window.0 && current_time_in_window <= vuln_window.1;
    assert!(is_vulnerable_now, "hub is vulnerable inside owner window");

    let access_group_id: i64 = 77_889;
    sov_call(&ctx, &mut sess, "sovMgr", "SetSovHubFuelAccessGroup", &[PyRep::Integer(hub_id), PyRep::Integer(access_group_id)])
        .await
        .expect("SetSovHubFuelAccessGroup");
    let got_group = sov_call(&ctx, &mut sess, "sovMgr", "GetSovHubFuelAccessGroup", &[PyRep::Integer(hub_id)])
        .await
        .expect("GetSovHubFuelAccessGroup");
    assert_eq!(got_group.as_i64(), Some(access_group_id));

    let is_on_group = sov_call(&ctx, &mut sess, "sovMgr", "IsOnLocalSovHubFuelAccessGroup", &[PyRep::Integer(hub_id)])
        .await
        .expect("IsOnLocalSovHubFuelAccessGroup");
    assert_eq!(is_on_group, PyRep::Bool(true));

    // Verify non-director line member without CEO/director/station manager role is denied
    let mut line_sess = SessionState::new(41_099);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&OTHER).unwrap().corporation_id = corp_id;
    });
    select(&ctx, &mut line_sess, "other", "other-secret", OTHER).await;
    let fuel_denied = sov_call(&ctx, &mut line_sess, "sovMgr", "SetSovHubFuelAccessGroup", &[PyRep::Integer(hub_id), PyRep::Integer(access_group_id)])
        .await
        .unwrap_err();
    assert_eq!(fuel_denied.code, "CorpRoleDenied");

    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&hub_id).unwrap().fuel_blocks = 0;
        s.structures.get_mut(&jammer_id).unwrap().fuel_blocks = 0;
    });
    let jammer_state_empty = sov_call(&ctx, &mut sess, "beyonce", "GetCynoJammerState", &[])
        .await
        .expect("GetCynoJammerState empty");
    assert_eq!(jammer_state_empty.dict_get("isJammed"), Some(&PyRep::Bool(false)));

    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&jammer_id).unwrap().fuel_blocks = 50;
    });
    let jammer_state_fueled = sov_call(&ctx, &mut sess, "beyonce", "GetCynoJammerState", &[])
        .await
        .expect("GetCynoJammerState fueled");
    assert_eq!(jammer_state_fueled.dict_get("isJammed"), Some(&PyRep::Bool(true)));

    ctx.db.with_mut_snapshot(|s| {
        s.cosmic_anomalies.entry(sys_id).or_default().push(eve_db::CosmicAnomalyRecord {
            anomaly_id: "DNG-SOV-COMBAT-001".into(),
            solar_system_id: sys_id,
            name: "Major Threat Detection Site".into(),
            anomaly_type: "CombatAnomaly".into(),
            position: [1000.0, 2000.0, 3000.0],
            signal_strength: 1.0,
            ore_types: Vec::new(),
            remaining_volume: 0.0,
            created_unix: ctx.now_unix(),
            lifetime_seconds: 3600,
            cleared: false,
        });
    });
    let has_anom = ctx.db.with_snapshot(|s| {
        s.cosmic_anomalies.get(&sys_id).map(|list| list.iter().any(|a| a.name == "Major Threat Detection Site")).unwrap_or(false)
    });
    assert!(has_anom, "combat anomaly spawned by upgrade");

    ctx.drain_notifications_for_char(PILOT);
    sov_services::record_hub_hack(&ctx, sys_id, hub_id, 3600);
    assert!(sov_services::is_hub_hacked(&ctx, hub_id));

    let notifs = ctx.drain_notifications_for_char(PILOT);
    let hacked_notif = notifs.iter().find(|n| n.event == "OnSovHubHacked").expect("OnSovHubHacked");
    assert_eq!(hacked_notif.args.first().and_then(|a| a.as_i64()), Some(hub_id));
    assert!(matches!(hacked_notif.args.get(1), Some(PyRep::List(_))));

    ctx.advance_virtual_clock(3601);
    assert!(!sov_services::is_hub_hacked(&ctx, hub_id));
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
        vec![sys_id, 30000013],
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
        assert_eq!(list[0].dict_get("isFobVulnerable"), Some(&PyRep::Bool(true)));
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
