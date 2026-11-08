//! Unit tests for `fleet_social_corp_gov` votes, sanctioned actions, medals, and bulletins.

use super::{call_gov, kv, rs_rows};
use crate::services::fleet_social::fleet_social_corps::fleet_social_corps_tests::seed_player_corps;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_db::{
    corp_roles, CorpVoteBallotRecord, CorpVoteCaseRecord, InventoryFlag, InventoryItemRecord,
    ServerClock, TrainedSkillRecord,
};
use eve_proto::PyRep;

#[tokio::test]
async fn corp_gov_votes_sanctioned_actions_and_employment_record() {
    let (ctx, clock) = test_ctx_and_clock(1_700_000_000);
    let (mut ceo, mut member) = seed_player_corps(&ctx);

    let mut unskilled = member.clone();
    unskilled.character_id = Some(90_000_088);
    let can_run_unskilled = call_gov(&ctx, &mut unskilled, "voteManager", "CanRunForCEO", vec![])
        .await
        .unwrap();
    assert_eq!(can_run_unskilled.as_bool(), Some(false));

    let can_run_before = call_gov(&ctx, &mut member, "voteManager", "CanRunForCEO", vec![])
        .await
        .unwrap();
    assert_eq!(can_run_before.as_bool(), Some(true));

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_002).or_default().insert(
            3363,
            TrainedSkillRecord { type_id: 3363, level: 2, skill_points: 1000.0 },
        );
        snap.items.insert(
            77_000_001,
            InventoryItemRecord {
                item_id: 77_000_001,
                type_id: 34,
                owner_id: 98_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.employment_history.insert(90_000_001, vec![(98_000_001, 1_700_000_000, None)]);
    });

    assert_eq!(
        call_gov(&ctx, &mut member, "voteManager", "CanRunForCEO", vec![])
            .await
            .unwrap()
            .as_bool(),
        Some(true)
    );
    assert_eq!(
        call_gov(&ctx, &mut member, "corpRegistry", "GetNumberOfPotentialCEOs", vec![])
            .await
            .unwrap()
            .as_i64(),
        Some(1)
    );

    let vc_shares = call_gov(
        &ctx,
        &mut ceo,
        "voteManager",
        "InsertVoteCase",
        vec![
            PyRep::String("Mint 500 Shares".into()),
            PyRep::String("Expand share capital".into()),
            PyRep::Integer(5),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::String("Approve 500".into()), PyRep::Integer(500)]),
                PyRep::Tuple(vec![PyRep::Integer(2), PyRep::String("Reject".into()), PyRep::Integer(0)]),
            ]),
            PyRep::Integer(1),
        ],
    )
    .await
    .unwrap();
    let case_id = kv(&vc_shares, "voteCaseID").as_i64().unwrap();

    let ballot = call_gov(
        &ctx,
        &mut ceo,
        "voteManager",
        "InsertVote",
        vec![PyRep::Integer(98_000_001), PyRep::Integer(case_id), PyRep::Integer(1)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&ballot, "weight").as_i64(), Some(1));

    let err_early = call_gov(
        &ctx,
        &mut ceo,
        "voteManager",
        "ActivateSanctionedAction",
        vec![PyRep::Integer(case_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_early.code, "VoteStillOpen");

    clock.advance_secs(90_000);
    let act_shares = call_gov(
        &ctx,
        &mut ceo,
        "voteManager",
        "ActivateSanctionedAction",
        vec![PyRep::Integer(case_id)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&act_shares, "activated").as_bool(), Some(true));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corp_governance[&98_000_001].company_shares),
        500
    );

    let err_twice = call_gov(
        &ctx,
        &mut ceo,
        "voteManager",
        "ActivateSanctionedAction",
        vec![PyRep::Integer(case_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_twice.code, "VoteAlreadyActivated");

    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_vote_cases.insert(
            901,
            CorpVoteCaseRecord {
                case_id: 901,
                corporation_id: 98_000_001,
                vote_type: 1,
                text: "Lock Blueprint".into(),
                description: "Lock BPO".into(),
                start_ts: now_u - 10,
                end_ts: now_u,
                parameter: 77_000_001,
                executed: false,
                options: Vec::new(),
            },
        );
        snap.corp_vote_ballots.insert(
            (901, 90_000_001),
            CorpVoteBallotRecord {
                case_id: 901,
                voter_id: 90_000_001,
                option_id: 1,
                weight: 100,
                voted_at_unix: now_u - 5,
            },
        );
        snap.corp_vote_cases.insert(
            902,
            CorpVoteCaseRecord {
                case_id: 902,
                corporation_id: 98_000_001,
                vote_type: 0,
                text: "CEO Election".into(),
                description: "Elect Line Member".into(),
                start_ts: now_u - 10,
                end_ts: now_u,
                parameter: 90_000_002,
                executed: false,
                options: Vec::new(),
            },
        );
        snap.corp_vote_ballots.insert(
            (902, 90_000_001),
            CorpVoteBallotRecord {
                case_id: 902,
                voter_id: 90_000_001,
                option_id: 1,
                weight: 100,
                voted_at_unix: now_u - 5,
            },
        );
    });

    call_gov(&ctx, &mut ceo, "voteManager", "ActivateSanctionedAction", vec![PyRep::Integer(901)])
        .await
        .unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&77_000_001].flag),
        InventoryFlag::Locked
    );

    call_gov(&ctx, &mut ceo, "voteManager", "ActivateSanctionedAction", vec![PyRep::Integer(902)])
        .await
        .unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corporations[&98_000_001].ceo_id),
        90_000_002
    );
    member.corp_role = corp_roles::ROLE_DIRECTOR;

    let emp = call_gov(
        &ctx,
        &mut member,
        "corporationSvc",
        "GetEmploymentRecord",
        vec![PyRep::Integer(90_000_001)],
    )
    .await
    .unwrap();
    assert_eq!(rs_rows(&emp)[0][0].as_i64(), Some(98_000_001));

    let emp_fallback = call_gov(
        &ctx,
        &mut member,
        "corporationSvc",
        "GetEmploymentRecord",
        vec![PyRep::Integer(90_000_002)],
    )
    .await
    .unwrap();
    assert_eq!(rs_rows(&emp_fallback)[0][0].as_i64(), Some(98_000_001));

    // #549: ActivateSanctionedAction refuses unlocking impounded/contract/safety items and refuses locking fitted/impounded items
    let now_u = clock.now_unix_secs();
    let imp_id = 77_000_549_i64;
    let mod_id = 77_000_550_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            imp_id,
            InventoryItemRecord {
                item_id: imp_id,
                type_id: 34,
                owner_id: 98_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Impounded,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 487,
                owner_id: 98_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for (case_id, vote_type, text, param) in [
            (951_i64, 7_i64, "Unlock Impounded", imp_id),
            (952_i64, 6_i64, "Lock Fitted Module", mod_id),
            (953_i64, 6_i64, "Lock Impounded", imp_id),
        ] {
            snap.corp_vote_cases.insert(
                case_id,
                CorpVoteCaseRecord {
                    case_id,
                    corporation_id: 98_000_001,
                    vote_type,
                    text: text.into(),
                    description: text.into(),
                    start_ts: now_u - 10,
                    end_ts: now_u,
                    parameter: param,
                    executed: false,
                    options: Vec::new(),
                },
            );
            snap.corp_vote_ballots.insert(
                (case_id, 90_000_001),
                CorpVoteBallotRecord {
                    case_id,
                    voter_id: 90_000_001,
                    option_id: 1,
                    weight: 100,
                    voted_at_unix: now_u - 5,
                },
            );
        }
    });
    for case_id in [951_i64, 952, 953] {
        let err = call_gov(
            &ctx,
            &mut member,
            "voteManager",
            "ActivateSanctionedAction",
            vec![PyRep::Integer(case_id)],
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err.code.as_str(), "ImpoundedItem" | "ItemLocked" | "AssetSafetyLocked"),
            "unexpected error for case {case_id}: {}",
            err.code
        );
    }
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&imp_id].flag),
        InventoryFlag::Impounded
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&mod_id].flag),
        InventoryFlag::HiSlot0
    );
}
