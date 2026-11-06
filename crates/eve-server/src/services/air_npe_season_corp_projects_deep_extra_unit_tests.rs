#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::social_corp_fleet::social_corp_projects::{
    close_or_delete_canonical_project, corp_goal_uuid, create_canonical_project, freelance_uuid,
    freelance_uuid_to_project_id, goal_uuid_to_project_id, handle_edit_project,
    record_server_project_activity, redeem_all_corp_goal_rewards_for_char,
    redeem_corp_goal_reward_for_char, redeem_freelance_reward_for_char,
    sweep_expired_corp_projects_in_snap, CanonicalProjectSpec,
};
use crate::services::{air_npe_nodegraph_runner as air, season_manager};
use eve_db::{
    corp_roles, IskCents, MiningLedgerRecord, VirtualServerClock, WorldRepository,
};
use eve_machonet::service_roles::ROLE_GML;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx(now_unix: u64) -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

fn ensure_pilot(ctx: &ServerContext, char_id: i64, corp_id: i64, corp_role: u64) {
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.corporations.contains_key(&corp_id) {
            if let Some(mut corp) = snap.corporations.values().next().cloned() {
                corp.corporation_id = corp_id;
                corp.master_wallet = IskCents::from_isk(500_000_000);
                corp.ensure_wallet_divisions();
                snap.corporations.insert(corp_id, corp);
            }
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            if corp_role != 0 {
                corp.member_roles.insert(char_id, corp_role);
            }
        }
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = Some(60_003_760);
        ch.structure_id = None;
        ch.solar_system_id = 30_000_142;
        ch.balance = IskCents::from_isk(10_000_000);
        ch.free_skill_points = 0.0;
        snap.characters.insert(char_id, ch);
    });
}

fn make_session(char_id: i64, corp_id: i64, role: u64, corp_role: u64) -> SessionState {
    SessionState {
        session_id: char_id as u64,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        corp_role,
        role,
        station_id: Some(60_003_760),
        solar_system_id: Some(30_000_142),
        ..Default::default()
    }
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        PyRep::KeyVal(map) => map.get(key),
        _ => None,
    }
}

fn call_season(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kw: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    season_manager::handle_season_manager(ctx, sess, "seasonManager", method, args, kw)
        .expect("unhandled seasonManager route")
}

#[tokio::test]
async fn air_npe_nodegraph_lifecycle_events_messages_complete_and_skip() {
    let ctx = test_ctx(1_800_000_000);
    let c1 = 93_000_001_i64;
    let c2 = 93_000_002_i64;
    ensure_pilot(&ctx, c1, 98_000_901, 0);
    ensure_pilot(&ctx, c2, 98_000_901, 0);

    // Graph ID classification
    assert!(air::is_air_npe_graph_id(49) && air::is_air_npe_graph_id(725));
    assert!(!air::is_air_npe_graph_id(999_999));

    let mut sess1 = make_session(c1, 98_000_901, 0, 0);
    air::on_start_tutorial(&ctx, &mut sess1, c1, 1, Some(3));

    ctx.db.with_snapshot(|snap| {
        let st = air::load_runner_state(snap, c1).expect("runner state initialized");
        assert_eq!(st.state_id, 1);
        assert_eq!(air::query_blackboard_value(snap, c1, 716, "act"), Some(PyRep::Integer(1)));
        let npe = snap.npe_progress.get(&c1).unwrap();
        assert_eq!(npe.node_graph_id, 716);
        assert!(!npe.completed);
    });

    // Non-NPE graph message ignored; active NPE graph message handled
    assert!(!air::on_client_graph_message(&ctx, &mut sess1, c1, 888_888, 12, "hello", &PyRep::None));
    assert!(air::on_client_graph_message(&ctx, &mut sess1, c1, 716, 12, "custom_ping", &PyRep::Integer(7)));

    // Client NPE cinematic finish transitions into pocket, then gameplay event & completion
    assert_eq!(
        air::on_client_npe_event(&ctx, &mut sess1, c1, "cinematic_finished", None),
        Some(PyRep::Bool(true))
    );
    air::on_server_gameplay_event(&ctx, c1, "target_locked", &[]);
    air::complete_air_npe(&ctx, Some(&mut sess1), c1, 1_800_000_100);
    assert!(air::on_client_npe_event(&ctx, &mut sess1, c1, "complete_air_npe", None).is_none());

    // Skip AIR NPE marks skipped & completed
    let mut sess2 = make_session(c2, 98_000_901, 0, 0);
    air::on_start_tutorial(&ctx, &mut sess2, c2, 1, None);
    let (_, _, skipped_completed) = air::on_skip_air_npe(&ctx, Some(&mut sess2), c2, 1_800_000_200).unwrap();
    assert!(skipped_completed);
    assert!(ctx.db.with_snapshot(|s| air::load_runner_state(s, c2).is_some_and(|r| r.skipped)));
}

