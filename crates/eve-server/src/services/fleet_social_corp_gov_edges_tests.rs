//! Behavioral edge-case tests for `fleet_social_corp_gov.rs`.

#![forbid(unsafe_code)]

use super::handle_corp_gov;
use crate::router::ServerContext;
use eve_db::{
    corp_roles, CorpVoteCaseRecord, CorpWalletDivisionRecord, CorporationRecord, IskCents,
    ServerClock, VirtualServerClock, WorldRepository, WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn call_gov(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    handle_corp_gov(ctx, sess, svc, method, &args, &HashMap::new())
        .unwrap_or_else(|| panic!("unhandled {svc}.{method}"))
}

fn setup_corp_sandbox() -> (
    ServerContext,
    Arc<VirtualServerClock>,
    SessionState,
    SessionState,
    i64,
    i64,
    i64,
) {
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000));
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    ctx.set_clock(clock.clone());

    let corp_id = 98_400_001_i64;
    let ceo_id = 91_400_001_i64;
    let member_id = 91_400_002_i64;

    ctx.db.with_mut_snapshot(|snap: &mut WorldStateSnapshot| {
        let tpl = snap.characters.values().next().unwrap().clone();
        for (cid, name, bal_isk) in [
            (ceo_id, "Corp CEO", 500_000_000_i64),
            (member_id, "Corp Line", 50_000_000_i64),
        ] {
            let mut ch = tpl.clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.corporation_id = corp_id;
            ch.station_id = Some(60_003_760);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(bal_isk);
            snap.characters.insert(cid, ch);
        }

        let mut divs =
            CorporationRecord::default_wallet_divisions(IskCents::from_isk(1_000_000_000));
        divs[0] = CorpWalletDivisionRecord {
            division_id: 1,
            name: "Master".into(),
            balance: IskCents::from_isk(1_000_000_000),
        };
        divs[1] = CorpWalletDivisionRecord {
            division_id: 2,
            name: "Dividends".into(),
            balance: IskCents::from_isk(10_000_000),
        };
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Gov Edges Corp".into(); __corp.ticker = "GVED".into(); __corp.ceo_id = ceo_id; __corp.alliance_id = None; __corp.tax_rate = 0.05; __corp.members = HashSet::from([ceo_id, member_id]); __corp.member_roles = HashMap::from([(ceo_id, u64::MAX), (member_id, 0u64)]); __corp.applications = HashMap::new(); __corp.wallet_divisions = divs; __corp.description = String::new(); __corp.url = String::new(); __corp.hq_station_id = Some(60_003_760); let __mw = IskCents::from_isk(1_000_000_000); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp },
        );
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.shares.insert(corp_id, 1000);
        gov.company_shares = 1000;
    });

    let mut ceo = SessionState::new(1);
    ceo.user_id = Some(ceo_id as u64);
    ceo.character_id = Some(ceo_id);
    ceo.corporation_id = Some(corp_id);
    ceo.corp_role = u64::MAX;
    ceo.station_id = Some(60_003_760);
    ceo.solar_system_id = Some(30_000_142);

    let mut member = SessionState::new(2);
    member.user_id = Some(member_id as u64);
    member.character_id = Some(member_id);
    member.corporation_id = Some(corp_id);
    member.corp_role = 0;
    member.station_id = Some(60_003_760);
    member.solar_system_id = Some(30_000_142);

    (ctx, clock, ceo, member, corp_id, ceo_id, member_id)
}

