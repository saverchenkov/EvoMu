//! Integration tests verifying remediation of batch-3 `FLAGGED_FAKE` `rpc:*` rows.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT_ID: i64 = 90_000_001;
const SYS_JITA: u32 = 30_000_142;
const SYS_PERIMETER: u32 = 30_000_144;
const SYS_TAMA: u32 = 30_002_813;
const SYS_TRIG_MV: u32 = 30_000_163;
const SYS_POCHVEN_KINO: u32 = 30_002_797;
const JITA_STATION: i64 = 60_003_760;
const ERR_CHALLENGE_NOT_FOUND: &str = "ChallengeNotFound";
const ERR_CHALLENGE_NOT_COMPLETED: &str = "ChallengeNotCompleted";
const ERR_CHALLENGE_ALREADY_CLAIMED: &str = "ChallengeAlreadyClaimed";

fn new_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn login_char(ctx: &ServerContext, sid: u64, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
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

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v),
        _ => None,
    }
}

fn count_nested_instances(rep: &PyRep, expected_sys: u32) -> usize {
    let PyRep::Dict(outer) = rep else {
        panic!("expected outer Dict, got {rep:?}");
    };
    let Some((_, inner)) = outer
        .iter()
        .find(|(k, _)| k.as_i64() == Some(i64::from(expected_sys)))
    else {
        panic!("missing solarSystemID {expected_sys} in {outer:?}");
    };
    let PyRep::Dict(by_dungeon) = inner else {
        panic!("expected inner Dict, got {inner:?}");
    };
    let mut total = 0;
    for (did, insts) in by_dungeon {
        assert!(did.as_i64().unwrap_or(0) > 0, "dungeon_id must be positive");
        let PyRep::List(ids) = insts else {
            panic!("expected List of instance_ids, got {insts:?}");
        };
        assert!(!ids.is_empty(), "dungeon {did:?} must have >= 1 instance");
        total += ids.len();
    }
    total
}

#[tokio::test]
async fn batch3_dungeon_instance_cache_homefront_and_triglavian() {
    // parity: rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances
    // parity: rpc:dungeonInstanceCacheMgr.GetTriglavianSiteInstances
    let ctx = new_ctx();
    let mut unauth = SessionState::default();
    assert!(rpc(
        &ctx,
        &mut unauth,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[],
    )
    .await
    .is_err());
    assert!(rpc(
        &ctx,
        &mut unauth,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[],
    )
    .await
    .is_err());

    let mut sess = login_char(&ctx, 101, PILOT_ID).await;
    sess.solar_system_id = Some(SYS_JITA);
    sess.station_id = None;

    // Invalid solarSystemID arguments must fail closed
    for bad_arg in [
        PyRep::String("not_a_system".into()),
        PyRep::Integer(-1),
        PyRep::Integer(99_999_999),
    ] {
        let err_hf = rpc(
            &ctx,
            &mut sess,
            "dungeonInstanceCacheMgr",
            "GetHomefrontSiteInstances",
            std::slice::from_ref(&bad_arg),
        )
        .await
        .expect_err("invalid solarSystemID must fail for GetHomefrontSiteInstances");
        assert_eq!(err_hf.code, "UnknownSolarSystem");

        let err_trig = rpc(
            &ctx,
            &mut sess,
            "dungeonInstanceCacheMgr",
            "GetTriglavianSiteInstances",
            &[bad_arg],
        )
        .await
        .expect_err("invalid solarSystemID must fail for GetTriglavianSiteInstances");
        assert_eq!(err_trig.code, "UnknownSolarSystem");
    }

    // High-sec systems (both Jita and dynamically seeded Perimeter) return Homefront sites
    let hf_jita = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[],
    )
    .await
    .expect("GetHomefrontSiteInstances Jita");
    assert_eq!(count_nested_instances(&hf_jita, SYS_JITA), 2);

    let hf_perim = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[PyRep::Integer(i64::from(SYS_PERIMETER))],
    )
    .await
    .expect("GetHomefrontSiteInstances Perimeter");
    assert_eq!(count_nested_instances(&hf_perim, SYS_PERIMETER), 2);

    // Low-sec system Tama has 0 Homefront sites
    let hf_tama = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[PyRep::Integer(i64::from(SYS_TAMA))],
    )
    .await
    .expect("GetHomefrontSiteInstances Tama");
    assert_eq!(count_nested_instances(&hf_tama, SYS_TAMA), 0);

    // Triglavian minor victory & Pochven systems return Triglavian sites; Jita returns 0
    let trig_mv = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(i64::from(SYS_TRIG_MV))],
    )
    .await
    .expect("GetTriglavianSiteInstances minor victory");
    assert_eq!(count_nested_instances(&trig_mv, SYS_TRIG_MV), 2);

    let trig_poch = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(i64::from(SYS_POCHVEN_KINO))],
    )
    .await
    .expect("GetTriglavianSiteInstances Pochven");
    assert_eq!(count_nested_instances(&trig_poch, SYS_POCHVEN_KINO), 2);

    let trig_jita = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[],
    )
    .await
    .expect("GetTriglavianSiteInstances Jita");
    assert_eq!(count_nested_instances(&trig_jita, SYS_JITA), 0);
}

