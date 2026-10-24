//! Integration tests covering open RPC parity rows for `agentMgr` (11 rows) and `voteManager` (10 rows).
//!
//! Covers:
//! - Agent attributes & solar system: `agentID`, `corporationID`, `factionID`, `solarsystemID`,
//!   `stationID`, `GetSolarSystemOfAgent`
//! - Career & Epic Arcs: `GetCompletedCareerAgentIDs`, `GetDisabledMissions`,
//!   `GetMessagesForEpicArcMissions`, `GetMyEpicArcStatus`, `GetPlausibleMissionIDs`
//! - Corporation voting & cases: `CanViewVotes`, `CanRunForCEO`, `GetVoteCasesByCorporation`,
//!   `GetVoteCase`, `GetVoteCaseOptions`, `GetVotes`, `InsertVoteCase`, `InsertVote`,
//!   `GetCorpSanctionedActions`, `ActivateSanctionedAction`

use crate::common;
use eve_db::{
    ActiveMissionRecord, CorporationRecord, IskCents, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::{HashMap, HashSet};

const PILOT1: i64 = 90_000_001;
const CANDIDATE_CEO: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const KNOWN_AGENT_ID: i64 = 3011110;
const CAREER_AGENT_ID: i64 = 3018920;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
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

#[tokio::test]
async fn test_agent_mgr_attributes_and_system() {
    // parity: rpc:agentMgr.agentID, rpc:agentMgr.corporationID, rpc:agentMgr.factionID, rpc:agentMgr.solarsystemID, rpc:agentMgr.stationID, rpc:agentMgr.GetSolarSystemOfAgent
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess = setup_pilot(&ctx, 503, "demo", "demo-secret", PILOT1).await;
    sess.solar_system_id = Some(SYS_JITA);

    let agent_sde = ctx
        .sde
        .agents
        .get(&KNOWN_AGENT_ID)
        .expect("Known agent must exist in SDE");
    let exp_corp = agent_sde.corporation_id;
    let exp_faction = ctx
        .sde
        .npc_corporations
        .get(&exp_corp)
        .and_then(|c| c.faction_id)
        .unwrap_or(500001);
    let exp_sys = agent_sde.solar_system_id.unwrap_or(30000142) as i64;
    let exp_station = agent_sde.station_id.unwrap_or(60003760);

    // 1. agentMgr.agentID
    let aid_res = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "agentID",
        &[PyRep::Integer(KNOWN_AGENT_ID)],
    )
    .await
    .expect("agentMgr.agentID");
    assert_eq!(aid_res, PyRep::Integer(KNOWN_AGENT_ID));

    // 2. agentMgr.corporationID
    let corp_res = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "corporationID",
        &[PyRep::Integer(KNOWN_AGENT_ID)],
    )
    .await
    .expect("agentMgr.corporationID");
    assert_eq!(corp_res, PyRep::Integer(exp_corp));

    // 3. agentMgr.factionID
    let fact_res = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "factionID",
        &[PyRep::Integer(KNOWN_AGENT_ID)],
    )
    .await
    .expect("agentMgr.factionID");
    assert_eq!(fact_res, PyRep::Integer(exp_faction as i64));

    // 4. agentMgr.solarsystemID
    let sys_res = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "solarsystemID",
        &[PyRep::Integer(KNOWN_AGENT_ID)],
    )
    .await
    .expect("agentMgr.solarsystemID");
    assert_eq!(sys_res, PyRep::Integer(exp_sys));

    // 5. agentMgr.stationID
    let sta_res = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "stationID",
        &[PyRep::Integer(KNOWN_AGENT_ID)],
    )
    .await
    .expect("agentMgr.stationID");
    assert_eq!(sta_res, PyRep::Integer(exp_station));

    // 6. agentMgr.GetSolarSystemOfAgent
    let get_sys_res = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetSolarSystemOfAgent",
        &[PyRep::Integer(KNOWN_AGENT_ID)],
    )
    .await
    .expect("agentMgr.GetSolarSystemOfAgent");
    assert_eq!(get_sys_res, PyRep::Integer(exp_sys));

    // Unknown agent returns error
    let unknown_err = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetSolarSystemOfAgent",
        &[PyRep::Integer(99_999_999)],
    )
    .await
    .expect_err("GetSolarSystemOfAgent for unknown agent must fail");
    assert_eq!(unknown_err.code, "UnknownAgent");
}