#[test]
fn shares_dividends_titles_resign_and_vote_validation_edges() {
    let (ctx, clock, mut ceo, mut member, corp_id, ceo_id, member_id) = setup_corp_sandbox();

    // 1. MoveCompanyShares validation errors
    assert_eq!(
        call_gov(
            &ctx,
            &mut member,
            "corpRegistry",
            "MoveCompanyShares",
            vec![PyRep::Integer(member_id), PyRep::Integer(100)],
        )
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "MoveCompanyShares",
            vec![
                PyRep::Integer(98_999_999),
                PyRep::Integer(member_id),
                PyRep::Integer(100),
            ],
        )
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "MoveCompanyShares",
            vec![PyRep::Integer(member_id), PyRep::Integer(0)],
        )
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "MoveCompanyShares",
            vec![PyRep::Integer(99_999_111), PyRep::Integer(100)],
        )
        .unwrap_err()
        .code,
        "TargetNotFound"
    );
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "MoveCompanyShares",
            vec![PyRep::Integer(member_id), PyRep::Integer(5_000)],
        )
        .unwrap_err()
        .code,
        "InsufficientShares"
    );

    // GetMemberIDsWithMoreThanAvgShares empty before member shares exist
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "GetMemberIDsWithMoreThanAvgShares",
            vec![],
        )
        .unwrap(),
        PyRep::List(vec![])
    );

    // Distribute 700 shares to CEO and 300 shares to member -> avg = 500 -> only CEO >= avg
    call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "MoveCompanyShares",
        vec![PyRep::Integer(ceo_id), PyRep::Integer(700)],
    )
    .unwrap();
    call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "MoveCompanyShares",
        vec![PyRep::Integer(member_id), PyRep::Integer(300)],
    )
    .unwrap();
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "GetMemberIDsWithMoreThanAvgShares",
            vec![],
        )
        .unwrap(),
        PyRep::List(vec![PyRep::Integer(ceo_id)])
    );

    // MovePrivateShares back to corp updates company_shares
    call_gov(
        &ctx,
        &mut member,
        "corpRegistry",
        "MovePrivateShares",
        vec![PyRep::Integer(corp_id), PyRep::Integer(100)],
    )
    .unwrap();
    assert_eq!(
        ctx.db
            .with_snapshot(|s| s.corp_governance.get(&corp_id).unwrap().company_shares),
        100
    );

    // 2. PayoutDividend validation & division 2 payout
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "PayoutDividend",
            vec![PyRep::Bool(true), PyRep::Real(1_000.0), PyRep::Integer(8)],
        )
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "PayoutDividend",
            vec![PyRep::Bool(true), PyRep::Real(-100.0), PyRep::Integer(1)],
        )
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_gov(
            &ctx,
            &mut member,
            "corpRegistry",
            "PayoutDividend",
            vec![PyRep::Bool(false), PyRep::Real(1_000.0), PyRep::Integer(1)],
        )
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    // Division 2 only has 10_000_000 ISK
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corpRegistry",
            "PayoutDividend",
            vec![PyRep::Bool(false), PyRep::Real(20_000_000.0), PyRep::Integer(2)],
        )
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );
    let div_res = call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "PayoutDividend",
        vec![PyRep::Bool(false), PyRep::Real(2_000_000.0), PyRep::Integer(2)],
    )
    .unwrap();
    assert_eq!(div_res.dict_get("recipientCount").and_then(|v| v.as_i64()), Some(2));
    ctx.db.with_snapshot(|s| {
        let corp = s.corporations.get(&corp_id).unwrap();
        assert_eq!(corp.division_balance(2).0, IskCents::from_isk(8_000_000).0);
        assert_eq!(corp.division_balance(1).0, IskCents::from_isk(1_000_000_000).0);
    });

    // 3. UpdateTitle strips ROLE_DIRECTOR and records pending delayed division role grants + role drops
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance
            .entry(corp_id)
            .or_default()
            .member_titles
            .insert(member_id, 1); // bit 0 = titleID 1
    });
    let delayed_role = 1u64 << 13;
    let upd_title = call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "UpdateTitle",
        vec![
            PyRep::Integer(1),
            PyRep::String("Quartermaster".into()),
            PyRep::Integer((corp_roles::ROLE_DIRECTOR | delayed_role) as i64),
        ],
    )
    .unwrap();
    assert_eq!(
        upd_title.dict_get("roles").and_then(|v| v.as_i64()),
        Some(delayed_role as i64)
    );
    ctx.db.with_snapshot(|snap| {
        let gov = snap.corp_governance.get(&corp_id).unwrap();
        let pending = gov.pending_title_role_grants.get(&member_id).copied();
        assert_eq!(pending.map(|(b, _)| b), Some(delayed_role));
    });
    call_gov(&ctx, &mut ceo, "corpRegistry", "DeleteTitle", vec![PyRep::Integer(1)]).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(snap
            .corp_governance
            .get(&corp_id)
            .unwrap()
            .role_drop_timestamps
            .contains_key(&member_id));
    });

    // 4. ResignFromCEO non-CEO denied
    assert_eq!(
        call_gov(&ctx, &mut member, "corpRegistry", "ResignFromCEO", vec![])
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    // 5. Vote errors: VoteCaseNotFound, CorpAccessDenied (wrong corp & zero shares), VoteCaseClosed
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "voteManager",
            "InsertVote",
            vec![PyRep::Integer(999_777), PyRep::Integer(1)],
        )
        .unwrap_err()
        .code,
        "VoteCaseNotFound"
    );
    let now = clock.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_vote_cases.insert(
            5001,
            CorpVoteCaseRecord {
                case_id: 5001,
                corporation_id: corp_id,
                vote_type: 1, // non-CEO election
                text: "Open Vote".into(),
                description: "desc".into(),
                parameter: 0,
                start_ts: now,
                end_ts: now + 3_600,
                executed: false,
                options: vec![],
            },
        );
        snap.corp_vote_cases.insert(
            5002,
            CorpVoteCaseRecord {
                case_id: 5002,
                corporation_id: 98_999_999,
                vote_type: 1,
                text: "Other Corp Vote".into(),
                description: "desc".into(),
                parameter: 0,
                start_ts: now,
                end_ts: now + 3_600,
                executed: false,
                options: vec![],
            },
        );
    });
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "voteManager",
            "InsertVote",
            vec![PyRep::Integer(5002), PyRep::Integer(1)],
        )
        .unwrap_err()
        .code,
        "CorpAccessDenied"
    );
    // Non-director cannot activate non-CEO sanctioned action
    assert_eq!(
        call_gov(
            &ctx,
            &mut member,
            "voteManager",
            "ActivateSanctionedAction",
            vec![PyRep::Integer(5001)],
        )
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    // Advance clock past end_ts -> VoteCaseClosed
    clock.advance_secs(4_000);
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "voteManager",
            "InsertVote",
            vec![PyRep::Integer(5001), PyRep::Integer(1)],
        )
        .unwrap_err()
        .code,
        "VoteCaseClosed"
    );
}