#[tokio::test]
async fn batch3_login_campaign_and_milestones() {
    // parity: rpc:loginCampaignManager.get_client_campaign_state
    // parity: rpc:loginCampaignManager.claim_reward
    // parity: rpc:milestoneMgr.ProcessCharacterLogon
    // parity: rpc:milestoneMgr.ClaimRewards
    let ctx = new_ctx();
    let mut unauth = SessionState::default();
    assert!(rpc(
        &ctx,
        &mut unauth,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .is_err());

    // Authenticated account without character selected -> None for get_client_campaign_state
    let mut charless = SessionState {
        user_id: Some(777),
        ..Default::default()
    };
    let none_state = rpc(
        &ctx,
        &mut charless,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .expect("charless get_client_campaign_state");
    assert_eq!(none_state, PyRep::None);

    let mut sess = login_char(&ctx, 102, PILOT_ID).await;
    sess.station_id = Some(JITA_STATION);

    let state_before = rpc(
        &ctx,
        &mut sess,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .expect("get_client_campaign_state");
    assert_eq!(
        dict_get(&state_before, "can_claim_today").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        dict_get(&state_before, "total_days_claimed").and_then(|v| v.as_i64()),
        Some(0)
    );

    let claim_rep = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward");
    assert_ne!(claim_rep, PyRep::None, "first daily claim must return reward tuple");

    let state_after = rpc(
        &ctx,
        &mut sess,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await
    .expect("get_client_campaign_state after claim");
    assert_eq!(
        dict_get(&state_after, "can_claim_today").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        dict_get(&state_after, "total_days_claimed").and_then(|v| v.as_i64()),
        Some(1)
    );

    // Second claim same day returns None
    let second_claim = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("second claim_reward");
    assert_eq!(second_claim, PyRep::None);

    // milestoneMgr: ClaimRewards before ProcessCharacterLogon fails
    let prereq_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect_err("ClaimRewards before ProcessCharacterLogon must fail");
    assert_eq!(prereq_err.code, "MilestonePrerequisiteNotMet");

    let bad_id_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(0)])
        .await
        .expect_err("ClaimRewards(0) must fail");
    assert_eq!(bad_id_err.code, "InvalidMilestoneID");

    let logon_rep = rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .expect("ProcessCharacterLogon");
    assert_eq!(logon_rep, PyRep::None);

    let persisted_logon = ctx.db.with_snapshot(|snap| {
        eve_server::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap)
            .milestone_logons
            .contains_key(&PILOT_ID)
    });
    assert!(persisted_logon, "ProcessCharacterLogon must persist milestone_logons");

    let claim_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect_err("ClaimRewards fails closed on V24 FSD data gap");
    assert_eq!(claim_err.code, "UnimplementedRPC");
}

#[tokio::test]
async fn batch3_season_manager_challenges_and_queries() {
    // parity: rpc:seasonManager.get_max_active_challenges
    // parity: rpc:seasonManager.get_season_end_time
    // parity: rpc:seasonManager.fill_up_and_return_challenges_for_character
    // parity: rpc:seasonManager.get_challenge
    // parity: rpc:seasonManager.challenge_is_dormant
    let ctx = new_ctx();
    let mut sess = login_char(&ctx, 103, PILOT_ID).await;

    let max_active = rpc(&ctx, &mut sess, "seasonManager", "get_max_active_challenges", &[])
        .await
        .expect("get_max_active_challenges")
        .as_i64()
        .expect("integer max_active");
    assert!(max_active >= 1, "max_active_challenges must be >= 1");

    let end_time = rpc(&ctx, &mut sess, "seasonManager", "get_season_end_time", &[])
        .await
        .expect("get_season_end_time")
        .as_i64()
        .expect("integer end_time");
    assert!(
        end_time > ctx.now_blueos_filetime_ticks(),
        "season end_time must be in the future"
    );

    let filled = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "fill_up_and_return_challenges_for_character",
        &[],
    )
    .await
    .expect("fill_up_and_return_challenges_for_character");
    let PyRep::Dict(ch_entries) = filled else {
        panic!("expected Dict from fill_up_and_return_challenges_for_character, got {filled:?}");
    };
    assert!(!ch_entries.is_empty(), "must seed >= 1 active challenge");
    let first_cid = ch_entries[0].0.as_i64().expect("challenge_id int");

    let ch_kv = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_challenge",
        &[PyRep::Integer(first_cid)],
    )
    .await
    .expect("get_challenge");
    assert_eq!(
        dict_get(&ch_kv, "challengeID").and_then(|v| v.as_i64()),
        Some(first_cid)
    );
    assert_eq!(
        dict_get(&ch_kv, "progress").and_then(|v| v.as_i64()),
        Some(0)
    );

    let dormant = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "challenge_is_dormant",
        &[PyRep::Integer(first_cid)],
    )
    .await
    .expect("challenge_is_dormant")
    .as_bool()
    .expect("bool dormant");
    assert!(!dormant, "active challenge must not be dormant");

    // Missing / unknown challenge IDs fail closed
    let missing_dormant = rpc(&ctx, &mut sess, "seasonManager", "challenge_is_dormant", &[])
        .await
        .expect_err("missing arg");
    assert_eq!(missing_dormant.code, ERR_CHALLENGE_NOT_FOUND);

    let unknown_ch = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_challenge",
        &[PyRep::Integer(999_999_999)],
    )
    .await
    .expect_err("unknown id");
    assert_eq!(unknown_ch.code, ERR_CHALLENGE_NOT_FOUND);
}

