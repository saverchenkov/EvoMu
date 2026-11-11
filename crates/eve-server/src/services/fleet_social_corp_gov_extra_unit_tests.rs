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

#[test]
fn add_corp_rpc_validation_tax_clamp_docked_alliance_stasis_and_ad_fallback() {
    let (ctx, clock) = test_ctx_and_clock(1_700_000_000);
    let (mut ceo, member) = seed_player_corps(&ctx);
    let kw = HashMap::new();

    // 1. AddCorporation validation: non-string ticker & NaN/Inf taxRate (short and long form), empty/unauth
    let mut unauth = SessionState::default();
    assert!(call_corp_gov(&ctx, &mut unauth, "corpRegistry", "AddCorporation", &[PyRep::String("X".into())], &kw).is_err());

    let mut f1 = member.clone();
    f1.character_id = Some(90_000_901);
    f1.corporation_id = Some(1_000_170);
    f1.alliance_id = Some(99_000_001);
    f1.station_id = Some(60_003_760);
    f1.structure_id = None;
    f1.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, 90_000_901, "Founder901", 1_000_170, 30_000_142, 55_777_901);
        ch.balance = IskCents::from_isk(50_000_000);
        ch.alliance_id = Some(99_000_001);
        ch.station_id = Some(60_003_760);
        snap.characters.insert(90_000_901, ch);
        snap.skills.entry(90_000_901).or_default().insert(3363, TrainedSkillRecord {
            type_id: 3363,
            skill_points: 256_000.0,
            level: 1,
        });
    });

    assert!(call_corp_gov(
        &ctx,
        &mut f1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("BadTick".into()), PyRep::Integer(123)],
        &kw,
    )
    .is_err());
    assert!(call_corp_gov(
        &ctx,
        &mut f1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("BadTaxS".into()), PyRep::String("BTS".into()), PyRep::Real(f64::NAN)],
        &kw,
    )
    .is_err());
    assert!(call_corp_gov(
        &ctx,
        &mut f1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("BadTaxL".into()),
            PyRep::String("BTL".into()),
            PyRep::String("desc".into()),
            PyRep::String("url".into()),
            PyRep::Real(f64::INFINITY),
        ],
        &kw,
    )
    .is_err());

    // MustBeDocked when in space or in Destroyed structure
    let prev_st = f1.station_id;
    f1.station_id = None;
    f1.structure_id = None;
    f1.ship_id = Some(55_777_901);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_901) {
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.structures.insert(1_030_000_901, eve_db::UpwellStructureRecord {
            structure_id: 1_030_000_901,
            type_id: 35832,
            owner_corp_id: 1_000_170,
            solar_system_id: 30_000_142,
            name: "Dead Fort".into(),
            state: "Destroyed".into(),
            ..Default::default()
        });
    });
    assert_eq!(
        call_corp_gov(
            &ctx,
            &mut f1,
            "corpRegistry",
            "AddCorporation",
            &[PyRep::String("SpaceCorp".into()), PyRep::String("SPC".into())],
            &kw,
        )
        .unwrap_err()
        .code,
        "MustBeDocked"
    );
    f1.structure_id = Some(1_030_000_901);
    assert_eq!(
        call_corp_gov(
            &ctx,
            &mut f1,
            "corpRegistry",
            "AddCorporation",
            &[PyRep::String("DeadStruct".into()), PyRep::String("DST".into())],
            &kw,
        )
        .unwrap_err()
        .code,
        "MustBeDocked"
    );

    // member_kick_blocker when char holds a role in player corp
    f1.structure_id = None;
    f1.station_id = prev_st;
    let ceo_corp = ceo.corporation_id.unwrap();
    f1.corporation_id = Some(ceo_corp);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_901) {
            ch.station_id = prev_st;
            ch.corporation_id = ceo_corp;
        }
        if let Some(corp) = snap.corporations.get_mut(&ceo_corp) {
            corp.members.insert(90_000_901);
            corp.member_roles.insert(90_000_901, corp_roles::ROLE_JUNIOR_ACCOUNTANT);
        }
    });
    assert_eq!(
        call_corp_gov(
            &ctx,
            &mut f1,
            "corpRegistry",
            "AddCorporation",
            &[PyRep::String("BlockedCorp".into()), PyRep::String("BLKD".into())],
            &kw,
        )
        .unwrap_err()
        .code,
        "MemberHasRoles"
    );

    // Clear roles -> long-form AddCorporation clamps taxRate > 1.0 to 1.0, clears alliance_id & emits OnSessionChanged + OnCorporationChanged
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&ceo_corp) {
            corp.member_roles.insert(90_000_901, 0);
        }
    });
    let _ = ctx.drain_notifications_for_char(90_000_901);
    let long_created = call_corp_gov(
        &ctx,
        &mut f1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Alpha Clamp 901 Corp".into()),
            PyRep::String("J901".into()),
            PyRep::String("Detailed desc".into()),
            PyRep::String("https://alph.invalid".into()),
            PyRep::Real(2.5),
            PyRep::Integer(1),
            PyRep::Integer(2),
            PyRep::Integer(3),
            PyRep::Integer(4),
            PyRep::Integer(5),
            PyRep::Integer(6),
            PyRep::None,
            PyRep::Integer(0),
        ],
        &kw,
    )
    .unwrap();
    let c_alpha = long_created.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(f1.alliance_id, None);
    ctx.db.with_snapshot(|snap| {
        assert!((snap.corporations[&c_alpha].tax_rate - 1.0).abs() < 1e-9);
        assert_eq!(snap.corporations[&c_alpha].description, "Detailed desc");
        assert_eq!(snap.corporations[&c_alpha].url, "https://alph.invalid");
        assert_eq!(snap.characters[&90_000_901].alliance_id, None);
        assert_eq!(snap.corp_governance[&c_alpha].applications_enabled, Some(false));
    });
    assert_eq!(f1.corporation_id, Some(c_alpha));
    assert!(f1.corp_role != 0);
    let _ = ctx.drain_notifications_for_char(90_000_901);

    // Negative taxRate clamps to 0.0
    let mut f2 = member.clone();
    f2.character_id = Some(90_000_902);
    f2.corporation_id = Some(1_000_170);
    f2.station_id = Some(60_003_760);
    f2.structure_id = None;
    f2.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, 90_000_902, "Founder902", 1_000_170, 30_000_142, 55_777_902);
        ch.balance = IskCents::from_isk(50_000_000);
        ch.station_id = Some(60_003_760);
        snap.characters.insert(90_000_902, ch);
        snap.skills.entry(90_000_902).or_default().insert(3363, TrainedSkillRecord {
            type_id: 3363,
            skill_points: 256_000.0,
            level: 1,
        });
    });
    let neg_created = call_corp_gov(
        &ctx,
        &mut f2,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Zero Tax 902 Corp".into()),
            PyRep::String("Z902".into()),
            PyRep::Real(-0.5),
        ],
        &kw,
    )
    .unwrap();
    let c_zero = neg_created.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|snap| assert!((snap.corporations[&c_zero].tax_rate - 0.0).abs() < 1e-9));

    // GetWalletDivisionsInfo: 4 columns with key=999+d & 24h pending role stasis enforcement
    let mut acc = member.clone();
    acc.character_id = Some(90_000_903);
    acc.corporation_id = Some(ceo_corp);
    acc.station_id = Some(60_003_760);
    acc.structure_id = None;
    acc.corp_role = 0;
    let now_u = clock.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, 90_000_903, "Accountant903", ceo_corp, 30_000_142, 55_777_903);
        ch.station_id = Some(60_003_760);
        snap.characters.insert(90_000_903, ch);
        if let Some(corp) = snap.corporations.get_mut(&ceo_corp) {
            corp.members.insert(90_000_903);
            corp.member_roles.insert(90_000_903, 0);
        }
        snap.corp_governance
            .entry(ceo_corp)
            .or_default()
            .pending_role_grants
            .insert(90_000_903, (corp_roles::ROLE_JUNIOR_ACCOUNTANT, now_u + 86_400));
    });
    assert_eq!(
        call_corp_gov(&ctx, &mut acc, "account", "GetWalletDivisionsInfo", &[], &kw)
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    clock.advance_secs(86_401);
    let wdivs = call_corp_gov(&ctx, &mut acc, "account", "GetWalletDivisionsInfo", &[], &kw).unwrap();
    let wrows = rs_rows(&wdivs);
    assert_eq!(wrows.len(), 7);
    assert_eq!(wrows[0][3], PyRep::Integer(1000));
    assert_eq!(wrows[6][3], PyRep::Integer(1006));

    // GetShareholders index_rowset + missing char None & GetSharesByShareholder GM/non-director/3rd-party gates
    let unknown_sh_id = 99_777_888_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let g = snap.corp_governance.entry(ceo_corp).or_default();
        g.shares.insert(unknown_sh_id, 250);
        g.shares.insert(90_000_903, 100);
    });
    let shs = call_corp_gov(&ctx, &mut ceo, "corpRegistry", "GetShareholders", &[], &kw).unwrap();
    match shs {
        PyRep::IndexRowset { key_column, columns, items } => {
            assert_eq!(key_column, "shareholderID");
            assert_eq!(columns, vec!["shareholderID", "corporationID", "shareholderCorporationID", "shares"]);
            let unk_row = items.iter().find(|(k, _)| *k == PyRep::Integer(unknown_sh_id)).expect("unknown shareholder");
            assert_eq!(unk_row.1[2], PyRep::None);
            let known_row = items.iter().find(|(k, _)| *k == PyRep::Integer(90_000_903)).expect("known shareholder");
            assert_eq!(known_row.1[2], PyRep::Integer(ceo_corp));
        }
        other => panic!("expected IndexRowset, got {other:?}"),
    }
    assert_eq!(
        call_corp_gov(&ctx, &mut acc, "corpRegistry", "GetSharesByShareholder", &[PyRep::Bool(true)], &kw)
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        call_corp_gov(&ctx, &mut acc, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(90_000_902)], &kw)
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    assert_eq!(rs_rows(&call_corp_gov(&ctx, &mut acc, "corpRegistry", "GetSharesByShareholder", &[PyRep::Bool(false)], &kw).unwrap()).len(), 1);
    let mut gm = member.clone();
    gm.character_id = Some(90_000_904);
    gm.corporation_id = Some(ceo_corp);
    gm.station_id = Some(60_003_760);
    gm.role = eve_machonet::service_roles::ROLE_GMH;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, 90_000_904, "GM904", ceo_corp, 30_000_142, 55_777_904);
        snap.characters.insert(90_000_904, ch);
    });
    assert_eq!(
        rs_rows(&call_corp_gov(&ctx, &mut gm, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(unknown_sh_id)], &kw).unwrap()).len(),
        1
    );

    // GetNumberOfPotentialCEOs, CanRunForCEO, CanViewVotes, GetEmploymentRecord (history & fallback)
    assert!(call_corp_gov(&ctx, &mut ceo, "corpRegistry", "GetNumberOfPotentialCEOs", &[], &kw)
        .unwrap()
        .as_i64()
        .unwrap() >= 1);
    assert_eq!(call_corp_gov(&ctx, &mut ceo, "voteManager", "CanRunForCEO", &[], &kw).unwrap(), PyRep::Bool(true));
    assert_eq!(call_corp_gov(&ctx, &mut acc, "voteManager", "CanViewVotes", &[], &kw).unwrap(), PyRep::Bool(false));
    assert_eq!(
        rs_rows(&call_corp_gov(&ctx, &mut acc, "corporationSvc", "GetEmploymentRecord", &[PyRep::Integer(90_000_903)], &kw).unwrap()).len(),
        1
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.employment_history.insert(90_000_903, vec![(1_000_170, 1_600_000_000, Some(1_650_000_000)), (ceo_corp, 1_650_000_000, None)]);
    });
    assert_eq!(
        rs_rows(&call_corp_gov(&ctx, &mut acc, "corporationSvc", "GetEmploymentRecord", &[PyRep::Integer(90_000_903)], &kw).unwrap()).len(),
        2
    );

    // Legacy corp_governance.recruitment_ads fallback + GetRecruitmentAdsByCorpID / ForCorp / GetRecruitmentAd / GetRecruiters / character fee fallback
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleet_and_social_ops.corp_recruitment_ads.clear();
        snap.corp_governance.entry(c_alpha).or_default().recruitment_ads.push((77, "Legacy Ad".into(), "Old store".into(), 3, 90_000_901));
    });
    let leg = call_corp_gov(&ctx, &mut f1, "corpRecProxy", "GetRecruitmentAdsForCorporation", &[], &kw).unwrap();
    assert_eq!(rs_rows(&leg).len(), 1);
    assert_eq!(rs_rows(&leg)[0][0], PyRep::Integer(77));
    assert_eq!(
        call_corp_gov(&ctx, &mut f1, "corpRecProxy", "GetRecruiters", &[PyRep::Integer(c_alpha), PyRep::Integer(77)], &kw).unwrap(),
        PyRep::List(vec![PyRep::Integer(90_000_901)])
    );
    assert_eq!(
        call_corp_gov(&ctx, &mut acc, "corpRecProxy", "CreateRecruitmentAd", &[], &kw)
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_corp_gov(&ctx, &mut acc, "corpRecProxy", "DeleteRecruitmentAd", &[PyRep::Integer(77)], &kw)
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    // Character balance debit fallback when corp wallet div 1 has 0 ISK
    let bal_before = ctx.db.with_snapshot(|snap| snap.characters[&90_000_901].balance);
    let created_ad = call_corp_gov(
        &ctx,
        &mut f1,
        "corpRecProxy",
        "CreateRecruitmentAd",
        &[PyRep::Integer(14), PyRep::Integer(5), PyRep::String("New Fallback Ad".into()), PyRep::String("Desc".into()), PyRep::Integer(1)],
        &kw,
    )
    .unwrap();
    let new_ad_id = created_ad.dict_get("adID").and_then(|v| v.as_i64()).unwrap();
    let bal_after = ctx.db.with_snapshot(|snap| snap.characters[&90_000_901].balance);
    assert_eq!(bal_before.0 - bal_after.0, IskCents::from_isk(250_000).0);
    assert_eq!(
        rs_rows(&call_corp_gov(&ctx, &mut f1, "corpRecProxy", "GetRecruitmentAd", &[PyRep::Integer(new_ad_id)], &kw).unwrap()).len(),
        1
    );
    assert_eq!(
        call_corp_gov(&ctx, &mut f1, "corpRecProxy", "DeleteRecruitmentAd", &[PyRep::Integer(999_999)], &kw)
            .unwrap_err()
            .code,
        "RecruitmentAdNotFound"
    );

    // Contacts removal (Integer + List), GetCorporateContacts, CreateLabel/EditLabel/GetLabels
    assert_eq!(
        call_corp_gov(&ctx, &mut acc, "corpRegistry", "RemoveCorporateContacts", &[PyRep::Integer(1)], &kw)
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_corp_gov(&ctx, &mut acc, "corpRegistry", "CreateLabel", &[PyRep::String("Foes".into())], &kw)
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    let lbl = call_corp_gov(&ctx, &mut ceo, "corpRegistry", "CreateLabel", &[PyRep::String("Blues".into()), PyRep::Integer(0x00FF00)], &kw).unwrap();
    let lid = lbl.dict_get("labelID").and_then(|v| v.as_i64()).unwrap();
    let edited = call_corp_gov(&ctx, &mut ceo, "corpRegistry", "EditLabel", &[PyRep::Integer(lid), PyRep::String("Frenemies".into()), PyRep::Integer(0xFF0000)], &kw).unwrap();
    assert_eq!(edited.dict_get("name"), Some(&PyRep::String("Frenemies".into())));
    assert_eq!(rs_rows(&call_corp_gov(&ctx, &mut ceo, "corpRegistry", "GetLabels", &[], &kw).unwrap()).len(), 1);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_contacts.insert(ceo_corp, vec![
            eve_db::SocialContactRecord { contact_id: 500_001, contact_type: "character".into(), standing: 10.0, is_watched: false, is_blocked: false, label_ids: vec![lid] },
            eve_db::SocialContactRecord { contact_id: 500_002, contact_type: "character".into(), standing: -5.0, is_watched: false, is_blocked: false, label_ids: vec![] },
            eve_db::SocialContactRecord { contact_id: 500_003, contact_type: "character".into(), standing: 0.0, is_watched: false, is_blocked: false, label_ids: vec![] },
        ]);
    });
    assert_eq!(rs_rows(&call_corp_gov(&ctx, &mut ceo, "corpRegistry", "GetCorporateContacts", &[], &kw).unwrap()).len(), 3);
    assert!(call_corp_gov(&ctx, &mut ceo, "corpRegistry", "RemoveCorporateContacts", &[PyRep::Integer(500_001)], &kw).is_ok());
    assert!(call_corp_gov(&ctx, &mut ceo, "corpRegistry", "RemoveCorporateContacts", &[PyRep::List(vec![PyRep::Integer(500_002)])], &kw).is_ok());
    let rem_c = call_corp_gov(&ctx, &mut ceo, "corpRegistry", "GetCorporateContacts", &[], &kw).unwrap();
    assert_eq!(rs_rows(&rem_c).len(), 1);
    assert_eq!(rs_rows(&rem_c)[0][0], PyRep::Integer(500_003));
}

