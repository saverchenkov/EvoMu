//! Focused unit tests for uncovered branches in `wallet_serverops`
//! (`tax_collector_id`, `create_bill`, `process_due_automatic_bills`, `billMgr`,
//! `account.GiveCash`, `account.SetContactCost`, `account.GetDefaultContactCost`,
//! `machoNet`, `pingService`, `localizationServer`, and `clientStatLogger`).

use super::*;
use eve_db::clock::VirtualServerClock;
use eve_db::{CorporationRecord, IskCents, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn full_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::clone(&ServerContext::new().sde)))
}

fn setup_wallet_ops_world() -> (ServerContext, i64, i64, i64, i64) {
    let ctx = ServerContext::with_sde_and_repository(
        full_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000)));

    let ceo_id = 96_910_001i64;
    let member_id = 96_910_002i64;
    let corp_a = 98_910_001i64;
    let corp_b = 98_910_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = ceo_id;
        ch.user_id = 9301;
        ch.name = "BillCEO".into();
        ch.corporation_id = corp_a;
        ch.balance = IskCents::from_f64_isk(50_000_000.0);
        snap.characters.insert(ceo_id, ch.clone());

        let mut mem = ch;
        mem.character_id = member_id;
        mem.user_id = 9302;
        mem.name = "BillMember".into();
        mem.balance = IskCents::from_f64_isk(10_000_000.0);
        snap.characters.insert(member_id, mem);

        let mut ca = CorporationRecord {
            corporation_id: corp_a,
            name: "Alpha Industry".into(),
            ticker: "ALPI".into(),
            ceo_id,
            master_wallet: IskCents::from_f64_isk(100_000_000.0),
            ..Default::default()
        };
        ca.ensure_wallet_divisions();
        ca.wallet_divisions[0].balance = IskCents::from_f64_isk(100_000_000.0);
        ca.wallet_divisions[1].balance = IskCents::from_f64_isk(25_000_000.0);
        snap.corporations.insert(corp_a, ca);

        let mut cb = CorporationRecord {
            corporation_id: corp_b,
            name: "Beta Creditor".into(),
            ticker: "BETC".into(),
            ceo_id: 96_910_099,
            master_wallet: IskCents::from_f64_isk(10_000_000.0),
            ..Default::default()
        };
        cb.ensure_wallet_divisions();
        snap.corporations.insert(corp_b, cb);
    });

    (ctx, ceo_id, member_id, corp_a, corp_b)
}

fn make_session(char_id: i64, corp_id: i64, role: u64) -> SessionState {
    SessionState {
        user_id: Some(9301),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        role,
        ..Default::default()
    }
}

#[test]
fn bills_autopay_and_tax_collector_workflow() {
    let (ctx, ceo_id, member_id, corp_a, corp_b) = setup_wallet_ops_world();
    let kw = HashMap::new();

    assert_ne!(tax_collector_id(&ctx), 0);

    let mut ceo_sess = make_session(ceo_id, corp_a, 0);
    let mut mem_sess = make_session(member_id, corp_a, 0);

    // Create personal bill (debtor = member_id), two corp payable bills (debtor = corp_a), & 1 receivable bill (creditor = corp_a)
    let personal_bill = create_bill(&ctx, 1, member_id, corp_b, 500_000.0, 1_700_010_000, 0.0, 0, 0);
    let corp_bill_1 = create_bill(&ctx, 2, corp_a, corp_b, 2_000_000.0, 1_700_010_000, 0.0, 60_003_760, 0);
    let _corp_bill_2 = create_bill(&ctx, 2, corp_a, corp_b, 1_500_000.0, 1_700_005_000, 0.0, 60_003_761, 0);
    let _corp_recv_bill = create_bill(&ctx, 2, corp_b, corp_a, 750_000.0, 1_700_010_000, 0.0, 60_003_762, 0);

    // Non-privileged member cannot query corp bills or pay corp bill
    let err = handle(&ctx, &mut mem_sess, "billMgr", "GetCorporationBills", &[], &kw)
        .expect("handled")
        .expect_err("member lacks accountant role");
    assert_eq!(err.code, "CorpRoleRequired");

    let err = handle(&ctx, &mut mem_sess, "billMgr", "PayCorporationBill", &[PyRep::Integer(corp_bill_1)], &kw)
        .expect("handled")
        .expect_err("member lacks accountant role");
    assert_eq!(err.code, "CorpRoleRequired");

    // Wrong debtor paying personal bill fails with AccessDenied
    let err = handle(&ctx, &mut ceo_sess, "billMgr", "CharPayBill", &[PyRep::Integer(personal_bill)], &kw)
        .expect("handled")
        .expect_err("ceo is not personal bill debtor");
    assert_eq!(err.code, "AccessDenied");

    // Member pays own personal bill -> character debited, corp_b credited, second attempt fails BillAlreadyPaid
    let paid = handle(&ctx, &mut mem_sess, "billMgr", "CharPayBill", &[PyRep::Integer(personal_bill)], &kw)
        .expect("handled")
        .expect("personal bill paid");
    assert_eq!(paid, PyRep::Bool(true));
    assert_eq!(
        handle(&ctx, &mut mem_sess, "billMgr", "CharPayBill", &[PyRep::Integer(personal_bill)], &kw)
            .expect("handled")
            .unwrap_err()
            .code,
        "BillAlreadyPaid"
    );

    // CEO pays corp_bill_1 from division 1001 (div 2)
    handle(
        &ctx,
        &mut ceo_sess,
        "billMgr",
        "PayCorporationBill",
        &[PyRep::Integer(corp_bill_1), PyRep::Integer(1001)],
        &kw,
    )
    .expect("handled")
    .expect("corp bill paid from div 2");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corporations[&corp_a].wallet_divisions[1].balance, IskCents::from_f64_isk(23_000_000.0));
    });

    // GetCorporationBills & GetCorporationBillsReceivable
    let payable = handle(&ctx, &mut ceo_sess, "billMgr", "GetCorporationBills", &[], &kw)
        .expect("handled")
        .expect("payable bills");
    assert!(matches!(payable, PyRep::Rowset { ref rows, .. } if !rows.is_empty()));
    let recv = handle(&ctx, &mut ceo_sess, "billMgr", "GetCorporationBillsReceivable", &[], &kw)
        .expect("handled")
        .expect("receivable bills");
    assert!(matches!(recv, PyRep::Rowset { ref rows, .. } if !rows.is_empty()));

    // Advance clock past corp_bill_2 dueDateTime (1_700_005_000) and enable automatic payment for bill_type 2
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_700_006_000)));
    let settings = PyRep::Dict(vec![(
        PyRep::Integer(corp_a),
        PyRep::Dict(vec![
            (PyRep::Integer(2), PyRep::Bool(true)),
            (PyRep::String("divisionID".into()), PyRep::Integer(1000)),
        ]),
    )]);
    handle(&ctx, &mut ceo_sess, "billMgr", "SendAutomaticPaySettings", &[settings], &kw)
        .expect("handled")
        .expect("autopay configured and processed");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corporations[&corp_a].master_wallet),
        IskCents::from_f64_isk(98_500_000.0)
    );

    let got_settings = handle(&ctx, &mut ceo_sess, "billMgr", "GetAutomaticPaySettings", &[], &kw)
        .expect("handled")
        .expect("get autopay settings");
    assert!(matches!(got_settings, PyRep::Dict(_)));
}

