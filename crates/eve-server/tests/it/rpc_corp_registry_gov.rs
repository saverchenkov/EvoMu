//! Authentic state-backed integration tests for `corpRegistry` shares, dividends, titles, bulletins, contacts, and labels:
//! - Shares & dividends: `MoveCompanyShares`, `MovePrivateShares`, `GetShareholders`, `GetSharesByShareholder`, `GetMemberIDsWithMoreThanAvgShares`, `PayoutDividend`, `ResignFromCEO`, `GetNumberOfPotentialCEOs`
//! - Titles & bulletins: `UpdateTitle`, `UpdateTitles`, `GetTitles`, `DeleteTitle`, `AddBulletin`, `UpdateBulletinOrder`, `GetBulletins`, `DeleteBulletin`
//! - Contacts & labels: `AddCorporateContact`, `EditCorporateContact`, `EditContactsRelationshipID`, `RemoveCorporateContacts`, `GetCorporateContacts`, `CreateLabel`, `EditLabel`, `GetLabels`, `DeleteLabel`, `AssignLabels`, `RemoveLabels`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const PILOT_3: i64 = 90_000_003;

async fn select_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn field(rep: &PyRep, key: &str) -> PyRep {
    match rep {
        PyRep::KeyVal(map) => map.get(key).cloned().unwrap_or(PyRep::None),
        PyRep::Dict(entries) => entries
            .iter()
            .find_map(|(k, v)| (k.as_str() == Some(key)).then(|| v.clone()))
            .unwrap_or(PyRep::None),
        _ => PyRep::None,
    }
}

fn rowset_data(rep: &PyRep) -> (Vec<String>, Vec<Vec<PyRep>>) {
    match rep {
        PyRep::Rowset { columns, rows } => (columns.clone(), rows.clone()),
        _ => (Vec::new(), Vec::new()),
    }
}

fn set_corp_balance(ctx: &ServerContext, corp_id: i64, isk: f64) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ensure_wallet_divisions();
            corp.wallet_divisions[0].balance = IskCents::from_f64_isk(isk);
            corp.master_wallet = IskCents::from_f64_isk(isk);
        }
    });
}