#[tokio::test]
async fn test_agent_mgr_career_and_epic_arcs() {
    // parity: rpc:agentMgr.GetCompletedCareerAgentIDs, rpc:agentMgr.GetDisabledMissions, rpc:agentMgr.GetMessagesForEpicArcMissions, rpc:agentMgr.GetMyEpicArcStatus, rpc:agentMgr.GetPlausibleMissionIDs
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess = setup_pilot(&ctx, 504, "demo", "demo-secret", PILOT1).await;

    // Seed a completed career agent mission for PILOT1
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (PILOT1, 9901),
            ActiveMissionRecord {
                mission_id: 9901,
                character_id: PILOT1,
                agent_id: CAREER_AGENT_ID,
                solar_system_id: 30012547,
                state: "Completed".into(),
                current_room_id: 0,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
            },
        );
    });

    // 1. agentMgr.GetCompletedCareerAgentIDs
    let completed_res = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetCompletedCareerAgentIDs",
        &[],
    )
    .await
    .expect("agentMgr.GetCompletedCareerAgentIDs");
    match completed_res {
        PyRep::List(ids) => {
            assert!(
                ids.contains(&PyRep::Integer(CAREER_AGENT_ID)),
                "Completed career agents must include CAREER_AGENT_ID"
            );
        }
        other => panic!("Expected PyRep::List from GetCompletedCareerAgentIDs, got: {other:?}"),
    }

    // 2. agentMgr.GetDisabledMissions
    let disabled_res = rpc(&ctx, &mut sess, "agentMgr", "GetDisabledMissions", &[])
        .await
        .expect("agentMgr.GetDisabledMissions");
    assert_eq!(disabled_res, PyRep::List(Vec::new()));

    // 3. agentMgr.GetPlausibleMissionIDs
    let plausible_res = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetPlausibleMissionIDs",
        &[],
    )
    .await
    .expect("agentMgr.GetPlausibleMissionIDs");
    match plausible_res {
        PyRep::List(mids) => {
            assert!(!mids.is_empty(), "Plausible missions list must not be empty");
            assert!(mids.len() <= 25, "Plausible missions sample should be <= 25");
        }
        other => panic!("Expected PyRep::List from GetPlausibleMissionIDs, got: {other:?}"),
    }

    // 4. agentMgr.GetMyEpicArcStatus
    let epic_status_res = rpc(&ctx, &mut sess, "agentMgr", "GetMyEpicArcStatus", &[])
        .await
        .expect("agentMgr.GetMyEpicArcStatus");
    assert!(matches!(epic_status_res, PyRep::List(_)));

    // 5. agentMgr.GetMessagesForEpicArcMissions
    let epic_msgs_res = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetMessagesForEpicArcMissions",
        &[],
    )
    .await
    .expect("agentMgr.GetMessagesForEpicArcMissions");
    assert!(matches!(epic_msgs_res, PyRep::List(_)));
}

