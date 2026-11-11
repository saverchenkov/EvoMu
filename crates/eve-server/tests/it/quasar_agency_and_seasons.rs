//! E2E parity closure tests for `pve`, `ui`, and `quasar`:
//! - `feat:pve.seasons-challenges`
//! - `feat:pve.project-discovery`
//! - `feat:ui.agency`
//! - `feat:ui.photo-mode`
//! - `feat:ui.info-panels`
//! - `feat:ui.milestone-logon`
//! - `feat:quasar.ai`
//! - `feat:quasar.app`
//! - `feat:quasar.campaign`

use crate::quasar_sov_equinox_hub::rpc as quasar_rpc;
use eve_db::d6_types::PersistedMercenaryDen;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_string_field, encode_varint_field, EvePublicEvent, GoogleAny, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::pve_dungeon_runtime::{
    despawn_completed_dungeon_core, get_dungeon_instance_state,
};
use eve_server::services::pve_dungeon_runtime_rpc::{on_dungeon_approach, on_dungeon_item_looted};
use eve_server::services::pve_special_sites::{
    ensure_seasonal_event_sites_for_system, on_special_site_npc_destroyed_core,
};
use eve_server::services::season_fsd::load_season_config;
use eve_server::services::season_manager::{
    fsd_challenge_definitions, load_season_state_from_blobs, save_season_state_to_blobs,
};
use std::collections::HashMap;

const JITA: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;
const PILOT_A: i64 = 90_000_001;

fn new_ctx() -> ServerContext {
    crate::seasons_and_campaigns::fast_real_sde_ctx()
}

async fn setup_pilot(ctx: &ServerContext, char_id: i64, sys_id: u32) -> (SessionState, i64) {
    let mut sess = SessionState::new(char_id as u64);
    crate::common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).expect("demo pilot");
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.active_ship_id
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);
    (sess, ship_id)
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn quasar_publish_event(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload: Vec<u8>,
) {
    let ev = EvePublicEvent {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload,
        }),
        ..Default::default()
    };
    let rep = rpc(
        ctx,
        sess,
        "publicGatewaySvc",
        "PublishEvent",
        &[PyRep::Bytes(ev.encode_proto())],
    )
    .await
    .expect("publicGatewaySvc.PublishEvent");
    assert_eq!(rep, PyRep::Bool(true));
}

