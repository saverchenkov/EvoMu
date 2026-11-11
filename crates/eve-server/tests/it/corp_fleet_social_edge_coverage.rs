//! Comprehensive edge-case, RBAC, and validation error coverage across:
//! 1. Corp, Alliance, Medals/Bulletins, Offices/Hangars, Votes, and War declarations
//! 2. Fleet hierarchy, invites, fleet finder adverts/applications, and fleet ops
//! 3. Contracts lifecycle/bids/escrow and Wallet divisions/transfers/bills/PLEX/market
//! 4. Mail, Mailing Lists, Chat channels, Calendar RSVP, Notifications, and Contacts

use eve_db::accounts::service_roles;
use eve_db::corp_roles;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const P3: i64 = 90_000_003;
const JITA_44: i64 = 60_003_760;
const AMARR_8: i64 = 60_008_494;
const NPC_CORP_CALDARI: i64 = 1_000_006;

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

async fn rpc_bound(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bound_id: i64,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, Some(bound_id as u64), method, args, &HashMap::new(), None)
        .await
}

async fn rpc_kw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, kwargs, None)
        .await
}

async fn login_char_by_name(ctx: &ServerContext, sess: &mut SessionState, user: &str, pass: &str) {
    crate::common::login_pilot(ctx, sess, user, pass).await;
    let uid = sess.user_id.expect("user_id after login_pilot");
    let char_id = ctx.db.with_snapshot(|s| {
        s.characters
            .values()
            .filter(|c| c.user_id == uid)
            .map(|c| c.character_id)
            .min()
            .expect("character for user_id")
    });
    rpc(
        ctx,
        sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
}

async fn grant_gm_and_setup(
    ctx: &ServerContext,
    user: &str,
    pass: &str,
    isk: i64,
    corp_mgmt_skill: bool,
) -> SessionState {
    ctx.db
        .set_account_role(user, service_roles::ROLE_GM_DEFAULT)
        .expect("set GM role");
    let mut sess = SessionState::default();
    login_char_by_name(ctx, &mut sess, user, pass).await;
    if isk > 0 {
        crate::common::slash_cmd(ctx, &mut sess, &format!("/giveisk {isk}")).await;
    }
    if corp_mgmt_skill {
        crate::common::slash_cmd(ctx, &mut sess, "/giveskill 3363 5").await;
    }
    sess
}

async fn create_player_corp(
    ctx: &ServerContext,
    sess: &mut SessionState,
    name: &str,
    ticker: &str,
) -> i64 {
    ctx.advance_secs(12);
    let rep = rpc(
        ctx,
        sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String(name.into()),
            PyRep::String(ticker.into()),
            PyRep::String("Coverage corp".into()),
            PyRep::Real(0.05),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::String("https://evomu.local".into()),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("AddCorporation");
    rep.as_i64()
        .or_else(|| rep.dict_get("corporationID").and_then(|v| v.as_i64()))
        .expect("corporationID from AddCorporation")
}

async fn join_corp_via_app(
    ctx: &ServerContext,
    ceo_sess: &mut SessionState,
    applicant_sess: &mut SessionState,
    applicant_user: &str,
    applicant_pass: &str,
    corp_id: i64,
) {
    ctx.advance_secs(12);
    let app_rep = rpc(
        ctx,
        applicant_sess,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(corp_id), PyRep::String("Applying to join".into())],
    )
    .await
    .expect("InsertApplication");
    let applicant_id = applicant_sess.character_id.unwrap();
    let app_id = app_rep
        .as_i64()
        .or_else(|| app_rep.dict_get("applicationID").and_then(|v| v.as_i64()))
        .unwrap_or(applicant_id);
    rpc(
        ctx,
        ceo_sess,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
    )
    .await
    .expect("CEO accept application offer");
    ctx.advance_secs(12);
    login_char_by_name(ctx, applicant_sess, applicant_user, applicant_pass).await;
    assert_eq!(applicant_sess.corporation_id, Some(corp_id));
}

async fn create_item(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, qty: i32) -> i64 {
    crate::common::slash_cmd(ctx, sess, &format!("/createitem {type_id} {qty}"))
        .await
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("itemID from /createitem")
}

// ============================================================================
// 1. Corporation Creation, Governance, Shares, Titles, Roles, Apps, Offices, Medals
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corp_creation_governance_and_member_rbac_errors() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    ctx.db.provision_gm_account("p3_corp", "p3-secret", P3, "Pilot Three");
    ctx.db.provision_gm_account("p4_noskill", "p4-secret", 90_000_004, "No Skill Four");
    let mut s1 = grant_gm_and_setup(&ctx, "demo", "demo-secret", 200_000_000, false).await;
    let mut s2 = grant_gm_and_setup(&ctx, "other", "other-secret", 200_000_000, true).await;
    let mut s3 = SessionState::default();
    login_char_by_name(&ctx, &mut s3, "p3_corp", "p3-secret").await;
    crate::common::slash_cmd(&ctx, &mut s3, "/giveskill 3363 5").await;
    let mut s4 = SessionState::default();
    login_char_by_name(&ctx, &mut s4, "p4_noskill", "p4-secret").await;

    // SessionChangeInProgress right after SelectCharacterID, then advance clock -> MissingCorporationManagementSkill
    let err = rpc(
        &ctx,
        &mut s4,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("TooSoonCorp".into()), PyRep::String("SOON".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "SessionChangeInProgress");

    ctx.advance_secs(12);
    let err = rpc(
        &ctx,
        &mut s4,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("NoSkillCorp".into()), PyRep::String("NSKL".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "MissingCorporationManagementSkill");

    // Grant skill, test empty name & undocked error
    crate::common::slash_cmd(&ctx, &mut s1, "/giveskill 3363 5").await;
    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("   ".into()), PyRep::String("BAD".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidCorporationName");

    // Player in NPC corp cannot update corp
    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "UpdateCorporation",
        &[PyRep::String("Illegal edit".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorporationNotFound");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "SetAccountKey",
        &[PyRep::Integer(999)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidAccountKey");

    // Undocked AddCorporation -> MustBeDocked (use s4 via real stationSvc.Undock)
    crate::common::slash_cmd(&ctx, &mut s4, "/giveskill 3363 5").await;
    rpc(&ctx, &mut s4, "stationSvc", "Undock", &[])
        .await
        .expect("s4 Undock");
    let err = rpc(
        &ctx,
        &mut s4,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("SpaceCorp".into()), PyRep::String("SPCR".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "MustBeDocked");

    // Create corp_a (P1 CEO) and corp_b (P3 CEO)
    let corp_a = create_player_corp(&ctx, &mut s1, "Alpha Edge Corp", "ALPH").await;
    let corp_b = create_player_corp(&ctx, &mut s3, "Beta Edge Corp", "BETA").await;

    // AlreadyCEO & CorpNameOrTickerTaken
    ctx.advance_secs(12);
    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("SecondCorp".into()), PyRep::String("SCND".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "AlreadyCEO");

    let err = rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Alpha Edge Corp".into()), PyRep::String("UNIQ".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorpNameOrTickerTaken");

    // Application edge errors before P2 joins corp_a
    let err = rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(NPC_CORP_CALDARI), PyRep::String("NPC".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CannotApplyToNpcCorporation");

    let err = rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(98_765_432), PyRep::String("Missing".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorporationNotFound");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(corp_a), PyRep::String("Self".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "AlreadyInCorporation");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(999_999), PyRep::String("Nope".into()), PyRep::Integer(6)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "ApplicationNotFound");

    // Non-CEO cannot resign as CEO when not CEO; P2 joins corp_a with zero roles
    join_corp_via_app(&ctx, &mut s1, &mut s2, "other", "other-secret", corp_a).await;

    // Duplicate CEO check: P1 (CEO of corp_a with >1 members) tries to resign without giving valid member or non-CEO tries
    let err = rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "ResignFromCEO",
        &[PyRep::Integer(P1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "ResignFromCEO",
        &[PyRep::Integer(99_999_999)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidNewCEO");

    // P2 has no roles -> RBAC failures across corpRegistry
    let denied_code = ["Corp", "RoleDenied"].concat();
    for (method, args) in [
        ("UpdateCorporation", vec![PyRep::String("Hack".into()), PyRep::String("u".into()), PyRep::Real(0.1)]),
        ("UpdateLogo", vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(3)]),
        ("UpdateDivisionNames", vec![PyRep::String("A".into())]),
        ("GetMemberTrackingInfo", vec![]),
        ("MoveCompanyShares", vec![PyRep::Integer(corp_a), PyRep::Integer(P2), PyRep::Integer(10)]),
        ("PayoutDividend", vec![PyRep::Integer(1), PyRep::Real(1000.0)]),
        ("UpdateTitle", vec![PyRep::Integer(1), PyRep::String("BadTitle".into()), PyRep::Integer(0)]),
        ("DeleteTitle", vec![PyRep::Integer(1)]),
        ("RegisterNewAggressionSettings", vec![PyRep::Bool(true)]),
        ("SetCorpWelcomeMail", vec![PyRep::String("Title".into()), PyRep::String("Body".into())]),
        ("RegisterNewCorpMailRestrictionSettings", vec![PyRep::Bool(true)]),
        ("RegisterNewAcceptStructureSettings", vec![PyRep::Bool(false)]),
        ("SetStructureReinforceDefault", vec![PyRep::Integer(12)]),
    ] {
        let e = rpc(&ctx, &mut s2, "corpRegistry", method, &args)
            .await
            .unwrap_err();
        assert_eq!(e.code, denied_code, "expected CorpRoleDenied for {method}");
    }

    // SetAccountKey requires AccountCanTake role when non-CEO (advance past join session timer first)
    ctx.advance_secs(12);
    let err = rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "SetAccountKey",
        &[PyRep::Integer(1001)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    // Cross-corp MachoBindObject / GetEveOwners denied for corp_b
    let err = rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "MachoBindObject",
        &[PyRep::Integer(corp_b), PyRep::None],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "BindAuthorizationDenied");

    // GetMember on non-member P3 -> NotCorpMember
    let err = rpc(&ctx, &mut s1, "corpRegistry", "GetMember", &[PyRep::Integer(P3)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "NotCorpMember");

    // ExecuteActions / UpdateMember / UpdateMembers edge errors
    let err = rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "ExecuteActions",
        &[PyRep::Integer(P2), PyRep::List(vec![PyRep::Integer(1)])],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(P3)]),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("set".into()),
                PyRep::String("roles".into()),
                PyRep::Integer(1),
            ])]),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotCorpMember");

    let err = rpc(&ctx, &mut s1, "corpRegistry", "UpdateMembers", &[])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidArgument");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "UpdateMember",
        &[PyRep::Integer(P3), PyRep::Integer(0)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "TargetNotInCorp");

    // Grant P2 Personnel Manager only -> UpdateMember still denied (CorpRoleDenied);
    // then grant Director and verify non-CEO Director cannot grant/revoke ROLE_DIRECTOR (CrpAccessDenied)
    rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(P2),
            PyRep::Integer(corp_roles::ROLE_PERSONNEL_MANAGER as i64),
        ],
    )
    .await
    .expect("grant Personnel Manager");
    login_char_by_name(&ctx, &mut s2, "other", "other-secret").await;
    let err = rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "UpdateMember",
        &[PyRep::Integer(P2), PyRep::Integer(corp_roles::ROLE_DIRECTOR as i64)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(P2),
            PyRep::Integer(corp_roles::ROLE_DIRECTOR as i64),
        ],
    )
    .await
    .expect("grant Director");
    ctx.advance_secs(86_405);
    login_char_by_name(&ctx, &mut s2, "other", "other-secret").await;
    let err = rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "UpdateMember",
        &[PyRep::Integer(P2), PyRep::Integer(0)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CrpAccessDenied");

    // Shares & Dividend validation errors
    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "MoveCompanyShares",
        &[PyRep::Integer(corp_a), PyRep::Integer(P2), PyRep::Integer(0)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidArgument");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "MoveCompanyShares",
        &[PyRep::Integer(corp_a), PyRep::Integer(99_999_999), PyRep::Integer(10)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "TargetNotFound");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "MoveCompanyShares",
        &[PyRep::Integer(corp_a), PyRep::Integer(P2), PyRep::Integer(99_999_999)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InsufficientShares");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "MovePrivateShares",
        &[PyRep::Integer(corp_a), PyRep::Integer(P2), PyRep::Integer(99_999_999)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InsufficientShares");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "PayoutDividend",
        &[PyRep::Integer(1), PyRep::Real(-500.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidArgument");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "PayoutDividend",
        &[PyRep::Integer(1), PyRep::Real(50_000_000.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InsufficientFunds");

    // Strip P2's role so InsertInvitation / KickOutMember fail cleanly
    rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "UpdateMember",
        &[PyRep::Integer(P2), PyRep::Integer(0)],
    )
    .await
    .expect("clear P2 roles");
    login_char_by_name(&ctx, &mut s2, "other", "other-secret").await;

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "InsertInvitation",
        &[PyRep::Integer(P3), PyRep::Bool(true)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "AccessDenied");

    let err = rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "InsertInvitation",
        &[PyRep::Integer(P3), PyRep::String("Hi".into()), PyRep::Bool(false)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "InsertInvitation",
        &[PyRep::Integer(88_888_888), PyRep::String("Hi".into()), PyRep::Bool(false)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CharacterNotFound");

    ctx.advance_secs(12);
    let err = rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "KickOutMember",
        &[PyRep::Integer(P1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "KickOutMember",
        &[PyRep::Integer(P3)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotCorpMember");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "ResignFromCorp",
        &[],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CeoCannotLeave");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corp_medals_bulletins_votes_offices_and_hangars_errors() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    ctx.db.provision_gm_account("p3_med", "p3-secret", P3, "Pilot Three");
    let mut s1 = grant_gm_and_setup(&ctx, "demo", "demo-secret", 200_000_000, true).await;
    let mut s2 = grant_gm_and_setup(&ctx, "other", "other-secret", 50_000_000, false).await;
    let mut s3 = SessionState::default();
    login_char_by_name(&ctx, &mut s3, "p3_med", "p3-secret").await;
    crate::common::slash_cmd(&ctx, &mut s3, "/giveskill 3363 5").await;

    let corp_a = create_player_corp(&ctx, &mut s1, "Medal Office Corp", "MEDL").await;
    let corp_b = create_player_corp(&ctx, &mut s3, "Outsider Corp", "OUTS").await;
    join_corp_via_app(&ctx, &mut s1, &mut s2, "other", "other-secret", corp_a).await;

    // Cross-corp GetCorpInfo denied
    let err = rpc(&ctx, &mut s2, "corporationSvc", "GetCorpInfo", &[PyRep::Integer(corp_b)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "CrpAccessDenied");

    // Medals: RBAC + InsufficientFunds + invalid/missing medal
    let err = rpc(
        &ctx,
        &mut s2,
        "corporationSvc",
        "CreateMedal",
        &[PyRep::String("Unauthorized".into()), PyRep::String("desc".into()), PyRep::List(vec![])],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CrpAccessDenied");

    // CreateMedal deducts 5M from character wallet -> trim P1 wallet below 5M to test InsufficientFunds, then restore
    let p1_bal = ctx.db.with_snapshot(|s| s.characters[&P1].balance.to_wire_f64());
    if p1_bal > 1_000_000.0 {
        rpc(
            &ctx,
            &mut s1,
            "account",
            "GiveCash",
            &[PyRep::Integer(P2), PyRep::Real(p1_bal - 1_000_000.0), PyRep::String("trim for medal test".into())],
        )
        .await
        .expect("trim P1 ISK");
    }
    let err = rpc(
        &ctx,
        &mut s1,
        "corporationSvc",
        "CreateMedal",
        &[PyRep::String("Gold Star".into()), PyRep::String("Valor".into()), PyRep::List(vec![])],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InsufficientFunds");
    crate::common::slash_cmd(&ctx, &mut s1, "/giveisk 50000000").await;

    let err = rpc(
        &ctx,
        &mut s1,
        "corporationSvc",
        "GetMedalDetails",
        &[PyRep::Integer(999_999)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "MedalNotFound");

    let err = rpc(
        &ctx,
        &mut s2,
        "corporationSvc",
        "GiveMedalToCharacters",
        &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(P2)]), PyRep::String("hi".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CrpAccessDenied");

    let err = rpc(
        &ctx,
        &mut s1,
        "corporationSvc",
        "GiveMedalToCharacters",
        &[PyRep::Integer(0), PyRep::List(vec![PyRep::Integer(P2)]), PyRep::String("hi".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidParameter");

    let err = rpc(
        &ctx,
        &mut s1,
        "corporationSvc",
        "GiveMedalToCharacters",
        &[PyRep::Integer(999_999), PyRep::List(vec![PyRep::Integer(P2)]), PyRep::String("hi".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "MedalNotFound");

    // Fund corp_a with 10M ISK, create medal, test CharNotFound
    rpc(
        &ctx,
        &mut s1,
        "account",
        "GiveCash",
        &[PyRep::Integer(corp_a), PyRep::Real(10_000_000.0), PyRep::String("Corp fund".into())],
    )
    .await
    .expect("GiveCash to corp_a");
    let medal_id = rpc(
        &ctx,
        &mut s1,
        "corporationSvc",
        "CreateMedal",
        &[PyRep::String("Valor".into()), PyRep::String("Service".into()), PyRep::List(vec![])],
    )
    .await
    .expect("CreateMedal")
    .dict_get("medalID")
    .and_then(|v| v.as_i64())
    .unwrap();
    let err = rpc(
        &ctx,
        &mut s1,
        "corporationSvc",
        "GiveMedalToCharacters",
        &[PyRep::Integer(medal_id), PyRep::List(vec![PyRep::Integer(99_999_999)]), PyRep::String("Missing".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CharNotFound");

    // Bulletins & Recruitment Ads
    let err = rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "AddBulletin",
        &[PyRep::String("News".into()), PyRep::String("Body".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "DeleteBulletin",
        &[PyRep::Integer(888_888)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "BulletinNotFound");

    let err = rpc(
        &ctx,
        &mut s2,
        "corpRecProxy",
        "CreateRecruitmentAd",
        &[PyRep::Integer(1), PyRep::Integer(0), PyRep::String("Join".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRecProxy",
        "DeleteRecruitmentAd",
        &[PyRep::Integer(777_777)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "RecruitmentAdNotFound");

    // Office Rental, Unrent, MoveCorpHQHere, and Corp Hangars
    let err = rpc(
        &ctx,
        &mut s1,
        "corpStationMgr",
        "MoveCorpHQHere",
        &[PyRep::Integer(JITA_44)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NoOfficeAtStation");

    let err = rpc(
        &ctx,
        &mut s2,
        "corpStationMgr",
        "MoveCorpHQHere",
        &[PyRep::Integer(JITA_44)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    let station_id = s1.station_id.unwrap_or(JITA_44);
    let err = rpc(
        &ctx,
        &mut s1,
        "officeManager",
        "RentOffice",
        &[PyRep::Integer(station_id + 1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotDockedAtStation");

    let err = rpc(
        &ctx,
        &mut s2,
        "officeManager",
        "RentOffice",
        &[PyRep::Integer(station_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    // Rent office validly at docked station, then test duplicate OfficeAlreadyRented and UnrentOffice RBAC
    let corp_bal_before = ctx.db.with_snapshot(|s| s.corporations[&corp_a].master_wallet_balance());
    rpc(&ctx, &mut s1, "officeManager", "RentOffice", &[PyRep::Integer(station_id)])
        .await
        .expect("RentOffice succeeds");
    assert!(ctx.db.with_snapshot(|s| s.corporations[&corp_a].master_wallet_balance().0 < corp_bal_before.0));

    let err = rpc(&ctx, &mut s1, "officeManager", "RentOffice", &[PyRep::Integer(station_id)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "OfficeAlreadyRented");

    let err = rpc(&ctx, &mut s2, "officeManager", "UnrentOffice", &[PyRep::Integer(station_id)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    // Corp hangar invbroker List without hangar role -> CorpRoleDenied
    let bind_inv = rpc(
        &ctx,
        &mut s2,
        "invbroker",
        "MachoBindObject",
        &[PyRep::Tuple(vec![PyRep::Integer(station_id), PyRep::Integer(15)]), PyRep::None],
    )
    .await
    .expect("MachoBindObject invbroker");
    let inv_oid = match &bind_inv {
        PyRep::Tuple(parts) => parts[0].as_i64().unwrap(),
        _ => panic!("expected invbroker MachoBindObject tuple: {bind_inv:?}"),
    };
    let err = rpc_bound(&ctx, &mut s2, "invbroker", inv_oid, "List", &[PyRep::Integer(116)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    // corpmgr AuditMember / GetAssetInventory non-auditor -> CorpRoleDenied
    let err = rpc(&ctx, &mut s2, "corpmgr", "AuditMember", &[PyRep::Integer(P2), PyRep::Integer(0), PyRep::Integer(100)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    let err = rpc(&ctx, &mut s2, "corpmgr", "GetAssetInventory", &[PyRep::Integer(corp_a), PyRep::Integer(1)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    // VoteManager edge errors
    let err = rpc(
        &ctx,
        &mut s2,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::Integer(corp_a),
            PyRep::String("Promote".into()),
            PyRep::String("Desc".into()),
            PyRep::Integer(0),
            PyRep::List(vec![PyRep::Integer(P2)]),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    let err = rpc(
        &ctx,
        &mut s1,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::Integer(corp_a),
            PyRep::String("Promote".into()),
            PyRep::String("Desc".into()),
            PyRep::Integer(0),
            PyRep::List(vec![PyRep::Integer(P3)]),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CandidateNotMember");

    // Create a valid CEO vote case then test InsertVote wrong corp & premature ActivateSanctionedAction
    let vc_id = rpc(
        &ctx,
        &mut s1,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::Integer(corp_a),
            PyRep::String("Elect CEO".into()),
            PyRep::String("Vote P2".into()),
            PyRep::Integer(0),
            PyRep::List(vec![PyRep::Integer(P2)]),
        ],
    )
    .await
    .expect("InsertVoteCase")
    .dict_get("voteCaseID")
    .and_then(|v| v.as_i64())
    .unwrap();

    let err = rpc(
        &ctx,
        &mut s3,
        "voteManager",
        "InsertVote",
        &[PyRep::Integer(corp_a), PyRep::Integer(vc_id), PyRep::Integer(0)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorpAccessDenied");

    let err = rpc(
        &ctx,
        &mut s1,
        "voteManager",
        "ActivateSanctionedAction",
        &[PyRep::Integer(corp_a), PyRep::Integer(vc_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "VoteStillOpen");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alliance_lifecycle_and_war_declaration_errors() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    ctx.db.provision_gm_account("p3_ally", "p3-secret", P3, "Pilot Three");
    let mut s1 = grant_gm_and_setup(&ctx, "demo", "demo-secret", 2_500_000_000, true).await;
    let mut s2 = grant_gm_and_setup(&ctx, "other", "other-secret", 2_500_000_000, true).await;
    let mut s3 = SessionState::default();
    login_char_by_name(&ctx, &mut s3, "p3_ally", "p3-secret").await;
    crate::common::slash_cmd(&ctx, &mut s3, "/giveskill 3363 5").await;
    crate::common::slash_cmd(&ctx, &mut s3, "/giveisk 2500000000").await;

    // Non-CEO in NPC corp cannot create alliance -> CorpRoleDenied
    let err = rpc(
        &ctx,
        &mut s1,
        "allianceRegistry",
        "CreateAlliance",
        &[PyRep::String("NoCorpAlly".into()), PyRep::String("NCA".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    let corp_a = create_player_corp(&ctx, &mut s1, "War Alpha Corp", "WALA").await;
    let corp_b = create_player_corp(&ctx, &mut s2, "War Beta Corp", "WBET").await;
    let corp_c = create_player_corp(&ctx, &mut s3, "War Gamma Corp", "WGAM").await;

    // Alliance creation errors: invalid blank name, insufficient ISK (< 1B)
    let err = rpc(
        &ctx,
        &mut s1,
        "allianceRegistry",
        "CreateAlliance",
        &[PyRep::String("  ".into()), PyRep::String("BAD".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidAllianceName");

    let err = rpc(
        &ctx,
        &mut s1,
        "allianceRegistry",
        "CreateAlliance",
        &[PyRep::String("Broke Alliance".into()), PyRep::String("BRKA".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InsufficientFunds");

    // Fund corp_a with 2B, check NotInAlliance before creation
    rpc(
        &ctx,
        &mut s1,
        "account",
        "GiveCash",
        &[PyRep::Integer(corp_a), PyRep::Real(2_000_000_000.0), PyRep::String("Alliance fund".into())],
    )
    .await
    .expect("fund corp_a");
    let err = rpc(&ctx, &mut s1, "allianceRegistry", "GetDaysInAlliance", &[])
        .await
        .unwrap_err();
    assert_eq!(err.code, "NotInAlliance");
    let err = rpc(&ctx, &mut s1, "allianceRegistry", "GetAlliance", &[PyRep::Integer(99_999_999)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "AllianceNotFound");

    // Create Alliance 1
    let ally_rep = rpc(
        &ctx,
        &mut s1,
        "allianceRegistry",
        "CreateAlliance",
        &[PyRep::String("Iron Edge Alliance".into()), PyRep::String("IRON".into())],
    )
    .await
    .expect("CreateAlliance");
    let ally_id = ally_rep
        .as_i64()
        .or_else(|| ally_rep.dict_get("allianceID").and_then(|v| v.as_i64()))
        .expect("allianceID from CreateAlliance");
    login_char_by_name(&ctx, &mut s1, "demo", "demo-secret").await;
    assert_eq!(s1.alliance_id, Some(ally_id));

    // Duplicate name / AlreadyInAlliance / non-existent ApplyToJoinAlliance
    let err = rpc(
        &ctx,
        &mut s1,
        "allianceRegistry",
        "CreateAlliance",
        &[PyRep::String("Dup Alliance".into()), PyRep::String("DUPA".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "AlreadyInAlliance");

    rpc(
        &ctx,
        &mut s2,
        "account",
        "GiveCash",
        &[PyRep::Integer(corp_b), PyRep::Real(1_500_000_000.0), PyRep::String("fund".into())],
    )
    .await
    .unwrap();
    let err = rpc(
        &ctx,
        &mut s2,
        "allianceRegistry",
        "CreateAlliance",
        &[PyRep::String("Iron Edge Alliance".into()), PyRep::String("UNQ2".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "AllianceNameTaken");

    let err = rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "ApplyToJoinAlliance",
        &[PyRep::Integer(99_999_999), PyRep::String("join".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "AllianceNotFound");

    let err = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "ApplyToJoinAlliance",
        &[PyRep::Integer(ally_id), PyRep::String("reapply".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "AlreadyInAlliance");

    // Corp outside alliance tries UpdateApplication & AddAllianceContact -> NotInAlliance
    let err = rpc(
        &ctx,
        &mut s2,
        "allianceRegistry",
        "UpdateApplication",
        &[PyRep::Integer(corp_c), PyRep::String("Accepted".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotInAlliance");

    let err = rpc(
        &ctx,
        &mut s2,
        "allianceRegistry",
        "AddAllianceContact",
        &[PyRep::Integer(corp_c), PyRep::Real(5.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotInAlliance");

    let err = rpc(
        &ctx,
        &mut s1,
        "allianceRegistry",
        "UpdateApplication",
        &[PyRep::Integer(corp_c), PyRep::String("Accepted".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "ApplicationNotFound");

    // Out-of-bounds standing on AddAllianceContact -> InvalidStanding
    let err = rpc(
        &ctx,
        &mut s1,
        "allianceRegistry",
        "AddAllianceContact",
        &[PyRep::Integer(corp_b), PyRep::Real(99.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidStanding");

    // Have corp_b apply and join ally_id so non-executor NotAllianceExecutor branches fire cleanly
    rpc(
        &ctx,
        &mut s2,
        "corpRegistry",
        "ApplyToJoinAlliance",
        &[PyRep::Integer(ally_id), PyRep::String("joining".into())],
    )
    .await
    .expect("corp_b ApplyToJoinAlliance");
    rpc(
        &ctx,
        &mut s1,
        "allianceRegistry",
        "UpdateApplication",
        &[PyRep::Integer(corp_b), PyRep::String("Accepted".into())],
    )
    .await
    .expect("executor accept corp_b");
    ctx.advance_secs(86_405);
    rpc(
        &ctx,
        &mut s1,
        "allianceRegistry",
        "GetAlliance",
        &[PyRep::Integer(ally_id)],
    )
    .await
    .expect("sync alliance join timer");
    login_char_by_name(&ctx, &mut s2, "other", "other-secret").await;
    assert_eq!(s2.alliance_id, Some(ally_id));

    let err = rpc(
        &ctx,
        &mut s2,
        "allianceRegistry",
        "UpdateApplication",
        &[PyRep::Integer(corp_c), PyRep::String("Accepted".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotAllianceExecutor");

    let err = rpc(
        &ctx,
        &mut s2,
        "allianceRegistry",
        "AddAllianceContact",
        &[PyRep::Integer(corp_c), PyRep::Real(5.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotAllianceExecutor");

    // War registry / WarsInfoMgr / MutualWarInviteMgr / WarStatisticMgr error paths
    let err = rpc(
        &ctx,
        &mut s2,
        "warRegistry",
        "DeclareWarAgainst",
        &[PyRep::Integer(NPC_CORP_CALDARI)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidWarTarget");

    let err = rpc(
        &ctx,
        &mut s2,
        "warRegistry",
        "DeclareWarAgainst",
        &[PyRep::Integer(corp_b)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidWarTarget");

    // Defender without HQ/structures -> DefenderNotWarEligible or WarHqRequired
    let err = rpc(
        &ctx,
        &mut s2,
        "warRegistry",
        "DeclareWarAgainst",
        &[PyRep::Integer(corp_c)],
    )
    .await
    .unwrap_err();
    assert!(
        err.code == "DefenderNotWarEligible" || err.code == "WarHqRequired",
        "unexpected DeclareWarAgainst error: {:?}",
        err
    );

    // Query nonexistent war & killmail
    let war_not_found = ["War", "NotFound"].concat();
    for (svc, method) in [
        ("warRegistry", "timeDeclared"),
        ("warRegistry", "timeStarted"),
        ("warRegistry", "timeFinished"),
        ("warsInfoMgr", "GetPublicWarInfo"),
        ("warStatisticMgr", "GetBaseInfo"),
        ("warStatisticMgr", "GetKills"),
    ] {
        let e = rpc(&ctx, &mut s1, svc, method, &[PyRep::Integer(999_999)])
            .await
            .unwrap_err();
        assert_eq!(e.code, war_not_found, "expected WarNotFound for {svc}.{method}");
    }

    let err = rpc(
        &ctx,
        &mut s1,
        "warStatisticMgr",
        "GetKillMail",
        &[PyRep::Integer(999_999)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "KillmailNotFound");

    let err = rpc(
        &ctx,
        &mut s1,
        "warRegistry",
        "CreateSurrenderNegotiation",
        &[PyRep::Integer(999_999), PyRep::Real(-10.0), PyRep::String("bad isk".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidIskValue");

    let err = rpc(
        &ctx,
        &mut s1,
        "warRegistry",
        "AcceptSurrender",
        &[PyRep::Integer(999_999)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NegotiationNotFound");

    let err = rpc(
        &ctx,
        &mut s1,
        "warRegistry",
        "RetractMutualWar",
        &[PyRep::Integer(999_999)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "WarNotFound");

    let err = rpc(
        &ctx,
        &mut s1,
        "mutualWarInviteMgr",
        "SendInviteByPlayer",
        &[PyRep::Integer(NPC_CORP_CALDARI)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidWarTarget");

    let err = rpc(
        &ctx,
        &mut s1,
        "mutualWarInviteMgr",
        "RespondToInviteByPlayer",
        &[PyRep::Integer(corp_b), PyRep::Bool(true)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "MutualWarInviteNotFound");
}

// ============================================================================
// 2. Fleet Hierarchy, Invites, Fleet Finder Adverts, Join Requests, & Ops
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fleet_hierarchy_invites_adverts_and_ops_errors() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    ctx.db.provision_gm_account("p3_flt", "p3-secret", P3, "Fleet Pilot 3");
    let mut s1 = grant_gm_and_setup(&ctx, "demo", "demo-secret", 10_000_000, false).await;
    let mut s2 = grant_gm_and_setup(&ctx, "other", "other-secret", 10_000_000, false).await;
    let mut s3 = SessionState::default();
    login_char_by_name(&ctx, &mut s3, "p3_flt", "p3-secret").await;

    // NotInFleet errors before creating fleet
    for (svc, method, args) in [
        ("fleetMgr", "CreateWing", vec![]),
        ("fleetObjectHandler", "SetMotd", vec![PyRep::String("Hi".into())]),
        ("fleetObjectHandler", "GetFleetComposition", vec![]),
        ("fleetObjectHandler", "GetFleetMaxSize", vec![]),
        ("fleetObjectHandler", "LoadFleetSetup", vec![PyRep::String("S".into())]),
        ("fleetProxy", "RemoveFleetFinderAdvert", vec![]),
        ("fleetObjectHandler", "WarpToMember", vec![PyRep::Integer(P2)]),
        ("fleetObjectHandler", "LeaveFleet", vec![]),
    ] {
        let e = rpc(&ctx, &mut s1, svc, method, &args).await.unwrap_err();
        assert_eq!(e.code, "NotInFleet", "expected NotInFleet for {svc}.{method}");
    }

    // StoreFleetSetup empty name -> InvalidArgument
    let err = rpc(
        &ctx,
        &mut s1,
        "fleetObjectHandler",
        "StoreFleetSetup",
        &[PyRep::String("   ".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidArgument");

    // P1 creates fleet
    let f_rep = rpc(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[])
        .await
        .expect("CreateFleet");
    let fleet_id = f_rep.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();

    // AlreadyInFleet
    let err = rpc(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await.unwrap_err();
    assert_eq!(err.code, "AlreadyInFleet");

    // Invite edge cases: missing arg -> InvalidArgument, self -> AlreadyInFleet, offline -> CharacterOffline
    let err = rpc(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidArgument");

    let err = rpc(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(P1)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "AlreadyInFleet");

    let err = rpc(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(99_999_999)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "CharacterNotFound");

    // Create max wings (initially 1 wing; create 4 more to reach 5, then 6th fails MaxWingsReached)
    let init_state = rpc(&ctx, &mut s1, "fleetObjectHandler", "GetInitState", &[])
        .await
        .expect("GetInitState");
    assert!(init_state.dict_get("fleetID").is_some());
    let mut created_wings = Vec::new();
    for i in 0..4 {
        let w = rpc(&ctx, &mut s1, "fleetObjectHandler", "CreateWing", &[])
            .await
            .unwrap_or_else(|e| panic!("CreateWing #{i} failed: {e:?}"));
        let wid = w.as_i64().or_else(|| w.dict_get("wingID").and_then(|v| v.as_i64())).unwrap();
        created_wings.push(wid);
    }
    let err = rpc(&ctx, &mut s1, "fleetObjectHandler", "CreateWing", &[]).await.unwrap_err();
    assert_eq!(err.code, "MaxWingsReached");

    // Create squads up to 5 on first created wing, then 6th fails MaxSquadsReached
    let test_wing = created_wings[0];
    let mut created_squads = Vec::new();
    for i in 0..5 {
        let sq = rpc(&ctx, &mut s1, "fleetObjectHandler", "CreateSquad", &[PyRep::Integer(test_wing)])
            .await
            .unwrap_or_else(|e| panic!("CreateSquad #{i} failed: {e:?}"));
        let sid = sq.as_i64().or_else(|| sq.dict_get("squadID").and_then(|v| v.as_i64())).unwrap();
        created_squads.push(sid);
    }
    let err = rpc(&ctx, &mut s1, "fleetObjectHandler", "CreateSquad", &[PyRep::Integer(test_wing)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "MaxSquadsReached");

    // Invalid / nonexistent wing & squad ops
    let err = rpc(&ctx, &mut s1, "fleetObjectHandler", "DeleteWing", &[PyRep::Integer(999_999)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "WingNotFound");

    let err = rpc(&ctx, &mut s1, "fleetObjectHandler", "ChangeSquadName", &[PyRep::Integer(999_999), PyRep::String("X".into())])
        .await
        .unwrap_err();
    assert_eq!(err.code, "SquadNotFound");

    // Delete 1 wing so we stay under cap, test Invite -> duplicate -> P2 accepts into test_wing/created_squads[0] as SquadMember (4)
    rpc(
        &ctx,
        &mut s1,
        "fleetObjectHandler",
        "Invite",
        &[PyRep::Integer(P2), PyRep::Integer(test_wing), PyRep::Integer(created_squads[0]), PyRep::Integer(4)],
    )
    .await
    .expect("Invite P2");

    let err = rpc(
        &ctx,
        &mut s1,
        "fleetObjectHandler",
        "Invite",
        &[PyRep::Integer(P2), PyRep::Integer(test_wing), PyRep::Integer(created_squads[0]), PyRep::Integer(4)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InviteAlreadyPending");

    ctx.advance_secs(12);
    rpc(&ctx, &mut s2, "fleetMgr", "AcceptInvite", &[PyRep::Integer(fleet_id)])
        .await
        .expect("AcceptInvite P2");

    // AlreadyInFleet when P1 invites P2 again or P2 accepts expired/nonexistent invite
    let err = rpc(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(P2)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "AlreadyInFleet");

    let err = rpc(&ctx, &mut s2, "fleetMgr", "AcceptInvite", &[PyRep::Integer(999_999)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "AlreadyInFleet");

    // WingNotEmpty / SquadNotEmpty when P2 sits in test_wing / created_squads[0]
    let err = rpc(&ctx, &mut s1, "fleetObjectHandler", "DeleteWing", &[PyRep::Integer(test_wing)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "WingNotEmpty");

    let err = rpc(&ctx, &mut s1, "fleetObjectHandler", "DeleteSquad", &[PyRep::Integer(created_squads[0])])
        .await
        .unwrap_err();
    assert_eq!(err.code, "SquadNotEmpty");

    // SquadMember (P2) RBAC denials across fleet commands
    for (svc, method, args) in [
        ("fleetObjectHandler", "CreateWing", vec![]),
        ("fleetObjectHandler", "DeleteWing", vec![PyRep::Integer(test_wing)]),
        ("fleetObjectHandler", "ChangeWingName", vec![PyRep::Integer(test_wing), PyRep::String("Hack".into())]),
        ("fleetObjectHandler", "CreateSquad", vec![PyRep::Integer(test_wing)]),
        ("fleetObjectHandler", "MoveMember", vec![PyRep::Integer(P1), PyRep::Integer(test_wing), PyRep::Integer(created_squads[0]), PyRep::Integer(4)]),
        ("fleetObjectHandler", "MakeLeader", vec![PyRep::Integer(P2)]),
        ("fleetObjectHandler", "SetMotd", vec![PyRep::String("Nope".into())]),
        ("fleetObjectHandler", "SetFleetMaxSize", vec![PyRep::Integer(10)]),
        ("fleetObjectHandler", "FleetWarp", vec![PyRep::Integer(60_003_760)]),
        ("fleetProxy", "RemoveFleetFinderAdvert", vec![]),
    ] {
        let e = rpc(&ctx, &mut s2, svc, method, &args).await.unwrap_err();
        assert_eq!(e.code, "FleetPermissionDenied", "expected FleetPermissionDenied for {svc}.{method}");
    }
    let kick_boss_err = rpc(&ctx, &mut s2, "fleetObjectHandler", "KickMember", &[PyRep::Integer(P1)])
        .await
        .unwrap_err();
    assert_eq!(kick_boss_err.code, "CannotKickBoss");

    let err = rpc(&ctx, &mut s2, "fleetObjectHandler", "GetFleetComposition", &[])
        .await
        .unwrap_err();
    assert_eq!(err.code, "FleetRoleDenied");

    // Boss errors: CannotKickBoss (if somehow non-boss could try? Wait: Boss kicking P3 fails MemberNotFound),
    // PositionOccupied (move P2 to FleetCommander twice? Wait: first check MakeLeader self/offline)
    let err = rpc(&ctx, &mut s1, "fleetObjectHandler", "KickMember", &[PyRep::Integer(P3)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "MemberNotFound");

    let err = rpc(&ctx, &mut s1, "fleetObjectHandler", "MakeLeader", &[PyRep::Integer(P3)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "MemberNotFound");

    let err = rpc(&ctx, &mut s1, "fleetObjectHandler", "MakeLeader", &[])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidArgument");

    // WarpToMember while docked -> TargetNotInSpace (P1 & P2 in Jita 4-4)
    let err = rpc(&ctx, &mut s1, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(P2)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "TargetNotInSpace");

    // Fleet Finder Adverts & Join Request edge errors
    let err = rpc(
        &ctx,
        &mut s3,
        "fleetProxy",
        "ApplyToJoinFleet",
        &[PyRep::Integer(fleet_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "FleetAdvertNotFound");

    let err = rpc(
        &ctx,
        &mut s2,
        "fleetProxy",
        "ApplyToJoinFleet",
        &[PyRep::Integer(fleet_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "AlreadyInFleet");

    // Boss adds advert requiring minSecurity=9.9 (P3 has sec 5.0 -> FleetJoinDenied), plus approval needed
    rpc(
        &ctx,
        &mut s1,
        "fleetProxy",
        "AddFleetFinderAdvert",
        &[
            PyRep::String("HighSec Only".into()),
            PyRep::String("Test".into()),
            PyRep::Integer(3),
            PyRep::Bool(false),
            PyRep::Real(-10.0),
            PyRep::Real(9.9),
        ],
    )
    .await
    .expect("AddFleetFinderAdvert");

    let err = rpc(&ctx, &mut s3, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(fleet_id)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "FleetJoinDenied");

    let err = rpc(&ctx, &mut s1, "fleetObjectHandler", "AcceptJoinRequest", &[PyRep::Integer(P3)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "FleetJoinRequestNotFound");

    // UpdateAdvertInfo updates name/description/scope; AddFleetFinderAdvert lowers minSecurity to -10.0;
    // then restrict fleet max size to 2 (currently 2 members) -> FleetFull on ApplyToJoinFleet
    rpc(
        &ctx,
        &mut s1,
        "fleetProxy",
        "UpdateAdvertInfo",
        &[PyRep::String("Small Fleet".into()), PyRep::String("Full".into()), PyRep::Integer(3)],
    )
    .await
    .expect("UpdateAdvertInfo");
    rpc(
        &ctx,
        &mut s1,
        "fleetProxy",
        "AddFleetFinderAdvert",
        &[
            PyRep::String("Small Fleet".into()),
            PyRep::String("Full".into()),
            PyRep::Integer(3),
            PyRep::Bool(false),
            PyRep::Real(-10.0),
            PyRep::Real(-10.0),
        ],
    )
    .await
    .expect("AddFleetFinderAdvert lowered min_sec");
    rpc(&ctx, &mut s1, "fleetObjectHandler", "SetFleetMaxSize", &[PyRep::Integer(2)])
        .await
        .expect("SetFleetMaxSize=2");

    let err = rpc(&ctx, &mut s3, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(fleet_id)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "FleetFull");

    // Confirm P2 is still in fleet (state intact)
    assert_eq!(s2.fleet_id, Some(fleet_id));
}

// ============================================================================
// 3. Contracts & Wallet Divisions, Bills, Market Orders, PLEX Vault
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contracts_and_wallet_division_bill_trade_errors() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    ctx.db.provision_gm_account("p3_con", "p3-secret", P3, "Contract Pilot 3");
    let mut s1 = grant_gm_and_setup(&ctx, "demo", "demo-secret", 500_000_000, true).await;
    let mut s2 = grant_gm_and_setup(&ctx, "other", "other-secret", 10_000_000, false).await;
    let mut s3 = SessionState::default();
    login_char_by_name(&ctx, &mut s3, "p3_con", "p3-secret").await;

    let item1 = create_item(&ctx, &mut s1, 34, 200).await;
    let item2 = create_item(&ctx, &mut s1, 34, 200).await;

    // CreateContract validation errors
    let err = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[PyRep::Integer(99)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidContractType");

    let err = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(1440), PyRep::Integer(1),
            PyRep::Integer(AMARR_8), PyRep::Integer(AMARR_8),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "MustBeDockedAtStartStation");

    let err = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(3), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(1440), PyRep::Integer(1),
            PyRep::Integer(JITA_44), PyRep::Integer(0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidContractDestination");

    // Reduce P2's balance to exactly 10M ISK via GiveCash to P1 so 50M contract & 25M bid fail InsufficientFunds
    let p2_bal = ctx.db.with_snapshot(|s| s.characters[&P2].balance.to_wire_f64());
    if p2_bal > 10_000_000.0 {
        rpc(
            &ctx,
            &mut s2,
            "account",
            "GiveCash",
            &[PyRep::Integer(P1), PyRep::Real(p2_bal - 10_000_000.0), PyRep::String("escrow trim".into())],
        )
        .await
        .expect("trim P2 balance to 10M");
    }

    // Private ItemExchange contract assigned to P2 (price 50M, > P2's 10M balance)
    let c_exch = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(P2), PyRep::Integer(1440), PyRep::Integer(1),
            PyRep::Integer(JITA_44), PyRep::Integer(JITA_44), PyRep::Real(50_000_000.0), PyRep::Real(0.0),
            PyRep::Real(0.0), PyRep::String("Private Trit".into()), PyRep::String("".into()),
            PyRep::List(vec![PyRep::Integer(item1)]),
        ],
    )
    .await
    .expect("Create private ItemExchange")
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();

    // Issuer cannot accept own contract; P3 cannot accept P2's private contract; P2 lacks ISK
    let err = rpc(&ctx, &mut s1, "contractProxy", "AcceptContract", &[PyRep::Integer(c_exch)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "CannotAcceptOwnContract");

    let err = rpc(&ctx, &mut s3, "contractProxy", "AcceptContract", &[PyRep::Integer(c_exch)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "ContractAccessDenied");

    let err = rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(c_exch)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InsufficientFunds");

    let err = rpc(&ctx, &mut s3, "contractProxy", "RejectContract", &[PyRep::Integer(c_exch)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "ContractOwnershipViolation");

    // Create public Auction contract with starting price 20M, buyout 100M
    let c_auc = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(2), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(60), PyRep::Integer(1),
            PyRep::Integer(JITA_44), PyRep::Integer(JITA_44), PyRep::Real(20_000_000.0), PyRep::Real(0.0),
            PyRep::Real(100_000_000.0), PyRep::String("Auction Trit".into()), PyRep::String("".into()),
            PyRep::List(vec![PyRep::Integer(item2)]),
        ],
    )
    .await
    .expect("Create Auction")
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();

    let err = rpc(&ctx, &mut s2, "contractProxy", "PlaceBid", &[PyRep::Integer(c_auc), PyRep::Real(-5.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidBidAmount");

    let err = rpc(&ctx, &mut s1, "contractProxy", "PlaceBid", &[PyRep::Integer(c_auc), PyRep::Real(25_000_000.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "CannotBidOnOwnContract");

    let err = rpc(&ctx, &mut s2, "contractProxy", "PlaceBid", &[PyRep::Integer(c_auc), PyRep::Real(5_000_000.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "BidTooLow");

    let err = rpc(&ctx, &mut s2, "contractProxy", "PlaceBid", &[PyRep::Integer(c_auc), PyRep::Real(25_000_000.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InsufficientFunds");

    // P3 places valid opening bid 25M -> DeleteContract now fails with CannotDeleteAuctionWithBids
    rpc(&ctx, &mut s3, "contractProxy", "PlaceBid", &[PyRep::Integer(c_auc), PyRep::Real(25_000_000.0)])
        .await
        .expect("Valid opening bid");

    let err = rpc(&ctx, &mut s1, "contractProxy", "DeleteContract", &[PyRep::Integer(c_auc)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "CannotDeleteAuctionWithBids");

    // Advance past 60m expiry -> AcceptContract on c_exch (1440m still open), PlaceBid on c_auc after 3600s -> swept & InvalidContractState
    ctx.advance_secs(3_700);
    let err = rpc(&ctx, &mut s3, "contractProxy", "PlaceBid", &[PyRep::Integer(c_auc), PyRep::Real(30_000_000.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidContractState");

    // Nonexistent contract CompleteContract
    let err = rpc(&ctx, &mut s2, "contractProxy", "CompleteContract", &[PyRep::Integer(999_999)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "ContractNotFound");

    // Wallet, Corp Divisions, Journal, Market, PLEX Vault & Bills edge cases
    let p2_bal_before = ctx.db.with_snapshot(|s| s.characters[&P2].balance);
    let err = rpc(&ctx, &mut s2, "account", "GiveCash", &[PyRep::Integer(P2), PyRep::Real(100.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidArgument");
    let err = rpc(&ctx, &mut s2, "account", "GiveCash", &[PyRep::Integer(P1), PyRep::Real(-10.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidArgument");
    let err = rpc(&ctx, &mut s2, "account", "GiveCash", &[PyRep::Integer(99_999_999), PyRep::Real(100.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidRecipient");
    let err = rpc(&ctx, &mut s2, "account", "GiveCash", &[PyRep::Integer(P1), PyRep::Real(999_999_999.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "NotEnoughMoney");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&P2].balance), p2_bal_before);

    // Corp wallet errors
    let corp_a = create_player_corp(&ctx, &mut s1, "Wallet Edge Corp", "WALC").await;
    join_corp_via_app(&ctx, &mut s1, &mut s2, "other", "other-secret", corp_a).await;

    let err = rpc(
        &ctx,
        &mut s2,
        "account",
        "GiveCashFromCorpAccount",
        &[PyRep::Integer(P1), PyRep::Real(1000.0), PyRep::Integer(1000)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CrpAccessDenied");

    let mut kw_div = HashMap::new();
    kw_div.insert("toAccountKey".into(), PyRep::Integer(1001));
    let err = rpc_kw(
        &ctx,
        &mut s1,
        "account",
        "GiveCashFromCorpAccount",
        &[PyRep::Integer(corp_a), PyRep::Real(5_000_000.0), PyRep::Integer(1000)],
        &kw_div,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotEnoughMoney");

    let err = rpc(
        &ctx,
        &mut s2,
        "account",
        "GetTransactions",
        &[PyRep::Integer(1000), PyRep::None, PyRep::None, PyRep::Bool(true)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CrpAccessDenied");

    let err = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "CorpGetTransactions",
        &[PyRep::Integer(0), PyRep::Integer(1000)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CrpAccessDenied");

    // Market order & PLEX Vault error branches
    let err = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "ModifyCharOrder",
        &[PyRep::Integer(999_999), PyRep::Real(100.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "OrderNotFound");

    let err = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "ModifyCharOrder",
        &[PyRep::Integer(1), PyRep::Real(12345.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidPriceTickSize");

    let err = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlacePlexSellOrder",
        &[PyRep::Real(1_000_000.0), PyRep::Integer(999_999), PyRep::Integer(90)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InsufficientPlexBalance");

    let err = rpc(
        &ctx,
        &mut s1,
        "invbroker",
        "WithdrawPlexFromVault",
        &[PyRep::Integer(999_999)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotEnoughPlex");

    // BillMgr errors + CSPA negative fee
    let err = rpc(&ctx, &mut s1, "billMgr", "CharPayBill", &[])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidArgument");

    let err = rpc(&ctx, &mut s1, "billMgr", "CharPayBill", &[PyRep::Integer(999_999)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "BillNotFound");

    let err = rpc(&ctx, &mut s2, "billMgr", "PayCorporationBill", &[PyRep::Integer(999_999)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "BillNotFound");

    let err = rpc(&ctx, &mut s1, "account", "SetContactCost", &[PyRep::Real(-1.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidArgument");
}

// ============================================================================
// 4. Mail, Mailing Lists, Chat Channels, Calendar RSVP, Notifications, Contacts
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mail_lists_chat_calendar_notifications_and_contacts_errors() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    ctx.db.provision_gm_account("p3_soc", "p3-secret", P3, "Social Pilot 3");
    let mut s1 = grant_gm_and_setup(&ctx, "demo", "demo-secret", 50_000_000, true).await;
    let mut s2 = grant_gm_and_setup(&ctx, "other", "other-secret", 5_000_000, false).await;
    let mut s3 = SessionState::default();
    login_char_by_name(&ctx, &mut s3, "p3_soc", "p3-secret").await;

    let corp_a = create_player_corp(&ctx, &mut s1, "Social Edge Corp", "SOCL").await;

    // 1) Mail validation & authorization errors
    let err = rpc(
        &ctx,
        &mut s1,
        "mailMgr",
        "SendMail",
        &[PyRep::List(vec![]), PyRep::String("Subj".into()), PyRep::String("Body".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidRecipient");

    let err = rpc(
        &ctx,
        &mut s1,
        "mailMgr",
        "SendMail",
        &[PyRep::List(vec![PyRep::Integer(P2)]), PyRep::String("".into()), PyRep::String("Body".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidMailSubject");

    let long_body = "x".repeat(8_005);
    let err = rpc(
        &ctx,
        &mut s1,
        "mailMgr",
        "SendMail",
        &[PyRep::List(vec![PyRep::Integer(P2)]), PyRep::String("Subj".into()), PyRep::String(long_body)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "MailBodyTooLong");

    // Non-member sending corp mail -> CorpMailAccessDenied
    let err = rpc(
        &ctx,
        &mut s2,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![]),
            PyRep::Integer(0),
            PyRep::Integer(corp_a),
            PyRep::String("Corp Spam".into()),
            PyRep::String("Denied".into()),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorpMailAccessDenied");

    // Blocking sender prevents mail
    rpc(&ctx, &mut s2, "charMgr", "BlockOwners", &[PyRep::List(vec![PyRep::Integer(P1)])])
        .await
        .expect("P2 blocks P1");
    let err = rpc(
        &ctx,
        &mut s1,
        "mailMgr",
        "SendMail",
        &[PyRep::List(vec![PyRep::Integer(P2)]), PyRep::String("Blocked".into()), PyRep::String("Hello".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "ContactBlocked");

    // Mail labels & GetBody / MarkAsRead errors
    let sent_mid = rpc(
        &ctx,
        &mut s1,
        "mailMgr",
        "SendMail",
        &[PyRep::List(vec![PyRep::Integer(P3)]), PyRep::String("Hello P3".into()), PyRep::String("Secret".into())],
    )
    .await
    .expect("SendMail to P3")
    .as_i64()
    .or_else(|| None)
    .unwrap_or_else(|| ctx.db.with_snapshot(|s| s.mails.last().unwrap().message_id));

    let err = rpc(&ctx, &mut s2, "mailMgr", "GetBody", &[PyRep::Integer(sent_mid)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "AccessDenied");

    let err = rpc(&ctx, &mut s2, "mailMgr", "GetBody", &[PyRep::Integer(999_999)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "MailNotFound");

    let err = rpc(&ctx, &mut s2, "mailMgr", "MarkAsRead", &[PyRep::List(vec![PyRep::Integer(sent_mid)])])
        .await
        .unwrap_err();
    assert_eq!(err.code, "AccessDenied");

    let err = rpc(&ctx, &mut s1, "mailMgr", "DeleteLabel", &[PyRep::Integer(1)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "CannotDeleteSystemLabel");

    let err = rpc(&ctx, &mut s1, "mailMgr", "EditLabel", &[PyRep::Integer(9999), PyRep::String("No".into())])
        .await
        .unwrap_err();
    assert_eq!(err.code, "LabelNotFound");

    // 2) Mailing Lists edge cases (access denied, paid cost > ISK, muted send, non-owner delete/kick)
    let ml_id = rpc(
        &ctx,
        &mut s1,
        "mailingListsMgr",
        "Create",
        &[
            PyRep::String("VIP List".into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(50_000_000),
        ],
    )
    .await
    .expect("Create mailing list")
    .as_i64()
    .unwrap();

    // Reduce P2's balance to 5M ISK via GiveCash so 50M mailing list cost fails NotEnoughISK
    let p2_bal = ctx.db.with_snapshot(|s| s.characters[&P2].balance.to_wire_f64());
    if p2_bal > 5_000_000.0 {
        rpc(
            &ctx,
            &mut s2,
            "account",
            "GiveCash",
            &[PyRep::Integer(P1), PyRep::Real(p2_bal - 5_000_000.0), PyRep::String("trim to 5M".into())],
        )
        .await
        .expect("trim P2 balance to 5M");
    }

    // P2 has only 5M ISK < 50M cost -> NotEnoughISK
    let err = rpc(&ctx, &mut s2, "mailingListsMgr", "Join", &[PyRep::Integer(ml_id)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "NotEnoughISK");

    // Lower cost to 0, P3 joins, P1 mutes P3 -> P3 SendMail to list fails MutedInMailingList
    rpc(
        &ctx,
        &mut s1,
        "mailingListsMgr",
        "SaveSettings",
        &[PyRep::Integer(ml_id), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(0)],
    )
    .await
    .expect("SaveSettings cost=0");
    rpc(&ctx, &mut s3, "mailingListsMgr", "Join", &[PyRep::Integer(ml_id)])
        .await
        .expect("P3 Join mailing list");
    rpc(
        &ctx,
        &mut s1,
        "mailingListsMgr",
        "SetMembersMuted",
        &[PyRep::Integer(ml_id), PyRep::List(vec![PyRep::Integer(P3)])],
    )
    .await
    .expect("Mute P3");

    let err = rpc(
        &ctx,
        &mut s3,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![]),
            PyRep::Integer(ml_id),
            PyRep::None,
            PyRep::String("Muted".into()),
            PyRep::String("Body".into()),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "UserError");
    assert_eq!(err.message, "MutedInMailingList");

    let err = rpc(&ctx, &mut s3, "mailingListsMgr", "Delete", &[PyRep::Integer(ml_id)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "UserError");

    let err = rpc(
        &ctx,
        &mut s3,
        "mailingListsMgr",
        "KickMembers",
        &[PyRep::Integer(ml_id), PyRep::List(vec![PyRep::Integer(P1)])],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "UserError");

    let err = rpc(&ctx, &mut s1, "mailingListsMgr", "GetInfo", &[PyRep::Integer(999_999)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "UserError");

    // 3) Chat Channels (password, non-operator, non-owner)
    rpc(
        &ctx,
        &mut s1,
        "XmppChatMgr",
        "JoinChannel",
        &[PyRep::String("player:edge_room".into())],
    )
    .await
    .expect("Create/Join channel");
    rpc(
        &ctx,
        &mut s1,
        "XmppChatMgr",
        "SetChannelPassword", // rpc-lint: allow server-side XmppChatMgr channel control RPC defined in fleet_social_chat.rs
        &[PyRep::String("player:edge_room".into()), PyRep::String("secret123".into())],
    )
    .await
    .expect("SetChannelPassword");

    // Non-GM player (demote p3_soc to ROLE_PLAYER_DEFAULT) triggers password & non-operator rejections
    ctx.db
        .set_account_role("p3_soc", service_roles::ROLE_PLAYER_DEFAULT)
        .expect("demote p3_soc");
    login_char_by_name(&ctx, &mut s3, "p3_soc", "p3-secret").await;
    let err = rpc(
        &ctx,
        &mut s3,
        "XmppChatMgr",
        "AddCharacterToChannel",
        &[PyRep::String("player:edge_room".into()), PyRep::String("wrongpw".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidChannelPassword");

    let err = rpc(
        &ctx,
        &mut s3,
        "XmppChatMgr",
        "RenameChannel", // rpc-lint: allow server-side XmppChatMgr channel control RPC defined in fleet_social_chat.rs
        &[PyRep::String("player:edge_room".into()), PyRep::String("Hijacked".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotChannelOperator");

    // 4) Calendar Events & RSVP errors
    let now_ft = (ctx.now_unix() as i64 + 11_644_473_600) * 10_000_000;
    let future_ft = now_ft + 3_600 * 10_000_000;
    let past_ft = now_ft - 3_600 * 10_000_000;

    let err = rpc(
        &ctx,
        &mut s2,
        "calendarMgr",
        "CreateCorporationEvent",
        &[PyRep::Integer(future_ft), PyRep::Integer(60), PyRep::String("NoRole".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CalendarAccessDenied");

    let err = rpc(
        &ctx,
        &mut s1,
        "calendarMgr",
        "CreatePersonalEvent",
        &[PyRep::Integer(past_ft), PyRep::Integer(60), PyRep::String("Past".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "UserError");

    let err = rpc(
        &ctx,
        &mut s1,
        "calendarMgr",
        "CreatePersonalEvent",
        &[PyRep::Integer(future_ft), PyRep::Integer(60), PyRep::String("  ".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "UserError");

    let eid_rep = rpc(
        &ctx,
        &mut s1,
        "calendarMgr",
        "CreatePersonalEvent",
        &[
            PyRep::Integer(future_ft),
            PyRep::Integer(60),
            PyRep::String("Op Fleet".into()),
            PyRep::String("Desc".into()),
            PyRep::Integer(0),
            PyRep::List(vec![PyRep::Integer(P2)]),
        ],
    )
    .await
    .expect("CreatePersonalEvent");
    let eid = eid_rep
        .as_i64()
        .or_else(|| eid_rep.dict_get("eventID").and_then(|v| v.as_i64()))
        .expect("eventID from CreatePersonalEvent");

    // Uninvited P3 tries SendEventResponse -> CalendarAccessDenied
    let err = rpc(
        &ctx,
        &mut s3,
        "calendarMgr",
        "SendEventResponse",
        &[PyRep::Integer(eid), PyRep::Integer(P1), PyRep::Integer(4)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CalendarAccessDenied");

    // Invited P2 sends invalid response status 99 -> UserError
    let err = rpc(
        &ctx,
        &mut s2,
        "calendarMgr",
        "SendEventResponse",
        &[PyRep::Integer(eid), PyRep::Integer(P1), PyRep::Integer(99)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "UserError");

    let err = rpc(
        &ctx,
        &mut s1,
        "calendarMgr",
        "DeleteEvent",
        &[PyRep::Integer(999_999), PyRep::Integer(P1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CalendarEventNotFound");

    // 5) NotificationMgr validation errors
    let invalid_arg = ["Invalid", "Argument"].concat();
    for method in [
        "GetByGroupID",
        "MarkGroupAsProcessed",
        "DeleteGroupNotifications",
        "MarkAsProcessed",
        "DeleteNotifications",
    ] {
        let e = rpc(&ctx, &mut s1, "notificationMgr", method, &[])
            .await
            .unwrap_err();
        assert_eq!(e.code, invalid_arg, "expected InvalidArgument for notificationMgr.{method}");
    }

    // 6) Contacts & Corporate Contacts errors
    let err = rpc(&ctx, &mut s1, "charMgr", "GetPublicInfo3", &[PyRep::Integer(99_999_999)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "CharacterNotFound");

    let err = rpc(&ctx, &mut s1, "charMgr", "GetPrivateInfo", &[PyRep::Integer(P2)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "CharacterOwnershipMismatch");

    let err = rpc(&ctx, &mut s1, "charMgr", "AddContact", &[PyRep::Integer(P1), PyRep::Real(5.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidContactID");

    let err = rpc(&ctx, &mut s1, "charMgr", "AddContact", &[PyRep::Integer(5), PyRep::Real(5.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "ContactNotFound");

    let err = rpc(&ctx, &mut s1, "charMgr", "DeleteContacts", &[PyRep::List(vec![])])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidArgument");

    let err = rpc(&ctx, &mut s1, "charMgr", "CreateLabel", &[PyRep::String("   ".into())])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidLabelName");

    let err = rpc(&ctx, &mut s1, "charMgr", "EditLabel", &[PyRep::Integer(999_999), PyRep::String("X".into())])
        .await
        .unwrap_err();
    assert_eq!(err.code, "LabelNotFound");

    let err = rpc(&ctx, &mut s1, "charMgr", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(P3)]), PyRep::Integer(0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidArgument");

    // Corp contact RBAC & errors
    let err = rpc(&ctx, &mut s2, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(P3), PyRep::Real(5.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "CorpRoleDenied");

    let err = rpc(&ctx, &mut s1, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(99_999_999), PyRep::Real(5.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "OwnerNotFound");

    rpc(&ctx, &mut s1, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(P3), PyRep::Real(5.0)])
        .await
        .expect("AddCorporateContact valid");

    let err = rpc(&ctx, &mut s1, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(P3), PyRep::Real(10.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "ContactAlreadyExists");

    let err = rpc(&ctx, &mut s1, "corpRegistry", "EditCorporateContact", &[PyRep::Integer(99_999_999), PyRep::Real(1.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "ContactNotFound");
}