#[tokio::test]
async fn batch3_season_manager_events_claim_reload_and_npc_standings() {
    // parity: rpc:seasonManager.get_challenge_expiration_date
    // parity: rpc:seasonManager.process_client_event
    // parity: rpc:seasonManager.claim_challenge_rewards
    // parity: rpc:seasonManager.reload_fsd_data
    // parity: rpc:standingMgr.GetNPCNPCStandings
    let ctx = new_ctx();
    let mut sess = login_char(&ctx, 104, PILOT_ID).await;
    sess.station_id = Some(JITA_STATION);

    // Clone a real FSD client-event-driven challenge so we can deterministically progress & claim it
    let mut custom_def = eve_server::services::season_manager::fsd_challenge_definitions(&ctx.sde)
        .iter()
        .find(|d| d.client_event_driven)
        .expect("real FSD client_event_driven challenge")
        .clone();
    let custom_cid: i64 = 880_001;
    custom_def.challenge_id = custom_cid;
    custom_def.max_progress = 1;
    custom_def.isk_awarded = 0;
    custom_def.event_conditions.clear();
    let ev_type = custom_def.event_type.clone();
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            eve_server::services::season_manager::SEASONAL_CHALLENGES_CONFIG_KEY.into(),
            serde_json::to_string(&vec![custom_def]).unwrap(),
        );
    });

    let filled = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "fill_up_and_return_challenges_for_character",
        &[],
    )
    .await
    .expect("fill_up");
    assert!(
        dict_get(&filled, "880001").is_some()
            || matches!(&filled, PyRep::Dict(v) if v.iter().any(|(k, _)| k.as_i64() == Some(custom_cid))),
        "custom challenge {custom_cid} must be active"
    );

    let exp_ft = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_challenge_expiration_date",
        &[PyRep::Integer(custom_cid)],
    )
    .await
    .expect("get_challenge_expiration_date")
    .as_i64()
    .expect("expiration int");
    assert!(exp_ft > ctx.now_blueos_filetime_ticks());

    let missing_exp = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_challenge_expiration_date",
        &[],
    )
    .await
    .expect_err("missing arg");
    assert_eq!(missing_exp.code, ERR_CHALLENGE_NOT_FOUND);

    // Uncompleted challenge cannot be claimed
    let uncomp_err = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_challenge_rewards",
        &[PyRep::Integer(custom_cid)],
    )
    .await
    .expect_err("uncompleted claim must fail");
    assert_eq!(uncomp_err.code, ERR_CHALLENGE_NOT_COMPLETED);

    // Progress via process_client_event
    rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "process_client_event",
        &[PyRep::String(ev_type)],
    )
    .await
    .expect("process_client_event");

    let ch_after_ev = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_challenge",
        &[PyRep::Integer(custom_cid)],
    )
    .await
    .expect("get_challenge after event");
    assert_eq!(
        dict_get(&ch_after_ev, "progress").and_then(|v| v.as_i64()),
        Some(1)
    );

    // Claim completed challenge
    let claim_res = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_challenge_rewards",
        &[PyRep::Integer(custom_cid)],
    )
    .await
    .expect("claim_challenge_rewards");
    assert_ne!(claim_res, PyRep::None);

    let dup_err = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_challenge_rewards",
        &[PyRep::Integer(custom_cid)],
    )
    .await
    .expect_err("duplicate claim must fail");
    assert_eq!(dup_err.code, ERR_CHALLENGE_ALREADY_CLAIMED);

    // reload_fsd_data requires GM role
    assert!(
        rpc(&ctx, &mut sess, "seasonManager", "reload_fsd_data", &[])
            .await
            .is_err(),
        "non-GM reload_fsd_data must fail"
    );
    sess.role |= eve_machonet::service_roles::ROLE_GMS;
    let reload_ok = rpc(&ctx, &mut sess, "seasonManager", "reload_fsd_data", &[])
        .await
        .expect("GM reload_fsd_data");
    assert_eq!(reload_ok, PyRep::Bool(true));

    // standingMgr.GetNPCNPCStandings
    let npc_st = rpc(&ctx, &mut sess, "standingMgr", "GetNPCNPCStandings", &[])
        .await
        .expect("GetNPCNPCStandings");
    let PyRep::Rowset { columns, rows } = npc_st else {
        panic!("expected Rowset from GetNPCNPCStandings, got {npc_st:?}");
    };
    assert_eq!(columns, vec!["fromID", "toID", "standing"]);
    assert!(rows.len() >= 50, "expected >= 50 inter-faction standing rows");
    assert!(
        rows.iter().any(|r| {
            r.first().and_then(|v| v.as_i64()) == Some(500_003)
                && r.get(1).and_then(|v| v.as_i64()) == Some(500_002)
                && r.get(2).and_then(|v| v.as_f64()) == Some(-5.0)
        }),
        "Amarr -> Minmatar standing (-5.0) must be present"
    );
}