#[tokio::test]
async fn test_corp_shares_dividends_and_ceo_transition() {
    // parity: feat:corporation.dividends
    // parity: feat:corporation.share-transfer
    // parity: rpc:corpRegistry.MoveCompanyShares
    // parity: rpc:corpRegistry.MovePrivateShares
    // parity: rpc:corpRegistry.GetShareholders
    // parity: rpc:corpRegistry.GetSharesByShareholder
    // parity: rpc:corpRegistry.GetMemberIDsWithMoreThanAvgShares
    // parity: rpc:corpRegistry.PayoutDividend
    // parity: rpc:corpRegistry.ResignFromCEO
    // parity: rpc:corpRegistry.GetNumberOfPotentialCEOs
    eve_testkit::attest(&[
        "feat:corporation.dividends",
        "feat:corporation.share-transfer",
        "rpc:corpRegistry.MoveCompanyShares",
        "rpc:corpRegistry.MovePrivateShares",
        "rpc:corpRegistry.GetShareholders",
        "rpc:corpRegistry.GetSharesByShareholder",
        "rpc:corpRegistry.GetMemberIDsWithMoreThanAvgShares",
        "rpc:corpRegistry.PayoutDividend",
        "rpc:corpRegistry.ResignFromCEO",
        "rpc:corpRegistry.GetNumberOfPotentialCEOs",
    ]);

    let ctx = ServerContext::new();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch3 = snap.characters.get(&PILOT_2).cloned().expect("PILOT_2");
        ch3.character_id = PILOT_3;
        ch3.user_id = 3;
        ch3.name = "Third Pilot".into();
        ch3.balance = IskCents(0);
        snap.characters.insert(PILOT_3, ch3);
    });
    let mut sess1 = select_pilot(&ctx, 61_001, "demo", "demo-secret", PILOT_1).await;

    let created = rpc(&ctx, &mut sess1, "corpRegistry", "AddCorporation", &[PyRep::String("Titan Holdings".into()), PyRep::String("TITAN".into()), PyRep::Real(0.05)]).await.expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().expect("corporationID");

    // 1. Initial state: AddCorporation sets 1000 shares in corporation itself (treasury)
    let init_corp_sh = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(corp_id), PyRep::Integer(corp_id)]).await.expect("GetSharesByShareholder corp");
    assert_eq!(rowset_data(&init_corp_sh).1[0][2], PyRep::Integer(1_000));

    // 1b. MoveCompanyShares: transfer company shares from corp treasury to PILOT_1 and PILOT_2
    let move_p1 = rpc(&ctx, &mut sess1, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_1), PyRep::Integer(350)]).await.expect("MoveCompanyShares PILOT_1");
    assert_eq!(field(&move_p1, "sharesTransferred"), PyRep::Integer(350));
    let move_corp = rpc(&ctx, &mut sess1, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_2), PyRep::Integer(200)]).await.expect("MoveCompanyShares PILOT_2");
    assert_eq!(field(&move_corp, "sharesTransferred"), PyRep::Integer(200));

    // 2. MovePrivateShares: transfer 50 shares from PILOT_1 to PILOT_3 (character-to-character)
    let move_priv = rpc(&ctx, &mut sess1, "corpRegistry", "MovePrivateShares", &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_3), PyRep::Integer(50)]).await.expect("MovePrivateShares PILOT_3");
    assert_eq!(field(&move_priv, "sharesTransferred"), PyRep::Integer(50));

    // 2b. MovePrivateShares: transfer 20 shares from PILOT_1 back to corp treasury (character-to-corp)
    let move_to_corp = rpc(&ctx, &mut sess1, "corpRegistry", "MovePrivateShares", &[PyRep::Integer(corp_id), PyRep::Integer(corp_id), PyRep::Integer(20)]).await.expect("MovePrivateShares corp");
    assert_eq!(field(&move_to_corp, "sharesTransferred"), PyRep::Integer(20));

    // Reject unknown recipient in MoveCompanyShares and MovePrivateShares
    let err_c_unknown = rpc(&ctx, &mut sess1, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(corp_id), PyRep::Integer(99_999_888), PyRep::Integer(10)]).await.unwrap_err();
    assert_eq!(err_c_unknown.code, "TargetNotFound");
    let err_p_unknown = rpc(&ctx, &mut sess1, "corpRegistry", "MovePrivateShares", &[PyRep::Integer(corp_id), PyRep::Integer(99_999_888), PyRep::Integer(10)]).await.unwrap_err();
    assert_eq!(err_p_unknown.code, "TargetNotFound");

    // 3. GetShareholders: 4 shareholders total; zero-share entries omitted
    ctx.db.with_mut_snapshot(|snap| { snap.corp_governance.entry(corp_id).or_default().shares.insert(999_998, 0); });
    let sh_res = rpc(&ctx, &mut sess1, "corpRegistry", "GetShareholders", &[PyRep::Integer(corp_id)]).await.expect("GetShareholders");
    let (s_cols, s_rows) = rowset_data(&sh_res);
    assert!(s_cols.contains(&"shareholderID".to_string()));
    assert_eq!(s_rows.len(), 4, "zero-share entry omitted");

    // 4. GetSharesByShareholder: verify exact holdings
    let by_sh_corp = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(corp_id), PyRep::Integer(corp_id)]).await.expect("GetSharesByShareholder corp");
    assert_eq!(rowset_data(&by_sh_corp).1[0][2], PyRep::Integer(470));
    let by_sh_p1 = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_1)]).await.expect("GetSharesByShareholder PILOT_1");
    assert_eq!(rowset_data(&by_sh_p1).1[0][2], PyRep::Integer(280));
    let by_sh = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_2)]).await.expect("GetSharesByShareholder PILOT_2");
    assert_eq!(rowset_data(&by_sh).1[0][2], PyRep::Integer(200));
    let by_sh_p3 = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_3)]).await.expect("GetSharesByShareholder PILOT_3");
    assert_eq!(rowset_data(&by_sh_p3).1[0][2], PyRep::Integer(50));
    let by_sh_none = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(corp_id), PyRep::Integer(999_999)]).await.expect("GetSharesByShareholder non-holder");
    assert!(rowset_data(&by_sh_none).1.is_empty());

    // 4b. GetSharesByShareholder 1-arg form (corpShares: bool / int)
    let by_sh_bf = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Bool(false)]).await.expect("GetSharesByShareholder false");
    assert_eq!(rowset_data(&by_sh_bf).1[0][2], PyRep::Integer(280));
    let by_sh_bt = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Bool(true)]).await.expect("GetSharesByShareholder true");
    assert_eq!(rowset_data(&by_sh_bt).1[0][2], PyRep::Integer(470));
    let by_sh_i0 = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(0)]).await.expect("GetSharesByShareholder 0");
    assert_eq!(rowset_data(&by_sh_i0).1[0][2], PyRep::Integer(280));
    let by_sh_i1 = rpc(&ctx, &mut sess1, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(1)]).await.expect("GetSharesByShareholder 1");
    assert_eq!(rowset_data(&by_sh_i1).1[0][2], PyRep::Integer(470));

    // 4c. Cross-corp GetSharesByShareholder: non-member sees their shares across corps
    let mut sess_p3 = SessionState::new(61_098);
    sess_p3.user_id = Some(3);
    sess_p3.character_id = Some(PILOT_3);
    sess_p3.corporation_id = Some(1000044);
    let by_sh_cross = rpc(&ctx, &mut sess_p3, "corpRegistry", "GetSharesByShareholder", &[PyRep::Bool(false)]).await.expect("cross-corp GetSharesByShareholder");
    assert_eq!(rowset_data(&by_sh_cross).1[0], vec![PyRep::Integer(PILOT_3), PyRep::Integer(corp_id), PyRep::Integer(50)]);

    // 5. GetMemberIDsWithMoreThanAvgShares (0 args; only PILOT_1 is member, avg = 280; non-members PILOT_2 and PILOT_3 excluded)
    let avg_sh = rpc(&ctx, &mut sess1, "corpRegistry", "GetMemberIDsWithMoreThanAvgShares", &[]).await.expect("GetMemberIDsWithMoreThanAvgShares");
    let PyRep::List(sh_list) = avg_sh else { panic!("expected list") };
    assert!(sh_list.contains(&PyRep::Integer(PILOT_1)));
    assert!(!sh_list.contains(&PyRep::Integer(PILOT_2)), "PILOT_2 is non-member shareholder; must be excluded");
    assert!(!sh_list.contains(&PyRep::Integer(PILOT_3)), "PILOT_3 is non-member shareholder; must be excluded");
    assert!(!sh_list.contains(&PyRep::Integer(corp_id)), "corp treasury must be excluded");

    // 5b. Share-weighted voting: verify vote weighting uses transferred share distribution (PILOT_2 has 200 shares)
    let opt_rows = vec![
        vec![PyRep::Integer(1), PyRep::String("Elect PILOT_2".into()), PyRep::Integer(PILOT_2)],
        vec![PyRep::Integer(2), PyRep::String("Reject".into()), PyRep::Integer(0)],
    ];
    let custom_options = PyRep::rowset(vec!["optionID", "optionText", "parameter"], opt_rows);
    let vc_res = rpc(&ctx, &mut sess1, "voteManager", "InsertVoteCase", &[
        PyRep::String("CEO Election".into()), PyRep::String("Vote for candidate".into()),
        PyRep::Integer(24), PyRep::Integer(1), PyRep::Integer(PILOT_2), custom_options,
    ]).await.expect("InsertVoteCase");
    let case_id = field(&vc_res, "voteCaseID").as_i64().expect("voteCaseID");

    let mut sess_p2 = SessionState::new(61_096);
    sess_p2.user_id = Some(2);
    sess_p2.character_id = Some(PILOT_2);
    sess_p2.corporation_id = Some(corp_id);
    let v_res = rpc(&ctx, &mut sess_p2, "voteManager", "InsertVote", &[PyRep::Integer(case_id), PyRep::Integer(1)]).await.expect("InsertVote");
    assert_eq!(field(&v_res, "weight"), PyRep::Integer(200), "vote weight must match shareholder's 200 shares");

    // 5c. Negative and zero share creation votes rejected
    let err_neg_shares = rpc(&ctx, &mut sess1, "voteManager", "InsertVoteCase", &[
        PyRep::String("Create Shares".into()), PyRep::String("Neg".into()), PyRep::Integer(5), PyRep::Integer(1), PyRep::Integer(-100),
    ]).await.unwrap_err();
    assert_eq!(err_neg_shares.code, "InvalidArgument");
    let err_zero_shares = rpc(&ctx, &mut sess1, "voteManager", "InsertVoteCase", &[
        PyRep::String("Create Shares".into()), PyRep::String("Zero".into()), PyRep::Integer(5), PyRep::Integer(1), PyRep::Integer(0),
    ]).await.unwrap_err();
    assert_eq!(err_zero_shares.code, "InvalidArgument");

    // 6. PayoutDividend: test V24 [payShareholders, payoutAmount] layout, wallet journaling, balances and error handling
    set_corp_balance(&ctx, corp_id, 10_000_000.0);
    let (p1_b0, p2_b0, p3_b0) = ctx.db.with_snapshot(|snap| (
        snap.characters.get(&PILOT_1).map(|c| c.balance.0).unwrap_or(0),
        snap.characters.get(&PILOT_2).map(|c| c.balance.0).unwrap_or(0),
        snap.characters.get(&PILOT_3).map(|c| c.balance.0).unwrap_or(0),
    ));

    // 6a. payShareholders=true: pays per share to external shareholders; corp division deducted only actual paid sum
    let div_res = rpc(&ctx, &mut sess1, "corpRegistry", "PayoutDividend", &[PyRep::Bool(true), PyRep::Real(1_000_000.0)]).await.expect("PayoutDividend per share");
    assert_eq!(field(&div_res, "corporationID"), PyRep::Integer(corp_id));
    assert_eq!(field(&div_res, "recipientCount"), PyRep::Integer(3));

    let (p1_b1, p2_b1, p3_b1, corp_b1) = ctx.db.with_snapshot(|snap| (
        snap.characters.get(&PILOT_1).map(|c| c.balance.0).unwrap_or(0),
        snap.characters.get(&PILOT_2).map(|c| c.balance.0).unwrap_or(0),
        snap.characters.get(&PILOT_3).map(|c| c.balance.0).unwrap_or(0),
        snap.corporations.get(&corp_id).map(|c| c.wallet_divisions[0].balance.0).unwrap_or(0),
    ));
    assert_eq!(p1_b1 - p1_b0, 28_000_000, "PILOT_1 credited 280,000 ISK for 280 shares");
    assert_eq!(p2_b1 - p2_b0, 20_000_000, "PILOT_2 credited 200,000 ISK for 200 shares");
    assert_eq!(p3_b1 - p3_b0, 5_000_000, "PILOT_3 credited 50,000 ISK for 50 shares");
    assert_eq!(corp_b1, 947_000_000, "Corp division 1 debited exactly 53,000,000 cents (actual paid)");

    let (has_corp_j, has_p1_j, has_p2_j, has_p3_j) = ctx.db.with_snapshot(|snap| {
        let c_j = snap.wallet_journal.iter().any(|j| j.character_id == corp_id && j.entry_type == "CorporationDividendPayout" && j.amount == eve_db::IskCents(-53_000_000) && j.balance_after == eve_db::IskCents(947_000_000));
        let p1_j = snap.wallet_journal.iter().any(|j| j.character_id == PILOT_1 && j.entry_type == "CorporationDividendPayment" && j.amount == eve_db::IskCents(28_000_000));
        let p2_j = snap.wallet_journal.iter().any(|j| j.character_id == PILOT_2 && j.entry_type == "CorporationDividendPayment" && j.amount == eve_db::IskCents(20_000_000));
        let p3_j = snap.wallet_journal.iter().any(|j| j.character_id == PILOT_3 && j.entry_type == "CorporationDividendPayment" && j.amount == eve_db::IskCents(5_000_000));
        (c_j, p1_j, p2_j, p3_j)
    });
    assert!(has_corp_j && has_p1_j && has_p2_j && has_p3_j, "both corp division deduction and shareholder additions journaled");

    // 6b. payShareholders=false: pays equally to all corporation members (PILOT_1 and PILOT_2)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_2) { ch.corporation_id = corp_id; }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(PILOT_2);
            corp.member_roles.insert(PILOT_2, 0);
        }
    });

    // 6b-2. GetMemberIDsWithMoreThanAvgShares after PILOT_2 joins: members are PILOT_1 (280) and PILOT_2 (200), avg = 240
    let avg_sh2 = rpc(&ctx, &mut sess1, "corpRegistry", "GetMemberIDsWithMoreThanAvgShares", &[]).await.expect("GetMemberIDsWithMoreThanAvgShares post-join");
    let PyRep::List(sh_list2) = avg_sh2 else { panic!("expected list") };
    assert!(sh_list2.contains(&PyRep::Integer(PILOT_1)), "PILOT_1 has 280 >= 240 avg member shares");
    assert!(!sh_list2.contains(&PyRep::Integer(PILOT_2)), "PILOT_2 has 200 < 240 avg member shares");
    assert!(!sh_list2.contains(&PyRep::Integer(PILOT_3)), "PILOT_3 is non-member shareholder");

    let div_member = rpc(&ctx, &mut sess1, "corpRegistry", "PayoutDividend", &[PyRep::Bool(false), PyRep::Real(500_000.0)]).await.expect("PayoutDividend per member");
    assert_eq!(field(&div_member, "recipientCount"), PyRep::Integer(2));
    let (p1_b2, p2_b2, corp_b2) = ctx.db.with_snapshot(|snap| (
        snap.characters.get(&PILOT_1).map(|c| c.balance.0).unwrap_or(0),
        snap.characters.get(&PILOT_2).map(|c| c.balance.0).unwrap_or(0),
        snap.corporations.get(&corp_id).map(|c| c.wallet_divisions[0].balance.0).unwrap_or(0),
    ));
    assert_eq!(p1_b2 - p1_b1, 25_000_000, "PILOT_1 credited 250,000 ISK member dividend");
    assert_eq!(p2_b2 - p2_b1, 25_000_000, "PILOT_2 credited 250,000 ISK member dividend");
    assert_eq!(corp_b2, 897_000_000, "Corp division 1 debited 50,000,000 cents for member payout");

    // 6c. Orphaned shareholder: uncredited orphaned shareholder ISK is NOT debited from corp wallet
    const ORPHAN_ID: i64 = 99_999_777;
    ctx.db.with_mut_snapshot(|snap| { snap.corp_governance.entry(corp_id).or_default().shares.insert(ORPHAN_ID, 100); });
    let corp_b_before_orphan = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].wallet_divisions[0].balance.0);
    let div_orphan = rpc(&ctx, &mut sess1, "corpRegistry", "PayoutDividend", &[PyRep::Bool(true), PyRep::Real(1_000_000.0)]).await.expect("PayoutDividend with orphan");
    assert_eq!(field(&div_orphan, "recipientCount"), PyRep::Integer(3));
    let corp_b_after_orphan = ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].wallet_divisions[0].balance.0);
    assert_eq!(corp_b_before_orphan - corp_b_after_orphan, 48_181_817, "Corp division debited only for sum of credited dividends; orphaned shareholder shares preserved in wallet");

    // 6d. Negative test: Insufficient funds fails with InsufficientFunds
    let err_funds = rpc(&ctx, &mut sess1, "corpRegistry", "PayoutDividend", &[PyRep::Bool(true), PyRep::Real(100_000_000_000.0)]).await.unwrap_err();
    assert_eq!(err_funds.code, "InsufficientFunds");

    // 6e. Negative test: Missing CEO/Director role fails with CorpRoleDenied
    let mut sess_unauth = SessionState::new(61_097);
    sess_unauth.user_id = Some(3);
    sess_unauth.character_id = Some(PILOT_3);
    sess_unauth.corporation_id = Some(corp_id);
    let err_role = rpc(&ctx, &mut sess_unauth, "corpRegistry", "PayoutDividend", &[PyRep::Bool(true), PyRep::Real(10_000.0)]).await.unwrap_err();
    assert_eq!(err_role.code, "CorpRoleDenied");

    // 6f. Negative test: Invalid division ID (< 1 or > 7) fails with InvalidArgument
    let err_div9 = rpc(&ctx, &mut sess1, "corpRegistry", "PayoutDividend", &[PyRep::Bool(true), PyRep::Real(1_000.0), PyRep::Integer(9)]).await.unwrap_err();
    assert_eq!(err_div9.code, "InvalidArgument");
    let err_div0 = rpc(&ctx, &mut sess1, "corpRegistry", "PayoutDividend", &[PyRep::Bool(true), PyRep::Real(1_000.0), PyRep::Integer(0)]).await.unwrap_err();
    assert_eq!(err_div0.code, "InvalidArgument");

    // 6g. Full-balance payout: drops wallet_divisions[0] and master_wallet to 0; ensure_wallet_divisions cannot restore funds
    set_corp_balance(&ctx, corp_id, 100_000.0);
    let div_full = rpc(&ctx, &mut sess1, "corpRegistry", "PayoutDividend", &[PyRep::Bool(false), PyRep::Real(100_000.0), PyRep::Integer(1)]).await.expect("PayoutDividend full");
    assert_eq!(field(&div_full, "recipientCount"), PyRep::Integer(2));
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        assert_eq!(corp.wallet_divisions[0].balance, eve_db::IskCents(0));
        assert_eq!(corp.master_wallet, eve_db::IskCents(0));
        corp.ensure_wallet_divisions();
        assert_eq!(corp.division_balance(1), eve_db::IskCents(0));
    });

    // 7. GetNumberOfPotentialCEOs
    let num_ceos = rpc(&ctx, &mut sess1, "corpRegistry", "GetNumberOfPotentialCEOs", &[PyRep::Integer(corp_id)]).await.expect("GetNumberOfPotentialCEOs");
    assert_eq!(num_ceos, PyRep::Integer(1));

    // 8. ResignFromCEO
    let resign = rpc(&ctx, &mut sess1, "corpRegistry", "ResignFromCEO", &[PyRep::Integer(PILOT_2)]).await.expect("ResignFromCEO");
    assert_eq!(field(&resign, "newCeoID"), PyRep::Integer(PILOT_2));

    // 9. Issue #43 security checks: non-director cannot MoveCompanyShares; cannot exceed treasury balance
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) { corp.member_roles.insert(PILOT_1, 0); }
    });
    let err_non_dir = rpc(&ctx, &mut sess1, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_3), PyRep::Integer(10)]).await.unwrap_err();
    assert_eq!(err_non_dir.code, "CorpRoleDenied");

    let mut sess2 = SessionState::new(61_099);
    sess2.user_id = Some(2);
    sess2.character_id = Some(PILOT_2);
    sess2.corporation_id = Some(corp_id);
    let err_insuff_corp = rpc(&ctx, &mut sess2, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_3), PyRep::Integer(10_000)]).await.unwrap_err();
    assert_eq!(err_insuff_corp.code, "InsufficientShares");

    let err_insuff_priv = rpc(&ctx, &mut sess2, "corpRegistry", "MovePrivateShares", &[PyRep::Integer(corp_id), PyRep::Integer(PILOT_3), PyRep::Integer(10_000)]).await.unwrap_err();
    assert_eq!(err_insuff_priv.code, "InsufficientShares");
}