#[test]
fn season_manager_selection_events_rewards_and_goal_payouts() {
    let ctx = test_ctx(1_800_000_000);
    let char_id = 93_000_101_i64;
    ensure_pilot(&ctx, char_id, 98_000_901, 0);
    let mut sess = make_session(char_id, 98_000_901, 0, 0);
    let kw = HashMap::new();

    assert!(call_season(&ctx, &mut sess, "get_max_active_challenges", &[], &kw).unwrap().as_i64().unwrap() >= 1);
    assert!(call_season(&ctx, &mut sess, "get_season_end_time", &[], &kw).unwrap().as_i64().is_some());

    // select_season validation & season switch
    assert_eq!(
        call_season(&ctx, &mut sess, "select_season", &[PyRep::String("bad".into())], &kw).unwrap_err().code,
        "InvalidSeasonId"
    );
    assert_eq!(
        call_season(&ctx, &mut sess, "select_season", &[PyRep::Integer(999_999)], &kw).unwrap_err().code,
        "InvalidSeasonId"
    );
    assert!(call_season(&ctx, &mut sess, "select_season", &[PyRep::Integer(2)], &kw).is_ok());
    assert!(call_season(&ctx, &mut sess, "get_season_data_for_character", &[], &kw).is_ok());
    assert!(call_season(&ctx, &mut sess, "fill_up_and_return_challenges_for_character", &[], &kw).is_ok());

    // Seed custom active & dormant challenges to verify progress + claim_challenge_rewards
    let ch_active = 900_001_i64;
    let ch_dormant = 900_002_i64;
    let now_ft = ctx.now_blueos_filetime_ticks();
    let end_ft = now_ft + 10_000_000_000_000;
    let mut cust_ch = season_manager::default_challenge(ch_active, 2, now_ft);
    cust_ch.client_event_driven = true;
    cust_ch.max_progress = 1;
    cust_ch.points = 25;
    cust_ch.expiration_date = end_ft;
    let mut dorm_ch = season_manager::default_challenge(ch_dormant, 2, now_ft);
    dorm_ch.is_dormant = true;
    dorm_ch.expiration_date = end_ft;
    season_manager::with_season_state(&ctx, char_id, |st, _| {
        st.challenges.insert(ch_active, cust_ch);
        st.challenges.insert(ch_dormant, dorm_ch);
        Ok(())
    })
    .unwrap();

    assert_eq!(
        call_season(&ctx, &mut sess, "challenge_is_dormant", &[PyRep::Integer(ch_dormant)], &kw).unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_season(&ctx, &mut sess, "get_challenge_expiration_date", &[PyRep::Integer(ch_active)], &kw).unwrap(),
        PyRep::Integer(end_ft)
    );

    // Invalid amount rejected; claiming incomplete challenge rejected
    assert_eq!(
        call_season(&ctx, &mut sess, "process_client_event", &[PyRep::Integer(ch_active), PyRep::Integer(0)], &kw)
            .unwrap_err()
            .code,
        "InvalidEventProgress"
    );
    assert_eq!(
        call_season(&ctx, &mut sess, "claim_challenge_rewards", &[PyRep::Integer(ch_active)], &kw)
            .unwrap_err()
            .code,
        "ChallengeNotCompleted"
    );
    assert_eq!(
        call_season(&ctx, &mut sess, "claim_challenge_rewards", &[PyRep::Integer(ch_dormant)], &kw)
            .unwrap_err()
            .code,
        "ChallengeDormant"
    );

    // Complete active challenge via process_client_event, verify dormant while unclaimed, then claim & guard double-claim
    assert!(call_season(&ctx, &mut sess, "process_client_event", &[PyRep::Integer(ch_active)], &kw).is_ok());
    assert_eq!(
        call_season(&ctx, &mut sess, "challenge_is_dormant", &[PyRep::Integer(ch_active)], &kw).unwrap(),
        PyRep::Bool(true)
    );
    assert!(call_season(&ctx, &mut sess, "claim_challenge_rewards", &[PyRep::Integer(ch_active)], &kw).is_ok());
    assert_eq!(
        call_season(&ctx, &mut sess, "claim_challenge_rewards", &[PyRep::Integer(ch_active)], &kw)
            .unwrap_err()
            .code,
        "ChallengeAlreadyClaimed"
    );

    // Career goal reward: negative ID error, incomplete error, mining milestone completion + SP payout, double-claim error
    assert_eq!(
        call_season(&ctx, &mut sess, "claim_goal_reward", &[PyRep::Integer(-5)], &kw).unwrap_err().code,
        "InvalidGoalId"
    );
    let milestones = crate::services::quasar_career_goals::default_career_milestones();
    let mine_ms = milestones.iter().find(|m| m.career_id == Some(2)).unwrap();
    let req_qty = (((mine_ms.required_points as u64) * 2) / 3).max(1);
    assert_eq!(
        call_season(&ctx, &mut sess, "claim_goal_reward", &[PyRep::Integer(mine_ms.milestone_id as i64)], &kw)
            .unwrap_err()
            .code,
        "GoalProgressIncomplete"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1230,
            quantity: req_qty as i64,
            date: "2026-11-06".into(),
            timestamp_unix: 1_800_000_000,
        });
    });
    let goal_res = call_season(&ctx, &mut sess, "claim_goal_reward", &[PyRep::Integer(mine_ms.milestone_id as i64)], &kw).unwrap();
    assert!(dict_get(&goal_res, "freeSkillPoints").and_then(|v| v.as_f64()).unwrap() > 0.0);
    assert_eq!(
        call_season(&ctx, &mut sess, "claim_goal_reward", &[PyRep::Integer(mine_ms.milestone_id as i64)], &kw)
            .unwrap_err()
            .code,
        "GoalAlreadyClaimed"
    );

    // reload_fsd_data role check
    assert!(call_season(&ctx, &mut sess, "reload_fsd_data", &[], &kw).is_err());
    sess.role = ROLE_GML;
    assert!(call_season(&ctx, &mut sess, "reload_fsd_data", &[], &kw).is_ok());
}

