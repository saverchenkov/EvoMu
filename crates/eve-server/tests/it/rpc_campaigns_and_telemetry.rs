//! Integration test suite for login campaigns, milestones, community tracks, and telemetry RPCs:
//! - `loginCampaignManager`: `get_client_campaign_state`, `claim_reward`
//! - `seasonalLoginCampaignManager`: `get_active_campaign`, `get_claim_history`, `claim_reward`
//! - `infoGatheringMgr`: `GetStateAndConfig`, `LogInfoEventsFromClient`
//! - `milestoneMgr`: `ProcessCharacterLogon`, `ClaimRewards`
//! - `communityTrackMgr`: `get_instance_attributes_public`, `change_qa_mode`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::campaigns_and_milestones::{
    DEFAULT_LOGIN_CAMPAIGN_ID, DEFAULT_SEASONAL_CAMPAIGN_ID, GREGORIAN_EPOCH_ORDINAL,
};
use eve_db::InventoryFlag;

use super::common;

const PILOT_1: i64 = 90_000_001;

fn field<'a>(rep: &'a PyRep, key: &str) -> &'a PyRep {
    match rep {
        PyRep::KeyVal(kv) => kv
            .get(key)
            .unwrap_or_else(|| panic!("missing keyval field: {key}")),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v)
            .unwrap_or_else(|| panic!("missing dict field: {key}")),
        other => panic!("expected KeyVal or Dict, got {other:?}"),
    }
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

async fn login_pilot_char(
    ctx: &ServerContext,
    sess_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess
}