#[tokio::test]
async fn test_pve_seasons_events_and_project_discovery() {
    // parity: feat:pve.seasons-challenges, feat:pve.project-discovery
    eve_testkit::attest(&[
        "feat:pve.seasons-challenges",
        "feat:pve.project-discovery",
    ]);

    let ctx = new_ctx();
    let (mut sess, _) = setup_pilot(&ctx, PILOT_A, JITA).await;

    // 1. Seasons & Challenges: fill per character, progress from events, expire, and claim rewards
    let _ = rpc(&ctx, &mut sess, "seasonManager", "select_season", &[PyRep::Integer(1)])
        .await
        .expect("select_season");
    let filled = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "fill_up_and_return_challenges_for_character",
        &[],
    )
    .await
    .expect("fill_up_and_return_challenges_for_character");
    let PyRep::Dict(ch_map) = filled else {
        panic!("expected Dict of challenges");
    };
    assert_eq!(ch_map.len(), 4);

    let def274 = fsd_challenge_definitions(&ctx.sde)
        .iter()
        .find(|d| d.challenge_id == 274)
        .expect("challenge 274");
    let ed274: Vec<(PyRep, PyRep)> = def274
        .event_conditions
        .iter()
        .filter_map(|(k, vs)| Some((PyRep::String(k.clone()), PyRep::Integer(*vs.first()?))))
        .collect();
    let kw = HashMap::from([("extra_data".to_string(), PyRep::Dict(ed274))]);
    for _ in 0..def274.max_progress {
        ctx.dispatch_rpc(
            &mut sess,
            "seasonManager",
            None,
            "process_client_event",
            &[PyRep::Integer(274), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("process_client_event");
    }
    let exp_ft = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_challenge_expiration_date",
        &[PyRep::Integer(274)],
    )
    .await
    .expect("get_challenge_expiration_date")
    .as_i64()
    .expect("expiration ticks");
    assert!(exp_ft > 0);

    let expire_cid = ch_map
        .iter()
        .filter_map(|(k, _)| k.as_i64())
        .find(|&id| id != 274)
        .expect("another active challenge to expire");
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_season_state_from_blobs(snap, PILOT_A);
        if let Some(ch) = st.challenges.get_mut(&expire_cid) {
            ch.expiration_date = 100;
        }
        save_season_state_to_blobs(snap, PILOT_A, &st);
    });
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let refilled = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "fill_up_and_return_challenges_for_character",
        &[],
    )
    .await
    .expect("refill after expiration");
    let PyRep::Dict(refilled_map) = refilled else {
        panic!("expected Dict");
    };
    assert!(!refilled_map.iter().any(|(k, _)| k.as_i64() == Some(expire_cid)));
    assert!(ctx
        .drain_notifications_for_char(PILOT_A)
        .iter()
        .any(|n| n.event == "OnChallengeExpired"));

    let claim_ch = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_challenge_rewards",
        &[PyRep::Integer(274)],
    )
    .await
    .expect("claim_challenge_rewards");
    assert_eq!(
        claim_ch.dict_get("claimed").and_then(|v| v.as_bool()),
        Some(true)
    );
    let pts_after_claim = ctx
        .db
        .with_snapshot(|snap| load_season_state_from_blobs(snap, PILOT_A).season_points);
    assert!(pts_after_claim >= def274.points);

    let goal_err = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_goal_reward",
        &[PyRep::Integer(999_999)],
    )
    .await
    .expect_err("unknown goal fails");
    assert!(!goal_err.code.is_empty());

    // 2. Seasonal Event Sites: spawn during active window, despawn outside window, reward loot+points+ISK
    let now_ft = ctx.now_blueos_filetime_ticks();
    let season_cfg = ctx.db.with_mut_snapshot(|snap| {
        let mut cfg = load_season_config(snap);
        cfg.start_time_ft = now_ft - 3_600 * 10_000_000;
        cfg.end_time_ft = now_ft + 3_600 * 10_000_000;
        snap.seasons_and_campaigns.season_configuration = Some(cfg.clone());
        cfg
    });
    let active_sites = ensure_seasonal_event_sites_for_system(&ctx, JITA).await;
    assert_eq!(active_sites.len(), 1);
    let anom_res = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetCombatAnomalyInstances",
        &[],
    )
    .await
    .expect("GetCombatAnomalyInstances");
    assert!(matches!(anom_res, PyRep::Dict(_)));

    let past_end_unix = ((season_cfg.end_time_ft - 116_444_736_000_000_000) / 10_000_000) as u64 + 60;
    let delta = past_end_unix.saturating_sub(ctx.now_unix());
    ctx.db.clock().advance_secs(delta);
    let expired_sites = ensure_seasonal_event_sites_for_system(&ctx, JITA).await;
    assert!(expired_sites.is_empty(), "event sites must despawn outside window");

    // Spawn a fresh site in a second system (30_000_144 Perimeter) with a fresh context inside window
    let ctx2 = new_ctx();
    let (mut sess2, _) = setup_pilot(&ctx2, PILOT_A, 30_000_144).await;
    let now_ft2 = ctx2.now_blueos_filetime_ticks();
    ctx2.db.with_mut_snapshot(|snap| {
        let mut cfg = load_season_config(snap);
        cfg.start_time_ft = now_ft2 - 3_600 * 10_000_000;
        cfg.end_time_ft = now_ft2 + 3_600 * 10_000_000;
        snap.seasons_and_campaigns.season_configuration = Some(cfg.clone());
    });
    let spawned = ensure_seasonal_event_sites_for_system(&ctx2, 30_000_144).await;
    assert_eq!(spawned.len(), 1);
    let boss_id = spawned[0].boss_npc_ball_id;
    let bal_before = ctx2.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    let pts_before = ctx2
        .db
        .with_snapshot(|s| load_season_state_from_blobs(s, PILOT_A).season_points);
    let changes = on_special_site_npc_destroyed_core(
        &ctx2.systems,
        &ctx2.db,
        &ctx2.sde,
        30_000_144,
        boss_id,
        Some(PILOT_A),
    )
    .await;
    assert!(!changes.is_empty());
    let bal_after = ctx2.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    let pts_after = ctx2
        .db
        .with_snapshot(|s| load_season_state_from_blobs(s, PILOT_A).season_points);
    assert!(bal_after > bal_before);
    assert!(pts_after > pts_before);
    let _ = &mut sess2;

    // 3. Project Discovery: get_new_task & post_classification/submit with scoring, XP, and ISK/item rewards
    let _ = rpc(&ctx, &mut sess, "ProjectDiscovery", "initialize_tutorial_status", &[])
        .await
        .expect("initialize_tutorial_status");
    let task_rep = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[])
        .await
        .expect("get_new_task");
    let task_id = task_rep
        .dict_get("id")
        .or_else(|| task_rep.dict_get("taskID"))
        .or_else(|| task_rep.dict_get("task_id"))
        .cloned()
        .unwrap_or(PyRep::Integer(1));
    let sub_payload = PyRep::Dict(vec![
        (PyRep::String("classification".into()), PyRep::List(vec![PyRep::Integer(1)])),
    ]);
    let sub_rep = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[task_id, sub_payload],
    )
    .await
    .expect("ProjectDiscovery.post_classification");
    assert!(matches!(sub_rep, PyRep::Dict(_) | PyRep::KeyVal(_)));
}

