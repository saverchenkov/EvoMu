//! Extra unit and mutation-killing tests for `services::fleet_social::fleet_social_corp_gov`
//! covering `AddCorporation` error/success variants, `GetWalletDivisionsInfo` role gates,
//! share transfers (`MoveCompanyShares`, `MovePrivateShares`, `GetSharesByShareholder`),
//! `PayoutDividend` argument permutations and corp-shareholder payouts, title delayed role grants,
//! `InsertVote` / `ActivateSanctionedAction` edge cases, and `corpRecProxy` search filters.

#![forbid(unsafe_code)]

use crate::services::fleet_social::fleet_social_corps::fleet_social_corps_tests::seed_player_corps;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_and_clock,
};
use crate::services::fleet_social::handle;
use eve_db::{
    corp_roles,
    IskCents, ServerClock, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub(super) fn call_corp_gov(
    ctx: &crate::router::ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(handle(ctx, sess, svc, method, args, kwargs))
        .unwrap_or_else(|| panic!("unhandled {svc}.{method}"))
}

pub(super) fn rs_rows(rep: &PyRep) -> &[Vec<PyRep>] {
    match rep {
        PyRep::Rowset { rows, .. } => rows,
        other => panic!("expected Rowset, got {other:?}"),
    }
}

#[test]
fn add_corporation_and_wallet_divisions_validation_and_role_gates() {
    let (ctx, clock) = test_ctx_and_clock(1_700_000_000);
    let (mut ceo, mut member) = seed_player_corps(&ctx);
    let kw = HashMap::new();

    // 1. GetWalletDivisionsInfo: non-existent corp -> CorporationNotFound
    let err_nf = call_corp_gov(
        &ctx,
        &mut ceo,
        "account",
        "GetWalletDivisionsInfo",
        &[PyRep::Integer(99_999_999)],
        &kw,
    )
    .unwrap_err();
    assert_eq!(err_nf.code, "CorporationNotFound");

    // 2. Member without Accountant/Junior Accountant role -> CorpRoleDenied
    let err_role = call_corp_gov(
        &ctx,
        &mut member,
        "account",
        "GetWalletDivisionsInfo",
        &[PyRep::Integer(98_000_001)],
        &kw,
    )
    .unwrap_err();
    assert_eq!(err_role.code, "CorpRoleDenied");

    // 3. Grant ROLE_JUNIOR_ACCOUNTANT -> succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&98_000_001)
            .unwrap()
            .member_roles
            .insert(90_000_002, corp_roles::ROLE_JUNIOR_ACCOUNTANT);
    });
    let divs = call_corp_gov(
        &ctx,
        &mut member,
        "account",
        "GetWalletDivisionsInfo",
        &[PyRep::Integer(98_000_001)],
        &kw,
    )
    .unwrap();
    assert_eq!(rs_rows(&divs).len(), 7);

    // 4. AddCorporation error paths: empty name, AlreadyCEO, missing skill, name/ticker taken, insufficient funds
    let err_empty = call_corp_gov(
        &ctx,
        &mut member,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("   ".into())],
        &kw,
    )
    .unwrap_err();
    assert_eq!(err_empty.code, "InvalidCorporationName");

    let err_ceo = call_corp_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("New Corp".into()), PyRep::String("NEWC".into())],
        &kw,
    )
    .unwrap_err();
    assert_eq!(err_ceo.code, "AlreadyCEO");

    let mut founder = member.clone();
    founder.character_id = Some(90_000_077);
    founder.corporation_id = Some(1_000_125);
    founder.alliance_id = Some(99_000_001);
    founder.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, 90_000_077, "Founder77", 1_000_125, 30_000_142, 60_003_760);
        ch.balance = IskCents::from_isk(100_000); // Not enough for 1,599,800 ISK fee
        ch.active_ship_id = 55_777_001;
        snap.characters.insert(90_000_077, ch);
    });

    let err_skill = call_corp_gov(
        &ctx,
        &mut founder,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Nova Corp".into()), PyRep::String("NOVA".into())],
        &kw,
    )
    .unwrap_err();
    assert_eq!(err_skill.code, "MissingCorporationManagementSkill");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_077).or_default().insert(
            3363,
            TrainedSkillRecord {
                type_id: 3363,
                level: 1,
                skill_points: 250.0,
            },
        );
    });

    let existing_name = ctx.db.with_snapshot(|s| s.corporations[&98_000_001].name.clone());
    let err_taken = call_corp_gov(
        &ctx,
        &mut founder,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String(existing_name), PyRep::String("UNIQ".into())],
        &kw,
    )
    .unwrap_err();
    assert_eq!(err_taken.code, "CorpNameOrTickerTaken");

    let err_funds = call_corp_gov(
        &ctx,
        &mut founder,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Nova Corp".into()), PyRep::String("NOVA".into())],
        &kw,
    )
    .unwrap_err();
    assert_eq!(err_funds.code, "InsufficientFunds");

    // Credit balance and create corporation with short 3-arg form (name, ticker, tax_rate)
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_077).unwrap().balance = IskCents::from_isk(5_000_000);
    });
    let created = call_corp_gov(
        &ctx,
        &mut founder,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Nova Corp".into()),
            PyRep::String("NOVA".into()),
            PyRep::Real(0.08),
        ],
        &kw,
    )
    .unwrap();
    let new_cid = created.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(founder.corporation_id, Some(new_cid));
    assert_eq!(founder.alliance_id, None);
    assert_eq!(founder.ship_id, Some(55_777_001));

    // Immediate second AddCorporation blocked by 10s session-change timer
    let err_timer = call_corp_gov(
        &ctx,
        &mut founder,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Second Corp".into()), PyRep::String("SEC2".into())],
        &kw,
    )
    .unwrap_err();
    assert_eq!(err_timer.code, "SessionChangeInProgress");

    clock.advance_secs(15);
    // Solo CEO resigning without successor succeeds with newCeoID == 0
    let resigned = call_corp_gov(&ctx, &mut founder, "corpRegistry", "ResignFromCEO", &[], &kw).unwrap();
    assert_eq!(resigned.dict_get("newCeoID"), Some(&PyRep::Integer(0)));
    assert_eq!(founder.corp_role, 0);
}