#[tokio::test]
async fn test_corp_titles_and_bulletins() {
    // parity: rpc:corpRegistry.UpdateTitle
    // parity: rpc:corpRegistry.UpdateTitles
    // parity: rpc:corpRegistry.GetTitles
    // parity: rpc:corpRegistry.DeleteTitle
    // parity: rpc:corpRegistry.AddBulletin
    // parity: rpc:corpRegistry.UpdateBulletinOrder
    // parity: rpc:corpRegistry.GetBulletins
    // parity: rpc:corpRegistry.DeleteBulletin

    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 61_002, "demo", "demo-secret", PILOT_1).await;

    let created = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Vanguard Command".into()),
            PyRep::String("VGCMD".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID")
        .as_i64()
        .expect("corporationID");

    // 1. UpdateTitle & UpdateTitles & GetTitles & DeleteTitle
    let t1 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UpdateTitle",
        &[
            PyRep::Integer(1),
            PyRep::String("Fleet Commander".into()),
            PyRep::Integer(1i64 << 15),
        ],
    )
    .await
    .expect("UpdateTitle");
    assert_eq!(field(&t1, "titleID"), PyRep::Integer(1));

    let t2 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UpdateTitles",
        &[
            PyRep::Integer(2),
            PyRep::String("Chief Engineer".into()),
            PyRep::Integer(1i64 << 22),
        ],
    )
    .await
    .expect("UpdateTitles");
    assert_eq!(field(&t2, "titleID"), PyRep::Integer(2));

    let titles = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetTitles",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetTitles");
    let (_, t_rows) = rowset_data(&titles);
    assert_eq!(t_rows.len(), 2);

    let del_t = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "DeleteTitle",
        &[PyRep::Integer(1)],
    )
    .await
    .expect("DeleteTitle");
    assert_eq!(field(&del_t, "deleted"), PyRep::Bool(true));

    // 2. AddBulletin & DeleteBulletin (monotonic ID allocation + BulletinNotFound error)
    let b1 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddBulletin",
        &[
            PyRep::String("Strategic Deployment".into()),
            PyRep::String("All pilots form up at 20:00 UTC".into()),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("AddBulletin 1");
    assert_eq!(field(&b1, "bulletinID"), PyRep::Integer(1));

    let b2 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddBulletin",
        &[
            PyRep::String("Mining Fleet Schedule".into()),
            PyRep::String("Moon mining belt 2 tomorrow morning".into()),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("AddBulletin 2");
    assert_eq!(field(&b2, "bulletinID"), PyRep::Integer(2));

    // Delete bulletin 2 and add a third bulletin: ID must be monotonic (3, not 2)
    let del_b2 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "DeleteBulletin",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("DeleteBulletin 2");
    assert_eq!(field(&del_b2, "bulletinID"), PyRep::Integer(2));
    assert!(
        rpc(
            &ctx,
            &mut sess,
            "corpRegistry",
            "DeleteBulletin",
            &[PyRep::Integer(999_999)]
        )
        .await
        .is_err(),
        "deleting unknown bulletin must return BulletinNotFound"
    );

    let b3 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddBulletin",
        &[
            PyRep::String("Logistics Convoy".into()),
            PyRep::String("Jump freighters depart at 22:00 UTC".into()),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("AddBulletin 3");
    assert_eq!(
        field(&b3, "bulletinID"),
        PyRep::Integer(3),
        "bulletin IDs must be monotonic after deletion"
    );

    // 3. UpdateBulletinOrder (V24 list of (bulletinID, sortOrder) pairs)
    let reorder_res = rpc(&ctx, &mut sess, "corpRegistry", "UpdateBulletinOrder", &[PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(10)]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(20)]),
    ])]).await.expect("UpdateBulletinOrder");
    assert_eq!(reorder_res, PyRep::Bool(true));

    // 4. GetBulletins (verify sortOrder column and reordered rows [3, 1])
    let bulletins = rpc(&ctx, &mut sess, "corpRegistry", "GetBulletins", &[PyRep::Integer(corp_id)]).await.expect("GetBulletins");
    let (b_cols, b_rows) = rowset_data(&bulletins);
    assert!(b_cols.contains(&"sortOrder".to_string()));
    assert_eq!(b_rows.len(), 2);
    assert_eq!(b_rows[0][0], PyRep::Integer(3));
    assert_eq!(b_rows[1][0], PyRep::Integer(1));

    // 5. RegisterNewAggressionSettings + GetAggressionSettings 24h delay & RegisterNewAcceptStructureSettings + DoesMyCorpAcceptStructures
    let reg_agg = rpc(&ctx, &mut sess, "corpRegistry", "RegisterNewAggressionSettings", &[PyRep::Bool(true)]).await.expect("RegisterNewAggressionSettings");
    assert!(field(&reg_agg, "_enableAfter").as_i64().unwrap_or(0) > 0);
    let agg_before = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetAggressionSettings",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetAggressionSettings before 24h");
    assert_eq!(field(&agg_before, "isFriendlyFireLegal"), PyRep::Bool(false));
    assert_eq!(field(&agg_before, "pendingFriendlyFireLegal"), PyRep::Bool(true));
    ctx.advance_secs(86_401);
    let agg_after = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetAggressionSettings",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetAggressionSettings after 24h");
    assert_eq!(field(&agg_after, "isFriendlyFireLegal"), PyRep::Bool(true));
    assert_eq!(field(&agg_after, "pendingFriendlyFireLegal"), PyRep::None);
    let (ff_persisted, ff_pending) = ctx.db.with_snapshot(|snap| {
        snap.corp_governance
            .get(&corp_id)
            .map(|g| (g.friendly_fire_legal, g.pending_friendly_fire))
            .unwrap_or((false, None))
    });
    assert!(ff_persisted && ff_pending.is_none());

    assert_eq!(
        rpc(&ctx, &mut sess, "corpRegistry", "DoesMyCorpAcceptStructures", &[]).await.unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "corpRegistry", "RegisterNewAcceptStructureSettings", &[PyRep::Bool(false)]).await.unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "corpRegistry", "DoesMyCorpAcceptStructures", &[]).await.unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "corpRegistry", "RegisterNewAcceptStructureSettings", &[PyRep::Bool(true)]).await.unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "corpRegistry", "DoesMyCorpAcceptStructures", &[]).await.unwrap(),
        PyRep::Bool(true)
    );
}