#[test]
fn sanctioned_actions_item_locks_ceo_election_share_guards_and_dividend_arg_variants() {
    let (ctx, clock) = test_ctx_and_clock(1_700_100_000);
    let now = clock.now_unix_secs();
    let kw = HashMap::new();
    let (mut ceo_sess, mut member_sess) = seed_player_corps(&ctx);
    let corp_a = 98_000_001i64;
    let corp_b = 98_000_002i64;
    let char_ceo = ceo_sess.character_id.unwrap();
    let char_mem = member_sess.character_id.unwrap();
    let char_foreign = 90_001_003i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_a) {
            c.ceo_id = char_ceo;
            c.members = [char_ceo, char_mem].into_iter().collect();
            c.member_roles.insert(char_ceo, u64::MAX);
            c.member_roles.insert(char_mem, 0);
            c.set_division_balance(1, IskCents::from_isk(1_000_000));
        }
        let st = snap.corp_governance.entry(corp_a).or_default();
        st.shares.insert(char_ceo, 500);
        st.shares.insert(char_mem, 0);
        st.shares.insert(corp_a, 100);
        snap.corp_shareholders.insert(
            corp_a,
            vec![
                eve_db::snapshot_ext::CorpShareholderEntry {
                    shareholder_id: char_ceo,
                    shareholder_type: "character".into(),
                    share_count: 500,
                },
                eve_db::snapshot_ext::CorpShareholderEntry {
                    shareholder_id: corp_a,
                    shareholder_type: "corporation".into(),
                    share_count: 100,
                },
            ],
        );
    });

    // MovePrivateShares error branches
    assert_eq!(
        call_corp_gov(&ctx, &mut ceo_sess, "corpRegistry", "MovePrivateShares", &[PyRep::Integer(corp_a), PyRep::Integer(char_mem), PyRep::Integer(0)], &kw)
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_corp_gov(&ctx, &mut ceo_sess, "corpRegistry", "MovePrivateShares", &[PyRep::Integer(corp_a), PyRep::Integer(999_999_999), PyRep::Integer(10)], &kw)
            .unwrap_err()
            .code,
        "TargetNotFound"
    );
    assert_eq!(
        call_corp_gov(&ctx, &mut member_sess, "corpRegistry", "MovePrivateShares", &[PyRep::Integer(corp_a), PyRep::Integer(char_ceo), PyRep::Integer(10)], &kw)
            .unwrap_err()
            .code,
        "InsufficientShares"
    );

    // PayoutDividend argument forms & validation errors
    assert_eq!(
        call_corp_gov(&ctx, &mut ceo_sess, "corpRegistry", "PayoutDividend", &[PyRep::Bool(false), PyRep::Real(-5.0)], &kw)
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_corp_gov(&ctx, &mut ceo_sess, "corpRegistry", "PayoutDividend", &[PyRep::Real(100.0), PyRep::Bool(false), PyRep::Integer(99)], &kw)
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_corp_gov(&ctx, &mut ceo_sess, "corpRegistry", "PayoutDividend", &[PyRep::Integer(1), PyRep::Real(99_999_999.0)], &kw)
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );
    assert!(
        call_corp_gov(&ctx, &mut ceo_sess, "corpRegistry", "PayoutDividend", &[PyRep::Bool(false), PyRep::Real(1_000.0)], &kw).is_ok()
    );
    assert!(
        call_corp_gov(&ctx, &mut ceo_sess, "corpRegistry", "PayoutDividend", &[PyRep::Real(1_000.0), PyRep::Bool(false)], &kw).is_ok()
    );
    assert!(
        call_corp_gov(&ctx, &mut ceo_sess, "corpRegistry", "PayoutDividend", &[PyRep::Integer(0), PyRep::Real(1_000.0), PyRep::Integer(1)], &kw).is_ok()
    );

    // Prepare closed passed vote cases + ballots for ActivateSanctionedAction & InsertVote branches
    let closed_end = now - 5;
    let open_end = now + 300;
    ctx.db.with_mut_snapshot(|snap| {
        let mk_case = |case_id: i64, c_id: i64, vote_type: i64, param: i64, end_ts: u64, text: &str| eve_db::CorpVoteCaseRecord {
            case_id,
            corporation_id: c_id,
            vote_type,
            text: text.into(),
            description: "desc".into(),
            parameter: param,
            start_ts: now - 100,
            end_ts,
            executed: false,
            options: vec![],
        };
        let mk_item = |item_id: i64, owner_id: i64, location_id: i64, flag: eve_db::InventoryFlag| eve_db::InventoryItemRecord {
            item_id,
            type_id: 645,
            owner_id,
            location_id,
            flag,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        snap.items.insert(880_001, mk_item(880_001, corp_b, 60_003_760, eve_db::InventoryFlag::Hangar));
        snap.items.insert(880_002, mk_item(880_002, corp_a, 60_003_760, eve_db::InventoryFlag::Impounded));
        snap.items.insert(880_003, mk_item(880_003, corp_a, 60_003_760, eve_db::InventoryFlag::HiSlot0));
        snap.items.insert(880_004, mk_item(880_004, corp_a, 60_003_760, eve_db::InventoryFlag::Cargo));
        snap.items.insert(880_005, mk_item(880_005, corp_a, 60_003_760, eve_db::InventoryFlag::CorpSAG1));
        snap.items.insert(880_006, mk_item(880_006, corp_a, 60_003_760, eve_db::InventoryFlag::Locked));
        snap.items.insert(880_007, mk_item(880_007, corp_a, 123_456_789, eve_db::InventoryFlag::Locked));

        let cases = vec![
            (9501, corp_b, 5, 10, open_end, "other corp open"),
            (9502, corp_a, 5, 0, closed_end, "zero shares"),
            (9503, corp_a, 5, 10_000_000_001, closed_end, "overflow shares"),
            (9504, corp_a, 5, 250, closed_end, "add corp shares"),
            (9505, corp_a, 6, 999_880_000, closed_end, "missing item"),
            (9506, corp_a, 6, 880_001, closed_end, "foreign item"),
            (9507, corp_a, 6, 880_002, closed_end, "impounded item"),
            (9508, corp_a, 6, 880_003, closed_end, "fitted item"),
            (9509, corp_a, 6, 880_004, closed_end, "cargo item"),
            (9510, corp_a, 6, 880_005, closed_end, "lock sag1 item"),
            (9511, corp_a, 7, 880_004, closed_end, "unlock nonlocked"),
            (9512, corp_a, 7, 880_007, closed_end, "unlock unsafe loc"),
            (9513, corp_a, 7, 880_006, closed_end, "unlock valid locked"),
            (9514, corp_a, 0, char_foreign, closed_end, "ceo non-member"),
            (9515, corp_a, 0, char_mem, closed_end, "ceo valid change"),
        ];
        for (cid, co, vt, pr, et, txt) in cases {
            snap.corp_vote_cases.insert(cid, mk_case(cid, co, vt, pr, et, txt));
            snap.corp_vote_ballots.insert(
                (cid, char_ceo),
                eve_db::CorpVoteBallotRecord {
                    case_id: cid,
                    voter_id: char_ceo,
                    option_id: 1,
                    weight: 500,
                    voted_at_unix: now - 10,
                },
            );
        }
        snap.corp_vote_cases.insert(9516, mk_case(9516, corp_a, 5, 10, open_end, "open a"));
    });

    // InsertVote errors: foreign corp + zero shares
    assert_eq!(
        call_corp_gov(&ctx, &mut ceo_sess, "voteManager", "InsertVote", &[PyRep::Integer(9501), PyRep::Integer(1)], &kw)
            .unwrap_err()
            .code,
        "CorpAccessDenied"
    );
    assert_eq!(
        call_corp_gov(&ctx, &mut member_sess, "voteManager", "InsertVote", &[PyRep::Integer(9516), PyRep::Integer(1)], &kw)
            .unwrap_err()
            .code,
        "CorpAccessDenied"
    );

    // Non-director denied for vote_type != 0
    assert_eq!(
        call_corp_gov(&ctx, &mut member_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(9504)], &kw)
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    // Foreign corp vote rejected
    assert_eq!(
        call_corp_gov(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(9501)], &kw)
            .unwrap_err()
            .code,
        "CorpAccessDenied"
    );

    // Share creation guards + success updating existing shareholder
    assert_eq!(
        call_corp_gov(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(9502)], &kw)
            .unwrap_err()
            .code,
        "InvalidShareCount"
    );
    assert_eq!(
        call_corp_gov(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(9503)], &kw)
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert!(call_corp_gov(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(9504)], &kw).is_ok());

    // Item lock / unlock guards & successes
    assert_eq!(
        call_corp_gov(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(9505)], &kw).unwrap_err().code,
        "ItemNotFound"
    );
    for cid in [9506i64, 9508, 9509, 9511, 9512] {
        assert_eq!(
            call_corp_gov(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(cid)], &kw).unwrap_err().code,
            "ItemLocked"
        );
    }
    assert_eq!(
        call_corp_gov(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(9507)], &kw).unwrap_err().code,
        "ImpoundedItem"
    );
    assert!(call_corp_gov(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(9510)], &kw).is_ok());
    assert!(call_corp_gov(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(9513)], &kw).is_ok());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&880_005].flag, eve_db::InventoryFlag::Locked);
        assert_eq!(snap.items[&880_006].flag, eve_db::InventoryFlag::Hangar);
    });

    // CEO election non-member error & valid CEO transfer demoting old CEO u64::MAX -> 0
    assert_eq!(
        call_corp_gov(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(9514)], &kw)
            .unwrap_err()
            .code,
        "CandidateNotMember"
    );
    assert!(call_corp_gov(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(9515)], &kw).is_ok());
    ctx.db.with_snapshot(|snap| {
        let corp = &snap.corporations[&corp_a];
        assert_eq!(corp.ceo_id, char_mem);
        assert_eq!(corp.member_roles.get(&char_ceo).copied(), Some(0));
        assert_eq!(corp.member_roles.get(&char_mem).copied(), Some(u64::MAX));
    });
}

