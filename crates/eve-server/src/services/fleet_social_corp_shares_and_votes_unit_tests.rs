//! Semantic unit tests split from `fleet_social_corp_gov_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::fleet_social_corp_gov_extra_unit_tests::*;

use crate::services::fleet_social::fleet_social_corps::fleet_social_corps_tests::seed_player_corps;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_db::{
    CorpVoteBallotRecord, CorpVoteCaseRecord, InventoryFlag, InventoryItemRecord,
};
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn share_transfers_dividend_variants_and_title_delayed_grants() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (mut ceo, mut member) = seed_player_corps(&ctx);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let g = snap.corp_governance.entry(98_000_001).or_default();
        g.shares.insert(98_000_001, 1_000);
        g.company_shares = 1_000;
    });

    // MoveCompanyShares error branches: wrong corp, invalid qty, target not found, insufficient shares
    assert_eq!(
        call_corp_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "MoveCompanyShares",
            &[PyRep::Integer(98_000_002), PyRep::Integer(90_000_001), PyRep::Integer(10)],
            &kw,
        )
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_corp_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "MoveCompanyShares",
            &[PyRep::Integer(90_000_001), PyRep::Integer(0)],
            &kw,
        )
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_corp_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "MoveCompanyShares",
            &[PyRep::Integer(99_888_777), PyRep::Integer(10)],
            &kw,
        )
        .unwrap_err()
        .code,
        "TargetNotFound"
    );
    assert_eq!(
        call_corp_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "MoveCompanyShares",
            &[PyRep::Integer(90_000_001), PyRep::Integer(5_000)],
            &kw,
        )
        .unwrap_err()
        .code,
        "InsufficientShares"
    );

    // 2-arg MoveCompanyShares to external corporation (98_000_002) and to CEO (90_000_001)
    call_corp_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "MoveCompanyShares",
        &[PyRep::Integer(98_000_002), PyRep::Integer(200)],
        &kw,
    )
    .unwrap();
    call_corp_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "MoveCompanyShares",
        &[PyRep::Integer(90_000_001), PyRep::Integer(300)],
        &kw,
    )
    .unwrap();

    // MovePrivateShares 2-arg form back to company (98_000_001) updates company_shares
    call_corp_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "MovePrivateShares",
        &[PyRep::Integer(98_000_001), PyRep::Integer(100)],
        &kw,
    )
    .unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corp_governance[&98_000_001].company_shares),
        600
    );

    // GetSharesByShareholder argument forms: [Bool(true)], [Integer(98_000_002)], [None, Integer(90_000_001)], []
    let sh_corp = call_corp_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "GetSharesByShareholder",
        &[PyRep::Bool(true)],
        &kw,
    )
    .unwrap();
    assert_eq!(rs_rows(&sh_corp)[0][2].as_i64(), Some(600));

    // Third-party corporation/character and non-director corp share queries must be denied
    assert_eq!(
        call_corp_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "GetSharesByShareholder",
            &[PyRep::Integer(98_000_002)],
            &kw,
        )
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        call_corp_gov(
            &ctx,
            &mut member,
            "corpRegistry",
            "GetSharesByShareholder",
            &[PyRep::None, PyRep::Integer(90_000_001)],
            &kw,
        )
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        call_corp_gov(
            &ctx,
            &mut member,
            "corpRegistry",
            "GetSharesByShareholder",
            &[PyRep::Bool(true)],
            &kw,
        )
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    let sh_two = call_corp_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "GetSharesByShareholder",
        &[PyRep::None, PyRep::Integer(90_000_001)],
        &kw,
    )
    .unwrap();
    assert_eq!(rs_rows(&sh_two)[0][2].as_i64(), Some(200));
    assert!(call_corp_gov(
        &ctx,
        &mut member,
        "corpRegistry",
        "GetSharesByShareholder",
        &[PyRep::Bool(false)],
        &kw,
    )
    .is_ok());

    // PayoutDividend error branches & (Real, Bool, Integer) form crediting both character and corporate shareholder!
    assert_eq!(
        call_corp_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "PayoutDividend",
            &[PyRep::Real(1_000.0), PyRep::Bool(true), PyRep::Integer(9)],
            &kw,
        )
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_corp_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "PayoutDividend",
            &[PyRep::Real(-50.0), PyRep::Bool(true), PyRep::Integer(1)],
            &kw,
        )
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    let corp2_before = ctx.db.with_snapshot(|s| s.corporations[&98_000_002].division_balance(1).0);
    let div_res = call_corp_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "PayoutDividend",
        &[PyRep::Real(1_000_000.0), PyRep::Bool(true), PyRep::Integer(1)],
        &kw,
    )
    .unwrap();
    assert_eq!(div_res.dict_get("recipientCount"), Some(&PyRep::Integer(2)));
    assert!(ctx.db.with_snapshot(|s| s.corporations[&98_000_002].division_balance(1).0) > corp2_before);

    // Title update with delayed division role (bit 13) and role drop recording
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance
            .entry(98_000_001)
            .or_default()
            .member_titles
            .insert(90_000_002, 1);
    });
    let delayed_role = (1_u64 << 13) as i64;
    call_corp_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "UpdateTitles",
        &[
            PyRep::Integer(1),
            PyRep::String("Hangar Officer".into()),
            PyRep::Integer(delayed_role),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .unwrap();
    assert!(
        ctx.db.with_snapshot(|s| {
            s.corp_governance[&98_000_001]
                .pending_title_role_grants
                .contains_key(&90_000_002)
        })
    );

    // Dropping the role records role_drop_timestamps
    call_corp_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "UpdateTitle",
        &[
            PyRep::Integer(1),
            PyRep::String("Hangar Officer".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .unwrap();
    assert!(
        ctx.db.with_snapshot(|s| {
            s.corp_governance[&98_000_001]
                .role_drop_timestamps
                .contains_key(&90_000_002)
        })
    );
}

#[test]
fn vote_error_cases_unlock_item_and_recruitment_ad_filters() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (mut ceo, mut member) = seed_player_corps(&ctx);
    let kw = HashMap::new();
    let now = ctx.now_unix();

    let locked_item_id = 77_100_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            locked_item_id,
            InventoryItemRecord {
                item_id: locked_item_id,
                type_id: 34,
                owner_id: 98_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Case 950: closed vote
        snap.corp_vote_cases.insert(
            950,
            CorpVoteCaseRecord {
                case_id: 950,
                corporation_id: 98_000_001,
                vote_type: 7,
                text: "Unlock BPO".into(),
                description: "Unlock".into(),
                start_ts: now - 100,
                end_ts: now - 5,
                parameter: locked_item_id,
                executed: false,
                options: Vec::new(),
            },
        );
        snap.corp_vote_ballots.insert(
            (950, 90_000_001),
            CorpVoteBallotRecord {
                case_id: 950,
                voter_id: 90_000_001,
                option_id: 1,
                weight: 100,
                voted_at_unix: now - 50,
            },
        );
        // Case 951: failed vote (option 2 wins)
        snap.corp_vote_cases.insert(
            951,
            CorpVoteCaseRecord {
                case_id: 951,
                corporation_id: 98_000_001,
                vote_type: 5,
                text: "Reject Mint".into(),
                description: "No".into(),
                start_ts: now - 100,
                end_ts: now - 5,
                parameter: 500,
                executed: false,
                options: Vec::new(),
            },
        );
        snap.corp_vote_ballots.insert(
            (951, 90_000_001),
            CorpVoteBallotRecord {
                case_id: 951,
                voter_id: 90_000_001,
                option_id: 2,
                weight: 100,
                voted_at_unix: now - 50,
            },
        );
    });

    // Voting on closed case 950 -> VoteCaseClosed
    assert_eq!(
        call_corp_gov(
            &ctx,
            &mut ceo,
            "voteManager",
            "InsertVote",
            &[PyRep::Integer(950), PyRep::Integer(1)],
            &kw,
        )
        .unwrap_err()
        .code,
        "VoteCaseClosed"
    );

    // Activating failed case 951 -> VoteDidNotPass
    assert_eq!(
        call_corp_gov(
            &ctx,
            &mut ceo,
            "voteManager",
            "ActivateSanctionedAction",
            &[PyRep::Integer(951)],
            &kw,
        )
        .unwrap_err()
        .code,
        "VoteDidNotPass"
    );

    // Activating unlock case 950 -> unlocks item to Hangar
    call_corp_gov(
        &ctx,
        &mut ceo,
        "voteManager",
        "ActivateSanctionedAction",
        &[PyRep::Integer(98_000_001), PyRep::Integer(950)],
        &kw,
    )
    .unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&locked_item_id].flag),
        InventoryFlag::Hangar
    );

    // Recruitment ad fallback from corp_governance + SearchCorpAds min/max member filter
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance
            .entry(98_000_001)
            .or_default()
            .recruitment_ads
            .push((7, "Legacy Ad".into(), "From gov".into(), 3, 90_000_001));
    });
    let ad_row = call_corp_gov(
        &ctx,
        &mut member,
        "corpRecProxy",
        "GetRecruitmentAd",
        &[PyRep::Integer(7)],
        &kw,
    )
    .unwrap();
    assert_eq!(rs_rows(&ad_row).len(), 1);

    let filtered_out = call_corp_gov(
        &ctx,
        &mut member,
        "corpRecProxy",
        "SearchCorpAds",
        &[PyRep::Integer(3), PyRep::Integer(0), PyRep::Integer(50), PyRep::Integer(100)],
        &kw,
    )
    .unwrap();
    assert!(rs_rows(&filtered_out).is_empty());

    assert_eq!(
        call_corp_gov(
            &ctx,
            &mut ceo,
            "corpRecProxy",
            "DeleteRecruitmentAd",
            &[PyRep::Integer(999)],
            &kw,
        )
        .unwrap_err()
        .code,
        "RecruitmentAdNotFound"
    );
}