#[test]
fn medals_bulletins_recruitment_ads_and_contacts_edge_cases() {
    let (ctx, clock, mut ceo, mut member, corp_id, _ceo_id, member_id) = setup_corp_sandbox();

    // 1. Medals lifecycle & error guards (CreateMedal, GiveMedalToCharacters, SetMedalStatus, queries)
    assert_eq!(
        call_gov(
            &ctx,
            &mut member,
            "corporationSvc",
            "CreateMedal",
            vec![PyRep::String("Denied".into()), PyRep::String("No comms".into())],
        )
        .unwrap_err()
        .message,
        "CrpAccessDenied"
    );
    let medal = call_gov(
        &ctx,
        &mut ceo,
        "corporationSvc",
        "CreateMedal",
        vec![
            PyRep::String("Fleet Star".into()),
            PyRep::String("Distinction".into()),
            PyRep::List(vec![PyRep::List(vec![
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::String("ribbon_gold".into()),
                PyRep::Integer(12),
            ])]),
        ],
    )
    .unwrap();
    let medal_id = medal.dict_get("medalID").and_then(|v| v.as_i64()).unwrap();

    // GiveMedalToCharacters validation: invalid medalID / unknown recipient / unknown medal
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corporationSvc",
            "GiveMedalToCharacters",
            vec![PyRep::Integer(0), PyRep::Integer(member_id)],
        )
        .unwrap_err()
        .message,
        "InvalidParameter"
    );
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corporationSvc",
            "GiveMedalToCharacters",
            vec![PyRep::Integer(medal_id), PyRep::Integer(99_999_888)],
        )
        .unwrap_err()
        .message,
        "CharNotFound"
    );
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corporationSvc",
            "GiveMedalToCharacters",
            vec![PyRep::Integer(999_999), PyRep::Integer(member_id)],
        )
        .unwrap_err()
        .message,
        "MedalNotFound"
    );

    call_gov(
        &ctx,
        &mut ceo,
        "corporationSvc",
        "GiveMedalToCharacters",
        vec![
            PyRep::Integer(medal_id),
            PyRep::List(vec![PyRep::Integer(member_id)]),
            PyRep::String("Saved the fleet".into()),
        ],
    )
    .unwrap();

    call_gov(
        &ctx,
        &mut member,
        "corporationSvc",
        "SetMedalStatus",
        vec![PyRep::Integer(medal_id), PyRep::Integer(3)],
    )
    .unwrap();

    assert!(call_gov(&ctx, &mut ceo, "corporationSvc", "GetAllCorpMedals", vec![PyRep::Integer(corp_id)]).is_ok());
    assert!(call_gov(&ctx, &mut ceo, "corporationSvc", "GetMedalDetails", vec![PyRep::Integer(medal_id)]).is_ok());
    assert!(call_gov(&ctx, &mut member, "corporationSvc", "GetMedalsReceived", vec![PyRep::Integer(member_id)]).is_ok());
    assert!(call_gov(&ctx, &mut ceo, "corporationSvc", "GetRecipientsOfMedal", vec![PyRep::Integer(medal_id)]).is_ok());

    // 2. Bulletins: AddBulletin, UpdateBulletinOrder, GetBulletins, DeleteBulletin
    let b1 = call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "AddBulletin",
        vec![PyRep::String("Rules".into()), PyRep::String("Read rules".into())],
    )
    .unwrap()
    .dict_get("bulletinID")
    .and_then(|v| v.as_i64())
    .unwrap();
    let b2 = call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "AddBulletin",
        vec![PyRep::String("Ops".into()), PyRep::String("Ping times".into())],
    )
    .unwrap()
    .dict_get("bulletinID")
    .and_then(|v| v.as_i64())
    .unwrap();
    call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "UpdateBulletinOrder",
        vec![PyRep::List(vec![PyRep::Integer(b2), PyRep::Integer(b1)])],
    )
    .unwrap();
    assert!(call_gov(&ctx, &mut member, "corpRegistry", "GetBulletins", vec![]).is_ok());
    call_gov(&ctx, &mut ceo, "corpRegistry", "DeleteBulletin", vec![PyRep::Integer(b1)]).unwrap();

    // 3. Recruitment ads role guard, not-found errors, GetRecruiters, and expired filtering in SearchCorpAds
    assert_eq!(
        call_gov(
            &ctx,
            &mut member,
            "corpRecProxy",
            "CreateRecruitmentAd",
            vec![PyRep::String("Nope".into()), PyRep::String("No role".into())],
        )
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_gov(
            &ctx,
            &mut ceo,
            "corpRecProxy",
            "UpdateRecruitmentAd",
            vec![PyRep::Integer(999_555), PyRep::Integer(7), PyRep::String("X".into())],
        )
        .unwrap_err()
        .code,
        "RecruitmentAdNotFound"
    );
    let ad = call_gov(
        &ctx,
        &mut ceo,
        "corpRecProxy",
        "CreateRecruitmentAd",
        vec![
            PyRep::String("Active Ad".into()),
            PyRep::String("Join".into()),
            PyRep::Integer(1),
            PyRep::Integer(1), // 1 day expiry
        ],
    )
    .unwrap();
    let ad_id = ad.dict_get("adID").and_then(|v| v.as_i64()).unwrap();
    let recruiters = call_gov(
        &ctx,
        &mut member,
        "corpRecProxy",
        "GetRecruiters",
        vec![PyRep::Integer(corp_id), PyRep::Integer(ad_id)],
    )
    .unwrap();
    assert_eq!(recruiters, PyRep::List(vec![PyRep::Integer(ceo.character_id.unwrap())]));

    // Advance past 1-day expiry -> SearchCorpAds filters out expired ad, while GetRecruitmentAd still finds it
    clock.advance_secs(86_401);
    let search = call_gov(&ctx, &mut member, "corpRecProxy", "SearchCorpAds", vec![PyRep::Integer(1)])
        .unwrap();
    let PyRep::Rowset { rows: search_rows, .. } = search else { panic!("expected rowset") };
    assert!(search_rows.is_empty());

    let by_id = call_gov(
        &ctx,
        &mut member,
        "corpRecProxy",
        "GetRecruitmentAd",
        vec![PyRep::Integer(ad_id)],
    )
    .unwrap();
    let PyRep::Rowset { rows: ad_rows, .. } = by_id else { panic!("expected rowset") };
    assert_eq!(ad_rows.len(), 1);

    // 4. RemoveCorporateContacts non-diplomat denied vs batch removal
    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters.values().next().unwrap().clone();
        c.character_id = 99_111_222;
        c.user_id = 99_111_222;
        c.name = "ContactTarget".into();
        snap.characters.insert(99_111_222, c);
    });
    call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "AddCorporateContact",
        vec![PyRep::Integer(99_111_222), PyRep::Real(5.0)],
    )
    .unwrap();
    assert_eq!(
        call_gov(
            &ctx,
            &mut member,
            "corpRegistry",
            "RemoveCorporateContacts",
            vec![PyRep::Integer(99_111_222)],
        )
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    call_gov(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "RemoveCorporateContacts",
        vec![PyRep::List(vec![PyRep::Integer(99_111_222)])],
    )
    .unwrap();
    let PyRep::Rowset { rows: contact_rows, .. } =
        call_gov(&ctx, &mut member, "corpRegistry", "GetCorporateContacts", vec![]).unwrap()
    else {
        panic!("expected rowset");
    };
    assert!(contact_rows.is_empty());
}