#[tokio::test]
// DATA GAP: requires live login campaign schedule
// parity: feat:pve.login-campaign
// parity: rpc:loginCampaignManager.get_client_campaign_state, rpc:loginCampaignManager.claim_reward
// parity: rpc:seasonalLoginCampaignManager.get_active_campaign, rpc:seasonalLoginCampaignManager.get_claim_history
// parity: rpc:seasonalLoginCampaignManager.claim_reward
async fn test_login_and_seasonal_campaigns() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut auth_sess = SessionState::new(31_001);
    common::login_pilot(&ctx, &mut auth_sess, "demo", "demo-secret").await;

    // Character-selection screen before character selected
    let unselected_camp = rpc(
        &ctx,
        &mut auth_sess,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .expect("unselected get_client_campaign_state");
    assert_eq!(unselected_camp, PyRep::None);

    let unselected_seasonal = rpc(
        &ctx,
        &mut auth_sess,
        "seasonalLoginCampaignManager",
        "get_active_campaign",
        &[],
    )
    .await
    .expect("unselected seasonal get_active_campaign");
    assert_eq!(
        unselected_seasonal,
        PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None, PyRep::None])
    );

    // Select character PILOT_1
    let mut sess = login_pilot_char(&ctx, 31_002, "demo", "demo-secret", PILOT_1).await;

    let (init_sp, home_station_id, init_items) = ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&PILOT_1).unwrap();
        (ch.free_skill_points, ch.home_station_id, s.items.len())
    });

    // 1. loginCampaignManager.get_client_campaign_state: returns KeyVal backed by seasonalcampaignsloginrewards.fsdbinary
    let state1 = rpc(
        &ctx,
        &mut sess,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .expect("get_client_campaign_state");
    assert_eq!(
        field(&state1, "campaignID").as_i64(),
        Some(DEFAULT_LOGIN_CAMPAIGN_ID)
    );
    assert_eq!(field(&state1, "canClaimToday").as_bool(), Some(true));
    assert_eq!(
        field(field(&state1, "static_data"), "titleMessageID").as_i64(),
        Some(709_008)
    );
    let item_progress = field(&state1, "item_progress");
    assert_eq!(field(item_progress, "next_reward_index").as_i64(), Some(1));
    assert_eq!(field(item_progress, "reward_count").as_i64(), Some(30));

    // 2. loginCampaignManager.claim_reward: returns 4-tuple and delivers SDE day 0 rewards into home station hangar
    let claim1 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward day 1");
    let PyRep::Tuple(claim1_tuple) = claim1 else {
        panic!("expected 4-tuple from claim_reward, got {claim1:?}");
    };
    assert_eq!(claim1_tuple.len(), 4);
    let reward_item = &claim1_tuple[0];
    let next_prog = &claim1_tuple[1];
    let _reward_event = &claim1_tuple[2];
    let bucket_prog = &claim1_tuple[3];

    assert_eq!(
        field(reward_item, "campaignID").as_i64(),
        Some(DEFAULT_LOGIN_CAMPAIGN_ID)
    );
    assert_eq!(field(reward_item, "reward_index").as_i64(), Some(1));
    assert_eq!(field(reward_item, "typeID").as_i64(), Some(80_937));
    assert_eq!(field(reward_item, "quantity").as_i64(), Some(1));
    let claimed_item_id1 = field(reward_item, "itemID")
        .as_i64()
        .expect("delivered alpha reward itemID");
    assert_eq!(field(next_prog, "next_reward_index").as_i64(), Some(2));
    assert_eq!(field(bucket_prog, "progress").as_i64(), Some(1));

    // PILOT_1 is Omega (clone_grade 2): both Alpha (80937) and Omega (49809) SDE rewards are delivered to home station hangar
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters.get(&PILOT_1).unwrap().free_skill_points, init_sp);
        assert_eq!(s.items.len(), init_items + 2);
        let alpha_item = s.items.get(&claimed_item_id1).expect("minted alpha reward item");
        assert_eq!(alpha_item.owner_id, PILOT_1);
        assert_eq!(alpha_item.location_id, home_station_id);
        assert_eq!(alpha_item.flag, InventoryFlag::Hangar);
        assert_eq!(alpha_item.type_id, 80_937);
        assert_eq!(alpha_item.quantity, 1);
        assert!(s.items.values().any(|it| {
            it.owner_id == PILOT_1
                && it.location_id == home_station_id
                && it.flag == InventoryFlag::Hangar
                && it.type_id == 49_809
                && it.quantity == 1
        }));
    });

    // Duplicate claim on same day returns None (nothing to claim)
    let repeat_claim = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("repeat claim returns None");
    assert_eq!(repeat_claim, PyRep::None);

    // Advance clock by 1 full day (86,400s)
    ctx.advance_virtual_clock(86_400);

    // Day 2 get_client_campaign_state reflects next_reward_index 2
    let state2 = rpc(
        &ctx,
        &mut sess,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .expect("get_client_campaign_state day 2");
    assert_eq!(field(&state2, "canClaimToday").as_bool(), Some(true));
    assert_eq!(
        field(field(&state2, "item_progress"), "next_reward_index").as_i64(),
        Some(2)
    );

    // Claim day 2: delivers SDE day 1 Alpha (82052) and Omega (29266) rewards
    let claim2 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward day 2");
    let PyRep::Tuple(claim2_tuple) = claim2 else {
        panic!("expected 4-tuple from claim_reward day 2");
    };
    assert_eq!(field(&claim2_tuple[0], "reward_index").as_i64(), Some(2));
    assert_eq!(field(&claim2_tuple[0], "typeID").as_i64(), Some(82_052));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items.len(), init_items + 4);
        assert!(s.items.values().any(|it| it.owner_id == PILOT_1 && it.type_id == 82_052));
        assert!(s.items.values().any(|it| it.owner_id == PILOT_1 && it.type_id == 29_266));
    });

    // 3. seasonalLoginCampaignManager.get_active_campaign: returns 4-tuple with Proleptic Gregorian date ordinal
    let seasonal_active = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_active_campaign",
        &[],
    )
    .await
    .expect("seasonal get_active_campaign");
    let PyRep::Tuple(seasonal_tuple) = seasonal_active else {
        panic!("expected 4-tuple from seasonal get_active_campaign");
    };
    assert_eq!(seasonal_tuple.len(), 4);
    assert_eq!(
        seasonal_tuple[0].as_i64(),
        Some(DEFAULT_SEASONAL_CAMPAIGN_ID)
    );
    let start_date_ordinal = seasonal_tuple[1].as_i64().expect("start_date_ordinal");
    assert!(
        start_date_ordinal >= GREGORIAN_EPOCH_ORDINAL,
        "start_date_ordinal must be a proleptic Gregorian ordinal (>= 719,163)"
    );
    // end_date_str is derived from end_unix 1_850_000_000 (2028-08-16 00:53:20 UTC)
    assert_eq!(seasonal_tuple[2].as_str(), Some("2028-08-16 00:53:20"));
    let days_remaining = seasonal_tuple[3].as_i64().expect("days_remaining");
    assert!(days_remaining > 0, "days remaining must be dynamic > 0");

    // 4. seasonalLoginCampaignManager.get_claim_history: initially empty Dict
    let history_pre = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_claim_history",
        &[],
    )
    .await
    .expect("seasonal get_claim_history pre");
    let PyRep::Dict(hist_entries_pre) = history_pre else {
        panic!("expected Dict from get_claim_history");
    };
    assert!(hist_entries_pre.is_empty());

    // 5. seasonalLoginCampaignManager.claim_reward: delivers SDE campaign 2323 day 0 rewards (87900 + 49809)
    let seasonal_claim = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "claim_reward",
        &[],
    )
    .await
    .expect("seasonal claim_reward");
    assert_eq!(seasonal_claim, PyRep::Bool(true));

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters.get(&PILOT_1).unwrap().free_skill_points, init_sp);
        assert_eq!(s.items.len(), init_items + 6);
        assert!(s.items.values().any(|it| {
            it.owner_id == PILOT_1
                && it.location_id == home_station_id
                && it.flag == InventoryFlag::Hangar
                && it.type_id == 87_900
        }));
    });

    // Duplicate seasonal claim on same day must fail
    let repeat_seasonal_err = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "claim_reward",
        &[],
    )
    .await
    .expect_err("duplicate seasonal claim must fail");
    // rpc-lint: allow error code check
    assert_eq!(repeat_seasonal_err.code, "SeasonalRewardAlreadyClaimedToday");

    // History now has 1 entry with Gregorian date ordinal key and delivered SDE item IDs
    let history_post = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_claim_history",
        &[],
    )
    .await
    .expect("seasonal get_claim_history post");
    let PyRep::Dict(hist_entries_post) = history_post else {
        panic!("expected Dict from get_claim_history post");
    };
    assert_eq!(hist_entries_post.len(), 1);
    let (key_rep, claim_data) = &hist_entries_post[0];
    let key_ordinal = key_rep.as_i64().expect("date ordinal key");
    assert!(
        key_ordinal >= GREGORIAN_EPOCH_ORDINAL,
        "claim_history key must be Gregorian date ordinal"
    );
    assert_eq!(
        field(claim_data, "campaign_id").as_i64(),
        Some(DEFAULT_SEASONAL_CAMPAIGN_ID)
    );
    assert_eq!(field(claim_data, "alpha_type_id").as_i64(), Some(87_900));
    assert_eq!(field(claim_data, "omega_type_id").as_i64(), Some(49_809));
}