#[test]
fn corp_and_freelance_projects_lifecycle_redemption_and_expiry_refunds() {
    let now = 1_800_000_000_u64;
    let ctx = test_ctx(now);
    let corp_id = 98_000_555_i64;
    let director_id = 93_000_551_i64;
    let worker_id = 93_000_552_i64;
    ensure_pilot(&ctx, director_id, corp_id, corp_roles::ROLE_DIRECTOR);
    ensure_pilot(&ctx, worker_id, corp_id, 0);

    // UUID round-trip & malformed length rejection
    let g_uuid = corp_goal_uuid(4242);
    assert_eq!(goal_uuid_to_project_id(&g_uuid), Some(4242));
    assert!(goal_uuid_to_project_id(&g_uuid[..15]).is_none());
    let f_uuid = freelance_uuid(now, 7171);
    assert_eq!(freelance_uuid_to_project_id(&f_uuid), Some(7171));
    assert!(freelance_uuid_to_project_id(&[]).is_none());

    // Validation errors on create_canonical_project
    assert!(create_canonical_project(
        &ctx.db,
        CanonicalProjectSpec {
            corp_id,
            creator_id: director_id,
            name: "ZeroTarget".into(),
            description: String::new(),
            target_quantity: 0,
            initial_quantity: 0,
            escrow: IskCents::from_isk(1000),
            isk_per_unit: IskCents::from_isk(10),
            wallet_division: 1,
            matcher_type: "kill_npc".into(),
            is_freelance: false,
            auto_payout_on_contribution: false,
            max_per_character: None,
            expires_unix: None,
            filter_id: None,
            batch_size: 1,
            now_unix: now,
            min_project_id: 200,
        },
    )
    .is_err());

    assert!(create_canonical_project(
        &ctx.db,
        CanonicalProjectSpec {
            corp_id,
            creator_id: director_id,
            name: "Overdrawn".into(),
            description: String::new(),
            target_quantity: 10,
            initial_quantity: 0,
            escrow: IskCents::from_isk(999_999_999_999),
            isk_per_unit: IskCents::from_isk(10),
            wallet_division: 1,
            matcher_type: "kill_npc".into(),
            is_freelance: false,
            auto_payout_on_contribution: false,
            max_per_character: None,
            expires_unix: None,
            filter_id: None,
            batch_size: 1,
            now_unix: now,
            min_project_id: 200,
        },
    )
    .is_err());

    // Valid corp goal creation -> activity progress -> pro-rata reward redemption -> handle_edit_project -> close refund
    let (goal_pid, _) = create_canonical_project(
        &ctx.db,
        CanonicalProjectSpec {
            corp_id,
            creator_id: director_id,
            name: "Bounty Drive".into(),
            description: "Kill pirate NPCs".into(),
            target_quantity: 100,
            initial_quantity: 0,
            escrow: IskCents::from_isk(50_000),
            isk_per_unit: IskCents::from_isk(500),
            wallet_division: 1,
            matcher_type: "kill_npc".into(),
            is_freelance: false,
            auto_payout_on_contribution: false,
            max_per_character: None,
            expires_unix: Some(now + 3_600),
            filter_id: None,
            batch_size: 1,
            now_unix: now,
            min_project_id: 300,
        },
    )
    .unwrap();

    // Uncontributed worker cannot redeem
    assert!(redeem_corp_goal_reward_for_char(&ctx.db, goal_pid, worker_id, now).is_err());

    let pre_bal = ctx.db.with_snapshot(|s| s.characters.get(&worker_id).unwrap().balance.0);
    record_server_project_activity(&ctx, worker_id, &["kill_npc"], 10, None);
    let payout_isk = redeem_corp_goal_reward_for_char(&ctx.db, goal_pid, worker_id, now).unwrap();
    assert_eq!(payout_isk, 5_000);
    let post_bal = ctx.db.with_snapshot(|s| s.characters.get(&worker_id).unwrap().balance.0);
    assert_eq!(post_bal - pre_bal, IskCents::from_isk(5_000).0);
    assert!(redeem_corp_goal_reward_for_char(&ctx.db, goal_pid, worker_id, now).is_err());
    assert!(redeem_all_corp_goal_rewards_for_char(&ctx.db, worker_id, now).unwrap().is_empty());

    // handle_edit_project role gate & update
    let mut non_dir = make_session(worker_id, corp_id, 0, 0);
    assert_eq!(
        handle_edit_project(&ctx, &mut non_dir, &[PyRep::Integer(goal_pid), PyRep::String("Hack".into())])
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    let mut dir_sess = make_session(director_id, corp_id, 0, corp_roles::ROLE_DIRECTOR);
    let edited = handle_edit_project(
        &ctx,
        &mut dir_sess,
        &[PyRep::Integer(goal_pid), PyRep::String("Updated Drive".into()), PyRep::Integer((now + 7_200) as i64), PyRep::Integer(25)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_get(&edited, "updated"), Some(&PyRep::Bool(true)));

    // Freelance batch reward calculation (batch_size = 5 -> 7 units yields 1 batch = 2,500 ISK) & expiry refund
    let (fl_pid, _) = create_canonical_project(
        &ctx.db,
        CanonicalProjectSpec {
            corp_id,
            creator_id: director_id,
            name: "Mining Contract".into(),
            description: "Mine ore in batches".into(),
            target_quantity: 50,
            initial_quantity: 0,
            escrow: IskCents::from_isk(25_000),
            isk_per_unit: IskCents::from_isk(500),
            wallet_division: 1,
            matcher_type: "mine_ore".into(),
            is_freelance: true,
            auto_payout_on_contribution: false,
            max_per_character: None,
            expires_unix: Some(now + 100),
            filter_id: None,
            batch_size: 5,
            now_unix: now,
            min_project_id: 400,
        },
    )
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliance_war_state
            .project_details
            .entry(fl_pid)
            .or_default()
            .committed_participants
            .push(worker_id);
    });
    record_server_project_activity(&ctx, worker_id, &["mine_ore"], 7, None);
    let (batches_redeemed, fl_payout_isk) = redeem_freelance_reward_for_char(&ctx.db, fl_pid, worker_id, now).unwrap();
    assert_eq!((batches_redeemed, fl_payout_isk), (1, 2_500));
    assert!(redeem_freelance_reward_for_char(&ctx.db, fl_pid, worker_id, now).is_err());

    // Close goal & sweep expired freelance project to refund remaining escrow
    let (_, closed_refund) = close_or_delete_canonical_project(&ctx.db, goal_pid, false, now + 10).unwrap();
    assert_eq!(closed_refund, IskCents::from_isk(45_000));
    let expired_swept = ctx.db.with_mut_snapshot(|snap| sweep_expired_corp_projects_in_snap(snap, now + 200));
    assert!(expired_swept.contains(&(fl_pid, corp_id)));
}