#[tokio::test]
async fn test_corp_contacts_and_labels() {
    // parity: rpc:corpRegistry.AddCorporateContact
    // parity: rpc:corpRegistry.EditCorporateContact
    // parity: rpc:corpRegistry.EditContactsRelationshipID
    // parity: rpc:corpRegistry.RemoveCorporateContacts
    // parity: rpc:corpRegistry.GetCorporateContacts
    // parity: rpc:corpRegistry.CreateLabel
    // parity: rpc:corpRegistry.EditLabel
    // parity: rpc:corpRegistry.GetLabels
    // parity: rpc:corpRegistry.DeleteLabel
    // parity: rpc:corpRegistry.AssignLabels
    // parity: rpc:corpRegistry.RemoveLabels

    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 61_003, "demo", "demo-secret", PILOT_1).await;

    let created = rpc(&ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String("Diplomatic League".into()), PyRep::String("DLEAG".into()), PyRep::Real(0.05)]).await.expect("AddCorporation");
    let _corp_id = field(&created, "corporationID").as_i64().expect("corporationID");

    // Seed PILOT_3 so PILOT_3 is a valid owner ID
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut ch3) = snap.characters.get(&PILOT_2).cloned() {
            ch3.character_id = PILOT_3;
            ch3.name = "Diplomat Three".into();
            snap.characters.insert(PILOT_3, ch3);
        }
    });

    // 1. AddCorporateContact: duplicate and invalid owner checks
    let c1 = rpc(&ctx, &mut sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(PILOT_2), PyRep::Real(5.0), PyRep::Integer(1)]).await.expect("AddCorporateContact PILOT_2");
    assert_eq!(field(&c1, "contactID"), PyRep::Integer(PILOT_2));
    assert_eq!(field(&c1, "relationshipID"), PyRep::Real(5.0));

    assert!(rpc(&ctx, &mut sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(PILOT_2), PyRep::Real(5.0)]).await.is_err(), "duplicate AddCorporateContact must fail");
    assert!(rpc(&ctx, &mut sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(999_888_777), PyRep::Real(5.0)]).await.is_err(), "unknown owner ID must fail");

    // 2. EditCorporateContact: missing contact check + update
    assert!(rpc(&ctx, &mut sess, "corpRegistry", "EditCorporateContact", &[PyRep::Integer(PILOT_3), PyRep::Real(10.0)]).await.is_err(), "editing non-existent contact must fail");
    let c2 = rpc(&ctx, &mut sess, "corpRegistry", "EditCorporateContact", &[PyRep::Integer(PILOT_2), PyRep::Real(10.0), PyRep::Integer(1)]).await.expect("EditCorporateContact");
    assert_eq!(field(&c2, "relationshipID"), PyRep::Real(10.0));

    // Add PILOT_3 as second contact for batch operations
    rpc(&ctx, &mut sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(PILOT_3), PyRep::Real(0.0)]).await.expect("AddCorporateContact PILOT_3");

    // 3. EditContactsRelationshipID (V24 batch list [PILOT_2, PILOT_3] + missing contact check)
    assert!(rpc(&ctx, &mut sess, "corpRegistry", "EditContactsRelationshipID", &[PyRep::List(vec![PyRep::Integer(999_888_777)]), PyRep::Real(-5.0)]).await.is_err());
    let c3 = rpc(&ctx, &mut sess, "corpRegistry", "EditContactsRelationshipID", &[PyRep::List(vec![PyRep::Integer(PILOT_2), PyRep::Integer(PILOT_3)]), PyRep::Real(-5.0)]).await.expect("EditContactsRelationshipID batch");
    assert_eq!(field(&c3, "relationshipID"), PyRep::Real(-5.0));

    // 4. CreateLabel, EditLabel, GetLabels
    let l1 = rpc(&ctx, &mut sess, "corpRegistry", "CreateLabel", &[PyRep::String("Strategic Allies".into()), PyRep::Integer(0x00FF00)]).await.expect("CreateLabel");
    let label_id = field(&l1, "labelID").as_i64().expect("labelID");
    assert!(label_id > 0);

    let l2 = rpc(&ctx, &mut sess, "corpRegistry", "EditLabel", &[PyRep::Integer(label_id), PyRep::String("Coalition Partners".into()), PyRep::Integer(0x0000FF)]).await.expect("EditLabel");
    assert_eq!(field(&l2, "name"), PyRep::String("Coalition Partners".into()));

    let labels = rpc(&ctx, &mut sess, "corpRegistry", "GetLabels", &[]).await.expect("GetLabels");
    let (_, l_rows) = rowset_data(&labels);
    assert_eq!(l_rows.len(), 1);

    // 5. AssignLabels & RemoveLabels across multiple contacts + ContactNotFound checks
    assert!(rpc(&ctx, &mut sess, "corpRegistry", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(999_888_777)]), PyRep::Integer(label_id)]).await.is_err());
    rpc(&ctx, &mut sess, "corpRegistry", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(PILOT_2), PyRep::Integer(PILOT_3)]), PyRep::Integer(label_id)]).await.expect("AssignLabels batch");

    let contacts = rpc(&ctx, &mut sess, "corpRegistry", "GetCorporateContacts", &[]).await.expect("GetCorporateContacts after AssignLabels");
    let (_, c_rows) = rowset_data(&contacts);
    assert_eq!(c_rows.len(), 2);
    assert!(c_rows.iter().all(|r| r[1] == PyRep::Real(-5.0)));
    assert!(c_rows.iter().all(|r| r[2] == PyRep::Integer(label_id)));

    assert!(rpc(&ctx, &mut sess, "corpRegistry", "RemoveLabels", &[PyRep::List(vec![PyRep::Integer(999_888_777)]), PyRep::Integer(label_id)]).await.is_err());
    rpc(&ctx, &mut sess, "corpRegistry", "RemoveLabels", &[PyRep::List(vec![PyRep::Integer(PILOT_2), PyRep::Integer(PILOT_3)]), PyRep::Integer(label_id)]).await.expect("RemoveLabels batch");

    // 6. DeleteLabel + LabelNotFound check + RemoveCorporateContacts
    assert!(rpc(&ctx, &mut sess, "corpRegistry", "DeleteLabel", &[PyRep::Integer(999_999)]).await.is_err());
    let del_l = rpc(&ctx, &mut sess, "corpRegistry", "DeleteLabel", &[PyRep::Integer(label_id)]).await.expect("DeleteLabel");
    assert_eq!(field(&del_l, "labelID"), PyRep::Integer(label_id));

    let rem_c = rpc(&ctx, &mut sess, "corpRegistry", "RemoveCorporateContacts", &[PyRep::List(vec![PyRep::Integer(PILOT_2), PyRep::Integer(PILOT_3)])]).await.expect("RemoveCorporateContacts");
    assert_eq!(field(&rem_c, "removed"), PyRep::Bool(true));
}