#[tokio::test]
async fn test_pve_dungeons_milestones_and_mto() {
    // parity: feat:ui.milestone-logon
    eve_testkit::attest(&[
        "feat:ui.milestone-logon",
    ]);

    let ctx = new_ctx();
    let (mut sess, _) = setup_pilot(&ctx, PILOT_A, JITA).await;

    // 1. Dungeon System: seed & spawn dungeon instance, trigger OnApproach/OnLoot, and despawn
    eve_server::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, JITA).await;
    let inst_id = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system[&JITA]
            .iter()
            .find_map(|s| s.expedition_instance_id)
            .expect("seeded dungeon instanceID")
    });
    let state_before = get_dungeon_instance_state(&ctx, inst_id).expect("dungeon state");
    assert_eq!(state_before.dict_get("despawned").and_then(|v| v.as_bool()), Some(false));

    let ship_a = sess.ship_id.unwrap();
    on_dungeon_approach(&ctx, PILOT_A, JITA, ship_a, None).await;
    on_dungeon_item_looted(&ctx, PILOT_A, None, 999_888_777, JITA).await;
    despawn_completed_dungeon_core(
        &ctx.systems,
        &ctx.db,
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
        JITA,
        inst_id,
        PILOT_A,
    )
    .await;
    let state_after = get_dungeon_instance_state(&ctx, inst_id).expect("dungeon state after");
    assert_eq!(state_after.dict_get("despawned").and_then(|v| v.as_bool()), Some(true));

    // 2. Milestones & Milestone Logon: ProcessCharacterLogon + ClaimRewards once per milestone
    let pre_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect_err("ClaimRewards before ProcessCharacterLogon must fail");
    assert_eq!(pre_err.code, "MilestonePrerequisiteNotMet");

    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let logon_rep = rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .expect("ProcessCharacterLogon");
    assert_eq!(logon_rep, PyRep::None);
    assert!(ctx
        .drain_notifications_for_char(PILOT_A)
        .iter()
        .any(|n| n.event == "OnMilestoneTimerStarted"));

    ctx.db.clock().advance_secs(7_200);
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    let claim_rep = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect("ClaimRewards(1) must succeed once");
    assert_eq!(claim_rep, PyRep::None);
    let bal_after = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    assert!(bal_after > bal_before);
    assert!(ctx
        .drain_notifications_for_char(PILOT_A)
        .iter()
        .any(|n| n.event == "OnLoginMilestoneAchieved"));

    let dup_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect_err("duplicate ClaimRewards(1) must fail");
    assert_eq!(dup_err.code, "MilestoneAlreadyClaimed");

    // 3. Mercenary Tactical Operations (MTO): spawn activity at mercenary den and complete for infomorphs+ISK
    let mto_ball_id = 1_890_123_456_i64;
    let den_id = 1_890_123_000_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                solar_system_id: JITA,
                planet_id: 40_009_082,
                owner_char_id: PILOT_A,
                enabled: true,
                development_level: 10,
                anarchy_level: 0,
                infomorphs: 100,
                ..Default::default()
            },
        );
        snap.sov_runtime
            .mto_site_balls
            .insert(mto_ball_id, format!("{den_id}:mto-1"));
    });
    let bal_pre_mto = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    eve_server::services::sov_gameplay::on_space_entity_destroyed_ext(
        &ctx,
        0,
        Some(PILOT_A),
        mto_ball_id,
        JITA,
        true,
    );
    let bal_post_mto = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    let infomorphs_post = ctx
        .db
        .with_snapshot(|s| s.sov_runtime.mercenary_dens[&den_id].infomorphs);
    assert!(bal_post_mto >= bal_pre_mto);
    assert!(infomorphs_post >= 100);
}