#[tokio::test]
async fn test_vote_manager_cases_and_actions() {
    // parity: rpc:voteManager.CanViewVotes, rpc:voteManager.CanRunForCEO, rpc:voteManager.GetVoteCasesByCorporation, rpc:voteManager.GetVoteCase, rpc:voteManager.GetVoteCaseOptions, rpc:voteManager.GetVotes, rpc:voteManager.InsertVoteCase, rpc:voteManager.InsertVote, rpc:voteManager.GetCorpSanctionedActions, rpc:voteManager.ActivateSanctionedAction
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess = setup_pilot(&ctx, 505, "demo", "demo-secret", PILOT1).await;
    let corp_id = 98_000_101_i64;

    // 1. CanViewVotes & CanRunForCEO without Corporation Management skill (3363)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT1).or_default().remove(&3363);
    });
    let can_view_init = rpc(&ctx, &mut sess, "voteManager", "CanViewVotes", &[])
        .await
        .expect("voteManager.CanViewVotes init");
    assert_eq!(can_view_init, PyRep::Bool(false));

    let can_run_init = rpc(&ctx, &mut sess, "voteManager", "CanRunForCEO", &[])
        .await
        .expect("voteManager.CanRunForCEO init");
    assert_eq!(can_run_init, PyRep::Bool(false));

    // Train Corporation Management skill level 5
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(PILOT1).or_default();
        sk.insert(
            3363,
            TrainedSkillRecord {
                type_id: 3363,
                level: 5,
                skill_points: 256_000.0,
            },
        );

        // Seed player corporation with PILOT1 as CEO and initial director
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "EvoMu Shareholder Collective".into(),
                ticker: "ESHC".into(),
                ceo_id: PILOT1,
                alliance_id: None,
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(10_000_000),
                members: HashSet::from([PILOT1, CANDIDATE_CEO]),
                member_roles: HashMap::from([(PILOT1, u64::MAX)]),
                applications: HashMap::new(),
                wallet_divisions: Vec::new(),
                description: "Corporate governance voting test corporation".into(),
                url: "".into(),
                hq_station_id: None,
            },
        );
    });
    sess.corporation_id = Some(corp_id);

    // Verify CanViewVotes and CanRunForCEO now return true
    let can_view_trained = rpc(&ctx, &mut sess, "voteManager", "CanViewVotes", &[])
        .await
        .expect("voteManager.CanViewVotes trained");
    assert_eq!(can_view_trained, PyRep::Bool(true));

    let can_run_trained = rpc(&ctx, &mut sess, "voteManager", "CanRunForCEO", &[])
        .await
        .expect("voteManager.CanRunForCEO trained");
    assert_eq!(can_run_trained, PyRep::Bool(true));

    // 2. voteManager.InsertVoteCase (V24 layout with custom voteCaseOptions Rowset)
    let custom_opts = PyRep::rowset(
        vec!["optionText", "parameter", "parameter1", "parameter2"],
        vec![
            vec![
                PyRep::String("Elect Candidate CEO".into()),
                PyRep::Integer(CANDIDATE_CEO),
                PyRep::None,
                PyRep::None,
            ],
            vec![
                PyRep::String("Keep Current Leadership".into()),
                PyRep::Integer(0),
                PyRep::None,
                PyRep::None,
            ],
        ],
    );
    let insert_case_res = rpc(
        &ctx,
        &mut sess,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::Integer(corp_id),
            PyRep::String("CEO Election 2026".into()),
            PyRep::String("Elect candidate as new corporation CEO".into()),
            PyRep::Integer(0), // voteType = 0 (CEO Election)
            custom_opts,
            PyRep::Integer(1), // 1 day duration
        ],
    )
    .await
    .expect("voteManager.InsertVoteCase");
    let case_id = insert_case_res
        .dict_get("voteCaseID")
        .and_then(|v| match v {
            PyRep::Integer(i) => Some(*i),
            _ => None,
        })
        .expect("voteCaseID in InsertVoteCase result");
    assert_eq!(insert_case_res.dict_get("voteType"), Some(&PyRep::Integer(0)));
    assert_eq!(
        insert_case_res.dict_get("parameter"),
        Some(&PyRep::Integer(CANDIDATE_CEO))
    );

    let has_case = ctx.db.with_snapshot(|snap| {
        snap.corp_vote_cases
            .get(&case_id)
            .is_some_and(|c| c.corporation_id == corp_id && c.options.len() == 2)
    });
    assert!(
        has_case,
        "InsertVoteCase must persist vote case and custom options in snapshot corp_vote_cases"
    );

    // Non-member candidate fails with CandidateNotMember
    let invalid_cand_err = rpc(
        &ctx,
        &mut sess,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::String("Hostile Takeover".into()),
            PyRep::String("Attempt election of non-member outsider".into()),
            PyRep::Integer(0), // CEO Election
            PyRep::Integer(99_999_999),
        ],
    )
    .await
    .expect_err("CEO election for non-member must fail");
    assert_eq!(invalid_cand_err.code, "CandidateNotMember");

    // 3. voteManager.GetVoteCasesByCorporation (all, open status=2, closed status=1, maxLen)
    let cases_open = rpc(
        &ctx,
        &mut sess,
        "voteManager",
        "GetVoteCasesByCorporation",
        &[PyRep::Integer(corp_id), PyRep::Integer(2)],
    )
    .await
    .expect("voteManager.GetVoteCasesByCorporation open");
    if let PyRep::Rowset { rows, .. } = &cases_open {
        assert_eq!(rows.len(), 1);
    } else {
        panic!("expected Rowset");
    }
    let cases_closed = rpc(
        &ctx,
        &mut sess,
        "voteManager",
        "GetVoteCasesByCorporation",
        &[PyRep::Integer(corp_id), PyRep::Integer(1)],
    )
    .await
    .expect("voteManager.GetVoteCasesByCorporation closed");
    if let PyRep::Rowset { rows, .. } = &cases_closed {
        assert_eq!(rows.len(), 0, "open vote case must not appear in closed filter");
    }

    // 4. voteManager.GetVoteCase (2-arg (corpID, voteCaseID) + VoteCaseNotFound on unknown/wrong corp)
    assert!(
        rpc(
            &ctx,
            &mut sess,
            "voteManager",
            "GetVoteCase",
            &[PyRep::Integer(corp_id), PyRep::Integer(999_999)]
        )
        .await
        .is_err(),
        "unknown voteCaseID must return VoteCaseNotFound"
    );
    assert!(
        rpc(
            &ctx,
            &mut sess,
            "voteManager",
            "GetVoteCase",
            &[PyRep::Integer(98_999_999), PyRep::Integer(case_id)]
        )
        .await
        .is_err(),
        "wrong corpID must return VoteCaseNotFound"
    );
    let single_case_res = rpc(
        &ctx,
        &mut sess,
        "voteManager",
        "GetVoteCase",
        &[PyRep::Integer(corp_id), PyRep::Integer(case_id)],
    )
    .await
    .expect("voteManager.GetVoteCase");
    assert!(matches!(single_case_res, PyRep::Rowset { .. }));

    // 5. voteManager.GetVoteCaseOptions (persisted custom options + VoteCaseNotFound on wrong corp)
    assert!(
        rpc(
            &ctx,
            &mut sess,
            "voteManager",
            "GetVoteCaseOptions",
            &[PyRep::Integer(98_999_999), PyRep::Integer(case_id)]
        )
        .await
        .is_err()
    );
    let opts_res = rpc(
        &ctx,
        &mut sess,
        "voteManager",
        "GetVoteCaseOptions",
        &[PyRep::Integer(corp_id), PyRep::Integer(case_id)],
    )
    .await
    .expect("voteManager.GetVoteCaseOptions");
    if let PyRep::Rowset { columns, rows } = &opts_res {
        assert!(columns.contains(&"parameter".to_string()));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][2], PyRep::String("Elect Candidate CEO".into()));
        assert_eq!(rows[0][3], PyRep::Integer(CANDIDATE_CEO));
    } else {
        panic!("expected Rowset from GetVoteCaseOptions");
    }

    // 6. voteManager.InsertVote (vote for option 1: "Elect Candidate CEO")
    let insert_vote_res = rpc(
        &ctx,
        &mut sess,
        "voteManager",
        "InsertVote",
        &[
            PyRep::Integer(case_id),
            PyRep::Integer(1), // Option 1 ("Elect Candidate CEO")
        ],
    )
    .await
    .expect("voteManager.InsertVote");
    assert_eq!(
        insert_vote_res.dict_get("voteCaseID"),
        Some(&PyRep::Integer(case_id))
    );
    assert_eq!(insert_vote_res.dict_get("optionID"), Some(&PyRep::Integer(1)));

    let has_ballot = ctx.db.with_snapshot(|snap| {
        snap.corp_vote_ballots
            .get(&(case_id, PILOT1))
            .is_some_and(|b| b.option_id == 1)
    });
    assert!(
        has_ballot,
        "InsertVote must persist vote record in snapshot corp_vote_ballots"
    );

    // 7. voteManager.GetVotes
    let votes_res = rpc(
        &ctx,
        &mut sess,
        "voteManager",
        "GetVotes",
        &[PyRep::Integer(corp_id), PyRep::Integer(case_id)],
    )
    .await
    .expect("voteManager.GetVotes");
    assert!(matches!(votes_res, PyRep::Rowset { .. }));

    // 8. voteManager.GetCorpSanctionedActions
    let sa_early_res = rpc(
        &ctx,
        &mut sess,
        "voteManager",
        "GetCorpSanctionedActions",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("voteManager.GetCorpSanctionedActions early");
    if let PyRep::Rowset { rows, .. } = &sa_early_res {
        assert_eq!(rows.len(), 0, "No sanctioned actions while vote is still open");
    }

    // 9. Activating before voting period ends fails with VoteStillOpen
    let early_act_err = rpc(
        &ctx,
        &mut sess,
        "voteManager",
        "ActivateSanctionedAction",
        &[PyRep::Integer(case_id)],
    )
    .await
    .expect_err("Activating sanctioned action early must fail");
    assert_eq!(early_act_err.code, "VoteStillOpen");

    // Advance clock past the 24-hour voting window (86,400s)
    ctx.advance_secs(86_401);

    // After voting ends and passed, GetCorpSanctionedActions returns the case
    let sa_ready_res = rpc(
        &ctx,
        &mut sess,
        "voteManager",
        "GetCorpSanctionedActions",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("voteManager.GetCorpSanctionedActions ready");
    if let PyRep::Rowset { rows, .. } = &sa_ready_res {
        assert_eq!(rows.len(), 1, "Passed vote case must appear in sanctioned actions");
    }

    // 10. voteManager.ActivateSanctionedAction
    let act_res = rpc(
        &ctx,
        &mut sess,
        "voteManager",
        "ActivateSanctionedAction",
        &[PyRep::Integer(case_id)],
    )
    .await
    .expect("voteManager.ActivateSanctionedAction");
    assert_eq!(act_res.dict_get("voteCaseID"), Some(&PyRep::Integer(case_id)));
    assert_eq!(act_res.dict_get("activated"), Some(&PyRep::Bool(true)));

    // Verify DB snapshot reflects new CEO and executed flag
    let (new_ceo, is_executed) = ctx.db.with_snapshot(|snap| {
        let ceo = snap.corporations.get(&corp_id).map(|c| c.ceo_id);
        let exec = snap.corp_vote_cases.get(&case_id).map(|c| c.executed).unwrap_or(false);
        (ceo, exec)
    });
    assert_eq!(
        new_ceo,
        Some(CANDIDATE_CEO),
        "ActivateSanctionedAction must mutate CEO ID in world snapshot"
    );
    assert!(is_executed, "ActivateSanctionedAction must set executed flag in snapshot");

    // Activating again fails with VoteAlreadyActivated
    let double_act_err = rpc(
        &ctx,
        &mut sess,
        "voteManager",
        "ActivateSanctionedAction",
        &[PyRep::Integer(case_id)],
    )
    .await
    .expect_err("Activating already activated sanctioned action must fail");
    assert_eq!(double_act_err.code, "VoteAlreadyActivated");
}