#[tokio::test]
// parity: rpc:infoGatheringMgr.GetStateAndConfig
// parity: rpc:infoGatheringMgr.LogInfoEventsFromClient
async fn test_info_gathering_telemetry_rpcs() {
    let ctx = ServerContext::new();

    let mut unauth_sess = SessionState::new(31_003);

    // Unauthenticated call must fail closed
    let unauth_err = rpc(&ctx, &mut unauth_sess, "infoGatheringMgr", "GetStateAndConfig", &[])
        .await
        .expect_err("unauthenticated GetStateAndConfig must fail");
    // rpc-lint: allow error code check
    assert_eq!(unauth_err.code, "Unauthorized");

    let mut sess = SessionState::new(31_004);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    // 1. infoGatheringMgr.GetStateAndConfig: returns KeyVal config backed by SDE eventtypes.static
    let cfg = rpc(&ctx, &mut sess, "infoGatheringMgr", "GetStateAndConfig", &[])
        .await
        .expect("GetStateAndConfig");
    assert_eq!(field(&cfg, "clientWorkerInterval").as_i64(), Some(300));
    assert_eq!(field(&cfg, "isEnabled").as_bool(), Some(true));
    assert!(matches!(field(&cfg, "infoTypeAggregates"), PyRep::Dict(_)));
    assert!(matches!(field(&cfg, "infoTypeParameters"), PyRep::Dict(_)));
    let PyRep::List(info_types) = field(&cfg, "infoTypes") else {
        panic!("expected List for infoTypes");
    };
    assert_eq!(
        info_types.len(),
        72,
        "infoTypes must contain the 72 SDE event types from res:/staticdata/eventtypes.static"
    );
    assert!(matches!(field(&cfg, "infoTypesOncePerRun"), PyRep::List(_)));

    // State mutation via live machoNet.SetGlobalConfigValue: disabling info gathering and updating interval
    let mut gm_sess = sess.clone();
    gm_sess.role |= eve_machonet::service_roles::ROLE_GML;
    rpc(&ctx, &mut gm_sess, "machoNet", "SetGlobalConfigValue", &[PyRep::String("infoGatheringEnabled".into()), PyRep::Bool(false)]).await.expect("disable infoGathering");
    rpc(&ctx, &mut gm_sess, "machoNet", "SetGlobalConfigValue", &[PyRep::String("infoGatheringClientWorkerInterval".into()), PyRep::Integer(600)]).await.expect("set interval 600");
    let cfg_disabled = rpc(&ctx, &mut sess, "infoGatheringMgr", "GetStateAndConfig", &[])
        .await
        .expect("GetStateAndConfig disabled");
    assert_eq!(field(&cfg_disabled, "clientWorkerInterval").as_i64(), Some(600));
    assert_eq!(field(&cfg_disabled, "isEnabled").as_bool(), Some(false));
    let disabled_log = rpc(&ctx, &mut sess, "infoGatheringMgr", "LogInfoEventsFromClient", &[PyRep::List(vec![PyRep::String("dropped_evt".into())])]).await.expect("disabled log");
    assert_eq!(disabled_log, PyRep::Dict(vec![(PyRep::String("dropped_evt".into()), PyRep::Integer(1))]));
    rpc(&ctx, &mut gm_sess, "machoNet", "SetGlobalConfigValue", &[PyRep::String("infoGatheringEnabled".into()), PyRep::Bool(true)]).await.expect("enable infoGathering");
    rpc(&ctx, &mut gm_sess, "machoNet", "SetGlobalConfigValue", &[PyRep::String("infoGatheringClientWorkerInterval".into()), PyRep::Integer(300)]).await.expect("set interval 300");

    // 2. infoGatheringMgr.LogInfoEventsFromClient: requires 1 arg (dict or list)
    let missing_arg = rpc(&ctx, &mut sess, "infoGatheringMgr", "LogInfoEventsFromClient", &[])
        .await
        .expect_err("missing arg must fail");
    // rpc-lint: allow error code check
    assert_eq!(missing_arg.code, "InvalidArgument");

    let invalid_arg = rpc(
        &ctx,
        &mut sess,
        "infoGatheringMgr",
        "LogInfoEventsFromClient",
        &[PyRep::Integer(42)],
    )
    .await
    .expect_err("invalid arg type must fail");
    // rpc-lint: allow error code check
    assert_eq!(invalid_arg.code, "InvalidArgument");

    // Logging dict of events
    let events_dict = PyRep::Dict(vec![
        (PyRep::String("ui_click".into()), PyRep::Integer(1)),
        (PyRep::String("ui_window_open".into()), PyRep::Integer(2)),
    ]);
    let log_dict_rep = rpc(
        &ctx,
        &mut sess,
        "infoGatheringMgr",
        "LogInfoEventsFromClient",
        &[events_dict],
    )
    .await
    .expect("LogInfoEventsFromClient dict");
    let PyRep::Dict(status_entries) = log_dict_rep else {
        panic!("expected Dict mapping event IDs to status");
    };
    assert_eq!(status_entries.len(), 2);
    for (_, status) in &status_entries {
        assert_eq!(status, &PyRep::Integer(0));
    }

    // Verify events were stored in bounded telemetry buffer in snapshot
    ctx.db.with_snapshot(|snap| {
        let ext = eve_server::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap);
        assert!(
            ext.client_telemetry_events.len() >= 2,
            "telemetry buffer must record client events"
        );
    });

    // Logging list of events
    let events_list = PyRep::List(vec![
        PyRep::String("session_start".into()),
        PyRep::String("session_end".into()),
    ]);
    let log_list_rep = rpc(
        &ctx,
        &mut sess,
        "infoGatheringMgr",
        "LogInfoEventsFromClient",
        &[events_list],
    )
    .await
    .expect("LogInfoEventsFromClient list");
    let PyRep::Dict(list_status) = log_list_rep else {
        panic!("expected Dict from list logging");
    };
    assert_eq!(list_status.len(), 2);
}