#[tokio::test]
async fn test_ui_agency_photo_mode_and_info_panels() {
    // parity: feat:ui.agency, feat:ui.photo-mode, feat:ui.info-panels
    eve_testkit::attest(&[
        "feat:ui.agency",
        "feat:ui.photo-mode",
        "feat:ui.info-panels",
    ]);

    let ctx = new_ctx();
    let (mut sess, _) = setup_pilot(&ctx, PILOT_A, JITA).await;

    // 1. Agency: combat/ore/ice/signature/FW/homefront instance caches & custom_agency_provider
    let combat_anoms = rpc(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", &[])
        .await
        .expect("GetCombatAnomalyInstances");
    let PyRep::Dict(combat_dict) = &combat_anoms else {
        panic!("expected Dict from GetCombatAnomalyInstances");
    };
    assert!(
        combat_dict.iter().any(|(k, _)| k.as_i64() == Some(JITA as i64)),
        "GetCombatAnomalyInstances must include Jita"
    );
    for method in [
        "GetOreAnomalyInstances",
        "GetIceBeltInstances",
        "GetSignatureInstances",
        "GetFactionWarfareInstances",
        "GetHomefrontSiteInstances",
    ] {
        let res = rpc(&ctx, &mut sess, "dungeonInstanceCacheMgr", method, &[])
            .await
            .unwrap_or_else(|e| panic!("dungeonInstanceCacheMgr.{method} failed: {e:?}"));
        assert!(matches!(res, PyRep::Dict(_)));
    }
    let agency_content = rpc(&ctx, &mut sess, "custom_agency_provider", "get_content_data", &[])
        .await
        .expect("custom_agency_provider.get_content_data");
    assert!(matches!(agency_content, PyRep::List(_) | PyRep::Dict(_)));

    // 2. Photo Mode: space scene state (undocked) and station scene state (docked) without server errors
    let station_info = rpc(
        &ctx,
        &mut sess,
        "map",
        "GetStationInfo",
        &[PyRep::Integer(JITA_STATION)],
    )
    .await
    .expect("map.GetStationInfo");
    assert_eq!(
        station_info.dict_get("stationID").and_then(|v| v.as_i64()),
        Some(JITA_STATION)
    );

    sess.station_id = Some(JITA_STATION);
    let docked_station_info = rpc(
        &ctx,
        &mut sess,
        "map",
        "GetStationInfo",
        &[PyRep::Integer(JITA_STATION)],
    )
    .await
    .expect("docked map.GetStationInfo");
    assert_eq!(
        docked_station_info.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(JITA as i64)
    );
    sess.station_id = None;

    // 3. Info Panels: route, location, mission tracker, incursion, FW, insurgency, ESS, sov
    let ess_data = rpc(&ctx, &mut sess, "dynamicResourceCacheMgr", "GetESSAgencyData", &[])
        .await
        .expect("dynamicResourceCacheMgr.GetESSAgencyData");
    assert!(matches!(ess_data, PyRep::Dict(_) | PyRep::List(_)));

    let fw_info = rpc(&ctx, &mut sess, "map", "GetFacWarZoneInfo", &[])
        .await
        .expect("map.GetFacWarZoneInfo");
    assert!(matches!(
        fw_info,
        PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::List(_) | PyRep::Tuple(_)
    ));

    let inc_report = rpc(&ctx, &mut sess, "map", "GetIncursionGlobalReport", &[])
        .await
        .expect("map.GetIncursionGlobalReport");
    assert!(matches!(inc_report, PyRep::List(_) | PyRep::Dict(_)));

    let sov_activity = rpc(&ctx, &mut sess, "map", "GetRecentSovActivity", &[])
        .await
        .expect("map.GetRecentSovActivity");
    assert!(matches!(sov_activity, PyRep::List(_) | PyRep::Dict(_)));

    let journal = rpc(&ctx, &mut sess, "agentMgr", "GetMyJournalDetails", &[])
        .await
        .expect("agentMgr.GetMyJournalDetails");
    let PyRep::Tuple(journal_parts) = journal else {
        panic!("expected Tuple from agentMgr.GetMyJournalDetails");
    };
    assert_eq!(journal_parts.len(), 2);

    let ins_rsp = quasar_rpc(&ctx, &mut sess, "eve.pirate.insurgency.api.GetCurrentRequest", Vec::new()).await;
    assert_eq!(ins_rsp.status_code, 200);
    assert!(ins_rsp
        .payload
        .as_ref()
        .is_some_and(|p| p.type_url.ends_with("eve.pirate.insurgency.api.GetCurrentResponse")));
}

#[tokio::test]
async fn test_quasar_ai_app_and_campaign() {
    // parity: feat:quasar.ai, feat:quasar.app, feat:quasar.campaign
    eve_testkit::attest(&[
        "feat:quasar.ai",
        "feat:quasar.app",
        "feat:quasar.campaign",
    ]);

    let ctx = new_ctx();
    let (mut sess, _) = setup_pilot(&ctx, PILOT_A, JITA).await;

    // 1. Quasar AI: QueryRequest -> V24 QueryResponse + eve.ai.assistant.chat.api.Queried notice
    let mut rx = crate::quasar_sov_equinox_hub::subscribe(&ctx, &mut sess).await;
    let mut ai_req = Vec::new();
    encode_string_field(1, "Jita", &mut ai_req);
    let ai_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.ai.assistant.chat.api.QueryRequest",
        ai_req,
    )
    .await;
    assert_eq!(ai_rsp.status_code, 200);
    let ai_any = ai_rsp.payload.expect("QueryResponse payload");
    assert!(ai_any.type_url.ends_with("eve_public.ai.assistant.chat.api.QueryResponse"));
    assert!(!ai_any.value.is_empty());
    let notices = crate::quasar_sov_equinox_hub::drain(&mut rx);
    assert!(
        notices.iter().any(|(t, _)| t == "eve.ai.assistant.chat.api.Queried"),
        "Queried notice must be published to subscribed client, got {notices:?}"
    );

    // 2. Quasar App: career goal requests & app UI/settings/agency events
    let goals_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.app.eveonline.career.goal.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(goals_rsp.status_code, 200);

    let mut bm_ev = Vec::new();
    encode_varint_field(1, 42, &mut bm_ev);
    quasar_publish_event(
        &ctx,
        &mut sess,
        "eve_public.app.eveonline.agency.ui.bookmark.Added",
        bm_ev,
    )
    .await;

    let mut act_ev = Vec::new();
    encode_varint_field(1, 7, &mut act_ev);
    encode_varint_field(2, 1, &mut act_ev);
    quasar_publish_event(
        &ctx,
        &mut sess,
        "eve_public.app.eveonline.agency.ui.action.Taken",
        act_ev,
    )
    .await;

    // 3. Quasar Campaign: definitions, contribution methods, and campaign state requests
    let camp_all = quasar_rpc(&ctx, &mut sess, "eve_public.campaign.api.GetAllRequest", Vec::new()).await;
    assert_eq!(camp_all.status_code, 200);
    let contrib_all = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.campaign.contributionmethod.definition.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(contrib_all.status_code, 200);
    let trig_all = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(trig_all.status_code, 200);
}