#[test]
fn account_give_cash_contact_cost_and_utility_endpoints() {
    let (ctx, ceo_id, member_id, corp_a, _) = setup_wallet_ops_world();
    let kw = HashMap::new();
    let mut ceo_sess = make_session(ceo_id, corp_a, eve_machonet::service_roles::ROLE_PROGRAMMER);

    // GiveCash character-to-character & character-to-corporation
    let start_ceo = ctx.db.with_snapshot(|s| s.characters[&ceo_id].balance.0);
    let start_mem = ctx.db.with_snapshot(|s| s.characters[&member_id].balance.0);
    handle(
        &ctx,
        &mut ceo_sess,
        "account",
        "GiveCash",
        &[PyRep::Integer(member_id), PyRep::Real(1_250_000.0), PyRep::String("Ops stipend".into())],
        &kw,
    )
    .expect("handled")
    .expect("give cash to character");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&ceo_id].balance.0, start_ceo - IskCents::from_f64_isk(1_250_000.0).0);
        assert_eq!(snap.characters[&member_id].balance.0, start_mem + IskCents::from_f64_isk(1_250_000.0).0);
    });

    // GiveCash to corp wallet division 1002
    handle(
        &ctx,
        &mut ceo_sess,
        "account",
        "GiveCash",
        &[PyRep::Integer(corp_a), PyRep::Real(500_000.0), PyRep::String("Corp donation".into()), PyRep::Integer(1002)],
        &kw,
    )
    .expect("handled")
    .expect("give cash to corp");

    // SetContactCost & GetDefaultContactCost
    handle(&ctx, &mut ceo_sess, "account", "SetContactCost", &[PyRep::Real(250_000.0)], &kw)
        .expect("handled")
        .expect("set contact cost");
    let cost = handle(&ctx, &mut ceo_sess, "account", "GetDefaultContactCost", &[], &kw)
        .expect("handled")
        .expect("get contact cost")
        .as_f64()
        .unwrap();
    assert!((cost - 250_000.0).abs() < 0.01);

    // machoNet.GetClusterGameStatisticsForClient & pingService.Ping & clientStatLogger.LogString & localizationServer.ReloadFSDPickle
    let stats = handle(&ctx, &mut ceo_sess, "machoNet", "GetClusterGameStatisticsForClient", &[], &kw)
        .expect("handled")
        .expect("cluster stats");
    assert!(matches!(stats, PyRep::Tuple(ref t) if t.len() == 3));

    let pong = handle(&ctx, &mut ceo_sess, "pingService", "Ping", &[], &kw)
        .expect("handled")
        .expect("ping");
    assert!(pong.as_i64().unwrap() > 0);

    handle(&ctx, &mut ceo_sess, "clientStatLogger", "LogString", &[PyRep::String("client telemetry sample".into())], &kw)
        .expect("handled")
        .expect("log string");
    let reload = handle(&ctx, &mut ceo_sess, "localizationServer", "ReloadFSDPickle", &[], &kw)
        .expect("handled")
        .expect("reload pickle with programmer role");
    assert_eq!(reload, PyRep::Bool(true));
}