#[tokio::test]
// parity: rpc:communityTrackMgr.get_instance_attributes_public
// parity: feat:ui.community-track
async fn test_milestones_and_community_tracks() {
    eve_testkit::attest(&[
        "rpc:communityTrackMgr.get_instance_attributes_public",
        "feat:ui.community-track",
    ]);
    let ctx = ServerContext::new();

    let mut sess = login_pilot_char(&ctx, 31_005, "demo", "demo-secret", PILOT_1).await;

    // Prerequisite check: Claiming milestone before ProcessCharacterLogon must fail
    let pre_logon_claim = rpc(
        &ctx,
        &mut sess,
        "milestoneMgr",
        "ClaimRewards",
        &[PyRep::Integer(1)],
    )
    .await
    .expect_err("claiming milestone before logon processing must fail");
    // rpc-lint: allow error code check
    assert_eq!(pre_logon_claim.code, "MilestonePrerequisiteNotMet");

    // 1. milestoneMgr.ProcessCharacterLogon: returns PyRep::None and records logon timestamp
    let logon_res = rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .expect("ProcessCharacterLogon");
    assert_eq!(logon_res, PyRep::None);

    ctx.db.with_snapshot(|snap| {
        let ext = eve_server::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap);
        assert!(
            ext.milestone_logons.contains_key(&PILOT_1),
            "logon must be recorded in milestone_logons"
        );
    });

    // 2. milestoneMgr.ClaimRewards: argument validation and DATA GAP fail-closed behavior
    let missing_milestone = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[])
        .await
        .expect_err("missing milestone arg must fail");
    // rpc-lint: allow error code check
    assert_eq!(missing_milestone.code, "InvalidArgument");

    let invalid_milestone = rpc(
        &ctx,
        &mut sess,
        "milestoneMgr",
        "ClaimRewards",
        &[PyRep::Integer(0)],
    )
    .await
    .expect_err("invalid milestone 0 must fail");
    // rpc-lint: allow error code check
    assert_eq!(invalid_milestone.code, "InvalidMilestoneID");

    // DATA GAP: milestone reward tables are server-authored and absent from client .fsdbinary data
    let claim_err = rpc(
        &ctx,
        &mut sess,
        "milestoneMgr",
        "ClaimRewards",
        &[PyRep::Integer(1)],
    )
    .await
    .expect_err("ClaimRewards must fail closed when server milestone reward data is unavailable");
    // rpc-lint: allow error code check
    assert_eq!(claim_err.code, "UnimplementedRPC");

    // 3. communityTrackMgr.get_instance_attributes_public: backed by community_track_templates.fsdbinary
    for (args, expected_code) in [
        (vec![], "InvalidArgument"),
        (vec![PyRep::Integer(-5)], "InvalidInstanceID"),
        (vec![PyRep::Integer(9999)], "CommunityTrackInstanceNotFound"),
        (vec![PyRep::Integer(1)], "CommunityTrackInstanceNotFound"),
    ] {
        let err = rpc(&ctx, &mut sess, "communityTrackMgr", "get_instance_attributes_public", &args)
            .await
            .expect_err("invalid or inactive track instance must fail");
        // rpc-lint: allow error code check
        assert_eq!(err.code, expected_code);
    }

    let encode_seq_req = |id_val: u64| -> Vec<u8> {
        let (mut req, mut id_pb) = (Vec::new(), Vec::new());
        eve_proto::quasar::encode_varint_field(1, id_val, &mut id_pb);
        eve_proto::quasar::encode_bytes_field(1, &id_pb, &mut req);
        req
    };
    let mut gm_sess = crate::quasar_progression_complete::build_sess(31_099, 90_000_002, 98_000_001);
    gm_sess.user_id = Some(2);
    gm_sess.role |= eve_machonet::service_roles::ROLE_ADMIN;
    assert_eq!(crate::quasar_progression_complete::quasar_rpc(&ctx, &mut gm_sess, "eve.communitytrack.api.StartRequest", encode_seq_req(1)).await.status_code, 200);
    // Calling StartRequest(1) a second time must be idempotent and never allocate a duplicate active instance 10_002
    assert_eq!(crate::quasar_progression_complete::quasar_rpc(&ctx, &mut gm_sess, "eve.communitytrack.api.StartRequest", encode_seq_req(1)).await.status_code, 200);
    let dup_err = rpc(&ctx, &mut sess, "communityTrackMgr", "get_instance_attributes_public", &[PyRep::Integer(10_002)]).await.expect_err("duplicate instance must not exist");
    // rpc-lint: allow error code check
    assert_eq!(dup_err.code, "CommunityTrackInstanceNotFound");
    let tid = 10_001_i64;
    assert_eq!(rpc(&ctx, &mut sess, "communityTrackMgr", "get_instance_attributes_public", &[PyRep::Integer(tid)]).await.unwrap(), PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(0)]));

    // Event scheduler starts Vanguard Nemesis tale 466 in 30_000_142; starting alone does not advance track
    let encode_tale_req = |spawn_tpl: u64, sys_id: Option<u64>| -> Vec<u8> {
        let mut req = encode_seq_req(spawn_tpl);
        if let Some(sid) = sys_id {
            let mut sys_pb = Vec::new();
            eve_proto::quasar::encode_varint_field(1, sid, &mut sys_pb);
            eve_proto::quasar::encode_bytes_field(2, &sys_pb, &mut req);
        }
        req
    };
    assert_eq!(crate::quasar_progression_complete::quasar_rpc(&ctx, &mut gm_sess, "eve.tale.api.StartTaleRequest", encode_tale_req(466, Some(30_000_142))).await.status_code, 200);
    assert_eq!(rpc(&ctx, &mut sess, "communityTrackMgr", "get_instance_attributes_public", &[PyRep::Integer(tid)]).await.unwrap(), PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(0)]));
    rpc(&ctx, &mut sess, "publicGatewaySvc", "Subscribe", &[]).await.expect("Subscribe");
    let mut stream_rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("stream");
    while stream_rx.try_recv().is_ok() {}
    ctx.quasar_gateway.progression.extra.on_system_tale_encounter_completed(30_000_142, "Data", 999_999, 1, ctx.now_unix(), &ctx.quasar_gateway);
    assert_eq!(rpc(&ctx, &mut sess, "communityTrackMgr", "get_instance_attributes_public", &[PyRep::Integer(tid)]).await.unwrap(), PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(0)]), "mismatched Data site must not advance CombatAnomaly tale 466");
    assert!(stream_rx.try_recv().is_err(), "mismatched site must not emit ProgressedNotice");

    // Non-admin pilot undocks in 30_000_142 and destroys live PvE anomaly waves via dogmaIM.Activate -> advances track (1, 0) -> (1, 1)
    assert_eq!(sess.role & eve_machonet::service_roles::ROLE_ADMIN, 0, "player session must not hold ROLE_ADMIN");
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await.expect("Undock non-admin pilot");
    let ship_id = sess.ship_id.expect("active ship after undock");
    let sb_id = 910_003_001_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(sb_id, eve_db::InventoryItemRecord { item_id: sb_id, type_id: 3993, owner_id: PILOT_1, location_id: ship_id, flag: InventoryFlag::HiSlot1, quantity: 1, singleton: true, ..Default::default() });
        s.module_online_states.insert(sb_id, true);
    });
    let active_tales = rpc(&ctx, &mut sess, "taleMgr", "get_active_tales_by_template", &[PyRep::Integer(466)]).await.expect("get_active_tales_by_template");
    assert!(matches!(&active_tales, PyRep::List(items) if items.len() == 1), "non-admin pilot sees active tale 466");
    let anoms = rpc(&ctx, &mut sess, "scanSvc", "GetCombatAnomalyInstances", &[PyRep::Integer(30_000_142)]).await.expect("GetCombatAnomalyInstances");
    let site_id = match &anoms {
        PyRep::List(items) => items.iter().find_map(|it| field(it, "siteID").as_str().map(String::from)).expect("combat anomaly in 30_000_142"),
        other => panic!("expected anomaly list, got {other:?}"),
    };
    let sys_handle = ctx.systems.get_or_spawn(30_000_142);
    for _ in 0..24 {
        let wave_bids: Vec<i64> = match rpc(&ctx, &mut sess, "scanSvc", "GetCombatAnomalyInstances", &[PyRep::Integer(30_000_142)]).await.unwrap() {
            PyRep::List(items) => items.into_iter().find(|it| field(it, "siteID").as_str() == Some(&site_id))
                .map(|it| if let PyRep::List(l) = field(&it, "activeNpcBallIDs") { l.iter().filter_map(|v| v.as_i64()).collect() } else { Vec::new() }).unwrap_or_default(),
            _ => Vec::new(),
        };
        if wave_bids.is_empty() { break; }
        if let Ok(Some((_, npc_ball))) = sys_handle.get_ball_snapshot(wave_bids[0]).await {
            sys_handle.set_ball_position(ship_id, npc_ball.position).await;
        }
        if let Ok(Some(mut cs)) = sys_handle.get_combat_state(ship_id).await {
            cs.capacitor = 10_000.0;
            let _ = sys_handle.set_combat_state(ship_id, cs).await;
        }
        ctx.advance_secs(20);
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(sb_id)]).await.expect("Smartbomb Activate");
    }
    assert_eq!(rpc(&ctx, &mut sess, "communityTrackMgr", "get_instance_attributes_public", &[PyRep::Integer(tid)]).await.unwrap(), PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1)]));
    let mut progressed_tids = Vec::new();
    while let Ok(env) = stream_rx.try_recv() {
        if let Some(p) = env.payload.as_ref().filter(|p| p.normalized_type_name() == "eve_public.communitytrack.api.ProgressedNotice") {
            let wm = eve_proto::quasar::WireMessage::decode(&p.value).unwrap();
            if let Some(id) = wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()) {
                progressed_tids.push(id);
            }
        }
    }
    assert_eq!(progressed_tids, vec![tid as u64], "live PvE clear must publish ProgressedNotice directly to subscribed gateway stream");

    // Verify clamping at SDE goal 100 and StopRequest/StartRequest lifecycle
    for delta in [43_u64, 105_u64] {
        let mut add_pb = encode_seq_req(tid as u64);
        eve_proto::quasar::encode_varint_field(2, delta, &mut add_pb);
        assert_eq!(crate::quasar_progression_complete::quasar_rpc(&ctx, &mut gm_sess, "eve.communitytrack.api.AddProgressRequest", add_pb).await.status_code, 200);
        if delta == 43 {
            assert_eq!(rpc(&ctx, &mut sess, "communityTrackMgr", "get_instance_attributes_public", &[PyRep::Integer(tid)]).await.unwrap(), PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(44)]));
        }
    }
    assert_eq!(rpc(&ctx, &mut sess, "communityTrackMgr", "get_instance_attributes_public", &[PyRep::Integer(tid)]).await.unwrap(), PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(100)]));
    let get_pub = crate::quasar_progression_complete::quasar_rpc(&ctx, &mut sess, "eve_public.communitytrack.api.GetRequest", encode_seq_req(tid as u64)).await;
    assert_eq!(get_pub.status_code, 200);
    let attr_wm = eve_proto::quasar::WireMessage::decode(eve_proto::quasar::WireMessage::decode(&get_pub.payload.unwrap().value).unwrap().fields[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(attr_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(100), "Quasar GetRequest must also report clamped 100");

    // StopRequest, StartRequest, and SetProgressRequest resolve both track_id and template_id (1)
    let tpl_id_req = encode_seq_req(1);
    assert_eq!(crate::quasar_progression_complete::quasar_rpc(&ctx, &mut gm_sess, "eve.communitytrack.api.StopRequest", tpl_id_req.clone()).await.status_code, 200);
    let stopped_err = rpc(&ctx, &mut sess, "communityTrackMgr", "get_instance_attributes_public", &[PyRep::Integer(tid)]).await.expect_err("inactive track must fail");
    // rpc-lint: allow error code check
    assert_eq!(stopped_err.code, "CommunityTrackInstanceNotFound");
    assert_eq!(crate::quasar_progression_complete::quasar_rpc(&ctx, &mut gm_sess, "eve.communitytrack.api.StartRequest", tpl_id_req.clone()).await.status_code, 200);
    let mut set_pb = tpl_id_req;
    eve_proto::quasar::encode_varint_field(2, 75, &mut set_pb);
    assert_eq!(crate::quasar_progression_complete::quasar_rpc(&ctx, &mut gm_sess, "eve.communitytrack.admin.api.SetProgressRequest", set_pb).await.status_code, 200);
    assert_eq!(rpc(&ctx, &mut sess, "communityTrackMgr", "get_instance_attributes_public", &[PyRep::Integer(tid)]).await.expect("restarted track"), PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(75)]));

    // 4. communityTrackMgr.change_qa_mode: requires QA/GM privileges
    let denied = rpc(&ctx, &mut sess, "communityTrackMgr", "change_qa_mode", &[PyRep::Bool(true)]).await.expect_err("regular player must be denied change_qa_mode");
    // rpc-lint: allow error code check
    assert_eq!(denied.code, "AccessDenied");
    sess.role |= eve_machonet::service_roles::ROLE_QA;
    let missing_qa = rpc(&ctx, &mut sess, "communityTrackMgr", "change_qa_mode", &[]).await.expect_err("missing qa_mode must fail");
    // rpc-lint: allow error code check
    assert_eq!(missing_qa.code, "InvalidArgument");
    assert_eq!(rpc(&ctx, &mut sess, "communityTrackMgr", "change_qa_mode", &[PyRep::Bool(true)]).await.expect("change_qa_mode true with QA role"), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut sess, "communityTrackMgr", "change_qa_mode", &[PyRep::Bool(false)]).await.expect("change_qa_mode false"), PyRep::Bool(true));
}
