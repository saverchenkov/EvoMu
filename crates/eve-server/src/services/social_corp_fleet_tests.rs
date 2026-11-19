//! Unit tests for `social_corp_fleet` alliances, applications, executor support, and timers.

#[cfg(test)]
#[path = "social_corp_fleet_alliance_tests.rs"]
mod social_corp_fleet_alliance_tests;

use super::{handle, sync_all_alliance_timers};
use crate::router::ServerContext;
use eve_db::{
    CorpWalletDivisionRecord, CorporationRecord, IskCents, ServerClock, VirtualServerClock,
    WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub(super) fn kv<'a>(rep: &'a PyRep, k: &str) -> &'a PyRep {
    rep.dict_get(k).unwrap_or_else(|| panic!("missing key {k}"))
}

pub(super) fn rs_rows(rep: &PyRep) -> &[Vec<PyRep>] {
    match rep {
        PyRep::Rowset { rows, .. } => rows,
        _ => panic!("expected Rowset"),
    }
}

pub(super) async fn call_social(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    handle(ctx, sess, svc, method, &args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|| panic!("unhandled {svc}.{method}"))
}

pub(super) fn setup_alliance_sandbox() -> (ServerContext, Arc<VirtualServerClock>, SessionState, SessionState, i64) {
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000));
    let ctx = ServerContext::new();
    ctx.set_clock(clock.clone());
    ctx.db.with_mut_snapshot(|snap: &mut WorldStateSnapshot| {
        let mut tpl = snap.characters.values().next().unwrap().clone();
        for (cid, name, corp_id) in [
            (91_000_001i64, "Exec CEO", 98_100_001i64),
            (91_000_002i64, "Member CEO", 98_100_002i64),
        ] {
            let mut ch = tpl.clone();
            ch.character_id = cid;
            ch.name = name.to_string();
            ch.corporation_id = corp_id;
            ch.alliance_id = None;
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
            ch.constellation_id = 20_000_020;
            ch.region_id = 10_000_002;
            ch.balance = IskCents::from_isk(5_000_000_000);
            snap.characters.insert(cid, ch);

            let mut divs = CorporationRecord::default_wallet_divisions(IskCents::from_isk(5_000_000_000));
            divs[0] = CorpWalletDivisionRecord {
                division_id: 1,
                name: "Master Wallet".into(),
                balance: IskCents::from_isk(5_000_000_000),
            };
            snap.corporations.insert(
                corp_id,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = corp_id;
                    __corp.name = format!("Corp {corp_id}");
                    __corp.ticker = if corp_id == 98_100_001 { "EXEC".into() } else { "MEMB".into() };
                    __corp.ceo_id = cid;
                    __corp.alliance_id = None;
                    __corp.tax_rate = 0.05;
                    __corp.members = HashSet::from([cid]);
                    __corp.member_roles = HashMap::from([(cid, u64::MAX)]);
                    __corp.applications = HashMap::new();
                    __corp.description = String::new();
                    __corp.url = String::new();
                    __corp.hq_station_id = Some(60_003_760);
                    __corp.wallet_divisions = divs;
                    let __mw = IskCents::from_isk(5_000_000_000);
                    if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); }
                    __corp },
            );
        }
        tpl.character_id = 91_000_001;
        snap.characters.entry(91_000_001).or_insert(tpl);
    });

    let mut exec = SessionState::new(1);
    exec.user_id = Some(91_000_001);
    exec.character_id = Some(91_000_001);
    exec.corporation_id = Some(98_100_001);
    exec.corp_role = u64::MAX;
    exec.station_id = Some(60_003_760);
    exec.solar_system_id = Some(30_000_142);

    let mut memb = SessionState::new(2);
    memb.user_id = Some(91_000_002);
    memb.character_id = Some(91_000_002);
    memb.corporation_id = Some(98_100_002);
    memb.corp_role = u64::MAX;
    memb.station_id = Some(60_003_760);
    memb.solar_system_id = Some(30_000_142);

    (ctx, clock, exec, memb, 98_100_001)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn social_alliance_lifecycle_timers_and_executor() {
    let (ctx, clock, mut exec, mut memb, _) = setup_alliance_sandbox();

    let fleet = call_social(&ctx, &mut memb, "fleetSvc", "InitFleet", vec![])
        .await
        .unwrap();
    assert_eq!(kv(&fleet, "bossID").as_i64(), Some(91_000_002));

    let bad_name = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "CreateAlliance",
        vec![PyRep::String("Bad".into()), PyRep::String("TOOLONG".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(bad_name.code, "InvalidAllianceName");

    let created = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "CreateAlliance",
        vec![
            PyRep::String("Northern Accord".into()),
            PyRep::String("NACC".into()),
            PyRep::String("Coalition".into()),
            PyRep::String("https://nacc.eve".into()),
        ],
    )
    .await
    .unwrap();
    let aid = kv(&created, "allianceID").as_i64().unwrap();
    assert_eq!(kv(&created, "shortName").as_str(), Some("NACC"));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.corporations[&98_100_001].master_wallet_balance().0),
        4_000_000_000 * 100
    );

    let app = call_social(
        &ctx,
        &mut memb,
        "corpRegistry",
        "ApplyToJoinAlliance",
        vec![PyRep::Integer(aid), PyRep::String("Ready to deploy".into())],
    )
    .await
    .unwrap();
    assert_eq!(kv(&app, "status").as_str(), Some("Pending"));

    let acc = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "UpdateApplication",
        vec![PyRep::Integer(98_100_002)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&acc, "status").as_str(), Some("Accepted"));

    let apps_rowset = call_social(
        &ctx,
        &mut memb,
        "allianceRegistry",
        "GetAllianceApplications",
        vec![PyRep::Integer(aid)],
    )
    .await
    .unwrap();
    assert_eq!(rs_rows(&apps_rowset).len(), 1);

    clock.advance_secs(86_405);
    sync_all_alliance_timers(&ctx);
    assert!(ctx.db.with_snapshot(|s| s.alliances[&aid].member_corp_ids.contains(&98_100_002)));
    memb.alliance_id = Some(aid);

    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "allowWar", vec![PyRep::Integer(aid)])
            .await
            .unwrap()
            .as_bool(),
        Some(true)
    );
    let pub_info = call_social(
        &ctx,
        &mut memb,
        "allianceRegistry",
        "GetAlliance",
        vec![PyRep::Integer(aid)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&pub_info, "shortName").as_str(), Some("NACC"));
    assert_eq!(kv(&pub_info, "memberCount").as_i64(), Some(2));
    let members_rs = call_social(&ctx, &mut exec, "allianceRegistry", "GetAllianceMembers", vec![])
        .await
        .unwrap();
    assert_eq!(rs_rows(&members_rs).len(), 2);
    let emp_rs = call_social(&ctx, &mut exec, "allianceRegistry", "GetEmploymentRecord", vec![])
        .await
        .unwrap();
    assert_eq!(rs_rows(&emp_rs).len(), 2);
    let ranked_rs = call_social(&ctx, &mut exec, "allianceRegistry", "GetRankedAlliances", vec![PyRep::Integer(5)])
        .await
        .unwrap();
    assert_eq!(rs_rows(&ranked_rs).len(), 1);
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GetDaysInAlliance", vec![])
            .await
            .unwrap()
            .as_i64(),
        Some(1)
    );

    let err_kick = call_social(
        &ctx,
        &mut memb,
        "allianceRegistry",
        "KickMember",
        vec![PyRep::Integer(98_100_001)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_kick.code, "NotAllianceExecutor");

    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "DeclareExecutorSupport",
        vec![PyRep::Integer(98_100_002)],
    )
    .await
    .unwrap();
    let shifted = call_social(
        &ctx,
        &mut memb,
        "allianceRegistry",
        "DeclareExecutorSupport",
        vec![PyRep::Integer(98_100_002)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&shifted, "executorCorpID").as_i64(), Some(98_100_002));

    let kicked = call_social(
        &ctx,
        &mut memb,
        "allianceRegistry",
        "KickMember",
        vec![PyRep::Integer(98_100_001)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&kicked, "removed").as_bool(), Some(true));
    assert!(!ctx.db.with_snapshot(|s| s.alliances[&aid].member_corp_ids.contains(&98_100_001)));

    let withdraw = call_social(
        &ctx,
        &mut memb,
        "allianceRegistry",
        "DeleteMember",
        vec![PyRep::Integer(98_100_002)],
    )
    .await
    .unwrap();
    assert_eq!(kv(&withdraw, "delaySeconds").as_i64(), Some(86_400));

    let err_no_app = call_social(
        &ctx,
        &mut memb,
        "allianceRegistry",
        "UpdateApplication",
        vec![PyRep::Integer(98_100_001)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_no_app.code, "ApplicationNotFound");
    let _ = call_social(
        &ctx,
        &mut exec,
        "corpRegistry",
        "DeleteAllianceApplication",
        vec![PyRep::Integer(aid)],
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_application_rejects_and_gm_force_accepts_immediately() {
    let (ctx, _clock, mut exec, mut applicant, _) = setup_alliance_sandbox();
    let created = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "CreateAlliance",
        vec![
            PyRep::String("Rejection Test Alliance".into()),
            PyRep::String("RTAL".into()),
        ],
    )
    .await
    .unwrap();
    let aid = kv(&created, "allianceID").as_i64().unwrap();
    exec.alliance_id = Some(aid);
    call_social(
        &ctx,
        &mut applicant,
        "corpRegistry",
        "ApplyToJoinAlliance",
        vec![PyRep::Integer(aid), PyRep::String("Please let us in".into())],
    )
    .await
    .unwrap();

    // State 3 / "Rejected" rejects the application without scheduling membership.
    let rej = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "UpdateApplication",
        vec![
            PyRep::Integer(98_100_002),
            PyRep::String("No thanks".into()),
            PyRep::Integer(3),
        ],
    )
    .await
    .unwrap();
    assert_eq!(kv(&rej, "status"), &PyRep::String("Rejected".into()));
    assert_eq!(kv(&rej, "joinEffectiveTime"), &PyRep::None);

    let apps = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "GetAllianceApplications",
        vec![PyRep::Integer(aid)],
    )
    .await
    .unwrap();
    assert_eq!(rs_rows(&apps)[0][4], PyRep::String("Rejected".into()));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.alliances[&aid].member_corp_ids.contains(&98_100_002));
    });

    // Re-apply and test GMForceAcceptApplication with GM role.
    call_social(
        &ctx,
        &mut applicant,
        "corpRegistry",
        "DeleteAllianceApplication",
        vec![PyRep::Integer(aid)],
    )
    .await
    .unwrap();
    call_social(
        &ctx,
        &mut applicant,
        "corpRegistry",
        "ApplyToJoinAlliance",
        vec![PyRep::Integer(aid), PyRep::String("GM fast-track".into())],
    )
    .await
    .unwrap();
    exec.role |= eve_machonet::service_roles::ROLE_GMH;
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "GMForceAcceptApplication",
            vec![PyRep::Integer(98_100_002)],
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        assert!(snap.alliances[&aid].member_corp_ids.contains(&98_100_002));
        assert_eq!(snap.corporations[&98_100_002].alliance_id, Some(aid));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alliance_getters_rankings_capital_prime_bills_and_gm_errors() {
    use crate::services::wallet_services_ext::{bill_note_id, save_json_note, CorpBillRecord};

    let (ctx, clock, mut exec, mut memb, _) = setup_alliance_sandbox();
    let created = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "CreateAlliance",
        vec![PyRep::String("Test Alliance".into()), PyRep::String("TEST".into())],
    )
    .await
    .unwrap();
    let aid = created.dict_get("allianceID").unwrap().as_i64().unwrap();
    exec.alliance_id = Some(aid);

    // Alliance getters & missing alliance error
    for (getter, expected) in [
        ("allianceID", PyRep::Integer(aid)),
        ("shortName", PyRep::String("TEST".into())),
        ("executorCorpID", PyRep::Integer(98_100_001)),
        ("creatorCorpID", PyRep::Integer(98_100_001)),
        ("creatorCharID", PyRep::Integer(91_000_001)),
        ("allowWar", PyRep::Bool(true)),
    ] {
        assert_eq!(
            call_social(&ctx, &mut exec, "allianceRegistry", getter, vec![])
                .await
                .unwrap(),
            expected
        );
    }
    let mut bad_sess = exec.clone();
    bad_sess.alliance_id = Some(999_888_777);
    assert_eq!(
        call_social(&ctx, &mut bad_sess, "allianceRegistry", "GetAlliancePublicInfo", vec![])
            .await
            .unwrap_err()
            .code,
        "AllianceNotFound"
    );

    // GetDaysInAlliance: non-zero after clock advance; NotInAlliance for unallied caller
    clock.advance_secs(3 * 86_400 + 100);
    let days = call_social(&ctx, &mut exec, "allianceRegistry", "GetDaysInAlliance", vec![])
        .await
        .unwrap();
    assert!(matches!(days, PyRep::Integer(d) if d >= 3));
    assert_eq!(
        call_social(&ctx, &mut memb, "allianceRegistry", "GetDaysInAlliance", vec![])
            .await
            .unwrap_err()
            .code,
        "NotInAlliance"
    );

    // GetRankedAlliances with max_len truncation across two alliances
    let created2 = call_social(
        &ctx,
        &mut memb,
        "allianceRegistry",
        "CreateAlliance",
        vec![PyRep::String("Beta Alliance".into()), PyRep::String("BETA".into())],
    )
    .await
    .unwrap();
    let aid2 = created2.dict_get("allianceID").unwrap().as_i64().unwrap();
    let ranked = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "GetRankedAlliances",
        vec![PyRep::Integer(1)],
    )
    .await
    .unwrap();
    assert_eq!(rs_rows(&ranked).len(), 1);

    // GetCapitalSystemInfo + SetCapitalSystem (InvalidArgument, InvalidSolarSystem, valid + CancelCapitalSystemTransition + 7-day promotion)
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "SetCapitalSystem", vec![PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "SetCapitalSystem", vec![PyRep::Integer(99_999_999)])
            .await
            .unwrap_err()
            .code,
        "InvalidSolarSystem"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "CancelCapitalSystemTransition", vec![])
            .await
            .unwrap_err()
            .code,
        "NoCapitalSystemTransition"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "SetCapitalSystem", vec![PyRep::Integer(30_002_187)])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    let cap_pending = call_social(&ctx, &mut exec, "allianceRegistry", "GetCapitalSystemInfo", vec![])
        .await
        .unwrap();
    assert_eq!(cap_pending.dict_get("newCapitalSystem"), Some(&PyRep::Integer(30_002_187)));
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "CancelCapitalSystemTransition", vec![])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "SetCapitalSystem", vec![PyRep::Integer(30_002_187)])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    clock.advance_secs(7 * 86_400 + 1);
    let cap_done = call_social(&ctx, &mut exec, "allianceRegistry", "GetCapitalSystemInfo", vec![])
        .await
        .unwrap();
    assert_eq!(cap_done.dict_get("currentCapitalSystem"), Some(&PyRep::Integer(30_002_187)));
    assert_eq!(cap_done.dict_get("newCapitalSystem"), Some(&PyRep::None));

    // SetPrimeHour validation + 48-hour promotion
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "SetPrimeHour", vec![])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "SetPrimeHour", vec![PyRep::Integer(25)])
            .await
            .unwrap_err()
            .code,
        "InvalidPrimeHour"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "SetPrimeHour", vec![PyRep::Integer(19)])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    clock.advance_secs(48 * 3600 + 1);
    let prime_done = call_social(&ctx, &mut exec, "allianceRegistry", "GetPrimeTimeInfo", vec![])
        .await
        .unwrap();
    assert_eq!(prime_done.dict_get("currentPrimeHour"), Some(&PyRep::Integer(19)));

    // Bills: BillNotFound, AccessDenied, NotEnoughMoney, PayBill happy path & BillAlreadyPaid
    let now_u = clock.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        save_json_note(
            snap,
            bill_note_id(8001),
            -71,
            8001,
            "bill:8001".into(),
            &CorpBillRecord {
                bill_id: 8001,
                bill_type_id: 1,
                debtor_id: aid2,
                creditor_id: aid,
                amount_cents: 100_00,
                due_unix: now_u + 86_400,
                interest: 0.0,
                external_id: 0,
                external_id2: 0,
                paid: false,
                paid_by_division: None,
            },
            now_u,
        );
        save_json_note(
            snap,
            bill_note_id(8002),
            -71,
            8002,
            "bill:8002".into(),
            &CorpBillRecord {
                bill_id: 8002,
                bill_type_id: 2,
                debtor_id: aid,
                creditor_id: aid2,
                amount_cents: 5_000_000_000_000,
                due_unix: now_u + 86_400,
                interest: 0.0,
                external_id: 0,
                external_id2: 0,
                paid: false,
                paid_by_division: None,
            },
            now_u,
        );
        save_json_note(
            snap,
            bill_note_id(8003),
            -71,
            8003,
            "bill:8003".into(),
            &CorpBillRecord {
                bill_id: 8003,
                bill_type_id: 2,
                debtor_id: aid,
                creditor_id: aid2,
                amount_cents: 250_00,
                due_unix: now_u + 86_400,
                interest: 0.0,
                external_id: 0,
                external_id2: 0,
                paid: false,
                paid_by_division: None,
            },
            now_u,
        );
    });
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "PayBill", vec![PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "BillNotFound"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "PayBill", vec![PyRep::Integer(8001)])
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "PayBill", vec![PyRep::Integer(8002)])
            .await
            .unwrap_err()
            .code,
        "NotEnoughMoney"
    );
    let bal = call_social(&ctx, &mut exec, "allianceRegistry", "GetBillBalance", vec![PyRep::Integer(8003)])
        .await
        .unwrap();
    assert_eq!(bal, PyRep::Real(250.0));
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "PayBill", vec![PyRep::Integer(8003), PyRep::Integer(1000)])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "PayBill", vec![PyRep::Integer(8003)])
            .await
            .unwrap_err()
            .code,
        "BillAlreadyPaid"
    );
    let bills = call_social(&ctx, &mut exec, "allianceRegistry", "GetBills", vec![])
        .await
        .unwrap();
    assert!(!rs_rows(&bills).is_empty());

    // GMForceAcceptApplication errors: GMRoleRequired, InvalidArgument, CorpNotFound, ApplicationNotFound
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GMForceAcceptApplication", vec![PyRep::Integer(98_100_002)])
            .await
            .unwrap_err()
            .code,
        "GMRoleRequired"
    );
    exec.role = eve_db::accounts::service_roles::ROLE_GML;
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GMForceAcceptApplication", vec![PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GMForceAcceptApplication", vec![PyRep::Integer(999_888_777)])
            .await
            .unwrap_err()
            .code,
        "CorpNotFound"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GMForceAcceptApplication", vec![PyRep::Integer(98_100_002)])
            .await
            .unwrap_err()
            .code,
        "ApplicationNotFound"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alliance_bulletins_labels_war_kick_and_invbroker_deliveries_edges() {
    use eve_db::{InventoryFlag, InventoryItemRecord};

    let (ctx, clock, mut exec, memb, _) = setup_alliance_sandbox();
    let created = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "CreateAlliance",
        vec![PyRep::String("Bullet Alliance".into()), PyRep::String("BULL".into())],
    )
    .await
    .unwrap();
    let aid = created.dict_get("allianceID").unwrap().as_i64().unwrap();
    exec.alliance_id = Some(aid);

    // CreateLabel / AssignLabels / RemoveLabels / DeleteLabel + UpdateBulletinOrder error & happy reorder
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "AddBulletin", vec![PyRep::String("".into())])
            .await
            .unwrap_err()
            .code,
        "BulletinTitleRequired"
    );
    let b1 = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "AddBulletin",
        vec![PyRep::String("First".into()), PyRep::String("Body1".into())],
    )
    .await
    .unwrap();
    let b2 = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "AddBulletin",
        vec![PyRep::String("Second".into()), PyRep::String("Body2".into())],
    )
    .await
    .unwrap();
    let id1 = b1.dict_get("bulletinID").unwrap().as_i64().unwrap();
    let id2 = b2.dict_get("bulletinID").unwrap().as_i64().unwrap();
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "UpdateBulletinOrder", vec![PyRep::List(vec![])])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "UpdateBulletinOrder",
            vec![PyRep::List(vec![PyRep::Integer(id2), PyRep::Integer(id1)])],
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "DeleteBulletin", vec![PyRep::Integer(999_999)])
            .await
            .unwrap(),
        PyRep::Bool(false)
    );

    let lbl_rep = call_social(&ctx, &mut exec, "allianceRegistry", "CreateLabel", vec![PyRep::String("Logi".into()), PyRep::Integer(1)])
        .await
        .unwrap();
    let lbl_id = kv(&lbl_rep, "labelID").as_i64().unwrap();
    // Swapped (cid, labelID) auto-inserts contact and assigns/removes label
    let _ = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "AssignLabels",
        vec![PyRep::Integer(98_100_002), PyRep::Integer(lbl_id)],
    )
    .await
    .unwrap();
    let _ = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "RemoveLabels",
        vec![PyRep::Integer(98_100_002), PyRep::Integer(lbl_id)],
    )
    .await
    .unwrap();
    let _ = call_social(&ctx, &mut exec, "allianceRegistry", "DeleteLabel", vec![PyRep::Integer(lbl_id)])
        .await
        .unwrap();

    // invbroker.DeliverToCorpHangar & DeliverToCorpMember edge cases
    let mut undocked = exec.clone();
    undocked.station_id = None;
    undocked.structure_id = None;
    assert_eq!(
        call_social(&ctx, &mut undocked, "invbroker", "DeliverToCorpHangar", vec![PyRep::Integer(1)],)
            .await
            .unwrap_err()
            .code,
        "NotDocked"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "invbroker", "DeliverToCorpHangar", vec![PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "ItemNotFound"
    );
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&91_000_001].active_ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_000_001,
            InventoryItemRecord {
                item_id: 990_000_001,
                type_id: 34,
                owner_id: 91_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            990_000_002,
            InventoryItemRecord {
                item_id: 990_000_002,
                type_id: 34,
                owner_id: 91_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            990_000_003,
            InventoryItemRecord {
                item_id: 990_000_003,
                type_id: 34,
                owner_id: 91_000_001,
                location_id: 60_000_001,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            990_000_004,
            InventoryItemRecord {
                item_id: 990_000_004,
                type_id: 587,
                owner_id: 91_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if ship_id > 0 {
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 587,
                    owner_id: 91_000_001,
                    location_id: 60_003_760,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });
    if ship_id > 0 {
        assert_eq!(
            call_social(&ctx, &mut exec, "invbroker", "DeliverToCorpHangar", vec![PyRep::Integer(ship_id), PyRep::Integer(1)])
                .await
                .unwrap_err()
                .code,
            "CannotDeliverActiveShip"
        );
    }
    assert_eq!(
        call_social(&ctx, &mut exec, "invbroker", "DeliverToCorpHangar", vec![PyRep::Integer(990_000_002), PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "ItemCannotBeTransferred"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "invbroker", "DeliverToCorpHangar", vec![PyRep::Integer(990_000_003), PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "ItemNotAtLocation"
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "invbroker", "DeliverToCorpMember", vec![PyRep::Integer(990_000_001), PyRep::Integer(91_000_002)])
            .await
            .unwrap_err()
            .code,
        "TargetNotInCorporation"
    );
    let deliv = call_social(
        &ctx,
        &mut exec,
        "invbroker",
        "DeliverToCorpHangar",
        vec![PyRep::Integer(990_000_001), PyRep::Integer(99)],
    )
    .await
    .unwrap();
    assert_eq!(deliv.dict_get("delivered"), Some(&PyRep::Bool(true)));
    assert_eq!(deliv.dict_get("flagID"), Some(&PyRep::Integer(InventoryFlag::Deliveries as i64)));

    // KickMember self-last-member schedules 24h leave, and sync_all_alliance_timers closes alliance after 24h
    let _ = memb.character_id;
    let del_rep = call_social(&ctx, &mut exec, "allianceRegistry", "DeleteMember", vec![PyRep::Integer(98_100_001)])
        .await
        .unwrap();
    assert_eq!(kv(&del_rep, "delaySeconds"), &PyRep::Integer(86_400));
    assert_eq!(kv(&del_rep, "corporationID"), &PyRep::Integer(98_100_001));
    clock.advance_secs(86_401);
    sync_all_alliance_timers(&ctx);
    ctx.db.with_snapshot(|snap| assert!(!snap.alliances.contains_key(&aid)));
}

#[tokio::test]
async fn get_alliance_applications_enforces_director_and_blocks_npc_and_cross_corp_leaks() {
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000));
    let ctx = ServerContext::new();
    ctx.set_clock(clock.clone());
    let exec_corp = 98_100_901;
    let applicant_corp = 98_100_902;
    let bystander_corp = 98_100_903;
    let member_corp = 98_100_904;
    let exec_char = 91_000_901;
    let dir_char = 91_000_902;
    let plain_char = 91_000_903;
    let applicant_ceo = 91_000_904;
    let bystander_ceo = 91_000_905;
    let npc_char = 91_000_906;
    let member_ceo = 91_000_907;
    let npc_corp = 1_000_009;

    ctx.db.with_mut_snapshot(|snap| {
        for (corp_id, ceo_id, name, ticker) in [
            (exec_corp, exec_char, "ExecRbac", "EXRB"),
            (applicant_corp, applicant_ceo, "AppRbac", "APRB"),
            (bystander_corp, bystander_ceo, "ByRbac", "BYRB"),
            (member_corp, member_ceo, "MemRbac", "MMRB"),
        ] {
            let mut corp = eve_db::CorporationRecord::default();
            corp.corporation_id = corp_id;
            corp.name = name.into();
            corp.ticker = ticker.into();
            corp.ceo_id = ceo_id;
            corp.wallet_divisions = CorporationRecord::default_wallet_divisions(IskCents::from_isk(5_000_000_000));
            corp.members.insert(ceo_id);
            if corp_id == exec_corp {
                corp.members.extend([dir_char, plain_char]);
                corp.member_roles
                    .insert(dir_char, eve_db::corp_roles::ROLE_DIRECTOR);
            }
            snap.corporations.insert(corp_id, corp);
        }
    });

    let mut exec_sess = SessionState {
        user_id: Some(exec_char as u64),
        character_id: Some(exec_char),
        corporation_id: Some(exec_corp),
        solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760),
        ..Default::default()
    };
    let created = call_social(
        &ctx,
        &mut exec_sess,
        "corpRegistry",
        "CreateAlliance",
        vec![
            PyRep::String("RBAC Alliance".into()),
            PyRep::String("RBAL".into()),
            PyRep::String("desc".into()),
            PyRep::String("https://rbac.example".into()),
        ],
    )
    .await
    .unwrap();
    let aid = kv(&created, "allianceID").as_i64().unwrap();
    exec_sess.alliance_id = Some(aid);

    let mut app_sess = SessionState {
        user_id: Some(applicant_ceo as u64),
        character_id: Some(applicant_ceo),
        corporation_id: Some(applicant_corp),
        solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760),
        ..Default::default()
    };
    call_social(
        &ctx,
        &mut app_sess,
        "corpRegistry",
        "ApplyToJoinAlliance",
        vec![PyRep::Integer(aid), PyRep::String("App text".into())],
    )
    .await
    .unwrap();

    // Add a non-executor member corp and verify it cannot see incoming applications of other corps.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(a) = snap.alliances.get_mut(&aid) {
            a.member_corp_ids.insert(member_corp);
        }
        if let Some(c) = snap.corporations.get_mut(&member_corp) {
            c.alliance_id = Some(aid);
        }
    });
    let mut member_sess = SessionState {
        user_id: Some(member_ceo as u64),
        character_id: Some(member_ceo),
        corporation_id: Some(member_corp),
        alliance_id: Some(aid),
        solar_system_id: Some(30_000_142),
        ..Default::default()
    };
    for svc in ["corpRegistry", "allianceRegistry"] {
        let m_all = call_social(&ctx, &mut member_sess, svc, "GetAllianceApplications", vec![])
            .await
            .unwrap();
        assert_eq!(rs_rows(&m_all).len(), 0);
        let m_targeted = call_social(
            &ctx,
            &mut member_sess,
            svc,
            "GetAllianceApplications",
            vec![PyRep::Integer(aid)],
        )
        .await
        .unwrap();
        assert_eq!(rs_rows(&m_targeted).len(), 0);
    }

    // Exec director & applicant CEO succeed on both corpRegistry and allianceRegistry.
    let mut dir_sess = SessionState {
        user_id: Some(dir_char as u64),
        character_id: Some(dir_char),
        corporation_id: Some(exec_corp),
        alliance_id: Some(aid),
        solar_system_id: Some(30_000_142),
        ..Default::default()
    };
    for svc in ["corpRegistry", "allianceRegistry"] {
        let dir_rows = call_social(&ctx, &mut dir_sess, svc, "GetAllianceApplications", vec![])
            .await
            .unwrap();
        assert_eq!(rs_rows(&dir_rows).len(), 1);
        let app_rows = call_social(
            &ctx,
            &mut app_sess,
            svc,
            "GetAllianceApplications",
            vec![PyRep::Integer(aid)],
        )
        .await
        .unwrap();
        assert_eq!(rs_rows(&app_rows).len(), 1);
    }

    // Seed a due Accepted timer on `aid`; an unauthorized bystander query must NOT trigger sync_alliance_timers.
    let due_corp = 98_100_905;
    super::with_alliance_war_state(&ctx, |st| {
        if let Some(m) = st.alliances.get_mut(&aid) {
            m.applications.push((
                due_corp,
                "Due pending".into(),
                1_700_000_000,
                "Accepted".into(),
                Some(1_700_000_010),
            ));
        }
    });
    clock.advance_secs(20);

    // Roleless member in executor corp, NPC corp pilot, and foreign bystander corp are denied.
    let mut plain_sess = SessionState {
        user_id: Some(plain_char as u64),
        character_id: Some(plain_char),
        corporation_id: Some(exec_corp),
        alliance_id: Some(aid),
        solar_system_id: Some(30_000_142),
        ..Default::default()
    };
    let mut npc_sess = SessionState {
        user_id: Some(npc_char as u64),
        character_id: Some(npc_char),
        corporation_id: Some(npc_corp),
        solar_system_id: Some(30_000_142),
        ..Default::default()
    };
    let mut bystander_sess = SessionState {
        user_id: Some(bystander_ceo as u64),
        character_id: Some(bystander_ceo),
        corporation_id: Some(bystander_corp),
        solar_system_id: Some(30_000_142),
        ..Default::default()
    };

    for svc in ["corpRegistry", "allianceRegistry"] {
        assert_eq!(
            call_social(&ctx, &mut plain_sess, svc, "GetAllianceApplications", vec![])
                .await
                .unwrap_err()
                .code,
            "CorpRoleDenied"
        );
        assert_eq!(
            call_social(&ctx, &mut npc_sess, svc, "GetAllianceApplications", vec![])
                .await
                .unwrap_err()
                .code,
            "CorpRoleDenied"
        );
        assert_eq!(
            call_social(
                &ctx,
                &mut bystander_sess,
                svc,
                "GetAllianceApplications",
                vec![PyRep::Integer(aid)],
            )
            .await
            .unwrap_err()
            .code,
            "CrpAccessDenied"
        );
        let bystander_unfiltered = call_social(&ctx, &mut bystander_sess, svc, "GetAllianceApplications", vec![])
            .await
            .unwrap();
        assert_eq!(rs_rows(&bystander_unfiltered).len(), 0);
        // Due timer must still be "Accepted" (not matured by unauthorized bystander queries).
        let still_accepted = super::read_alliance_war_state(&ctx, |st| {
            st.alliances
                .get(&aid)
                .is_some_and(|m| m.applications.iter().any(|app| app.0 == due_corp && app.3 == "Accepted"))
        });
        assert!(still_accepted, "unauthorized GetAllianceApplications must not sync alliance timers");
        assert_eq!(
            call_social(
                &ctx,
                &mut bystander_sess,
                svc,
                "GetAllianceApplications",
                vec![PyRep::Integer(99_999_999)],
            )
            .await
            .unwrap_err()
            .code,
            "AllianceNotFound"
        );
    }

    // An alliance existing only in snap.alliances returns empty rowset for its executor (not AllianceNotFound),
    // and denies unrelated directors with CrpAccessDenied.
    let snap_only_aid = 99_900_999;
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            snap_only_aid,
            eve_db::AllianceRecord {
                alliance_id: snap_only_aid,
                name: "SnapOnly Alliance".into(),
                short_name: "SNAP".into(),
                executor_corp_id: bystander_corp,
                member_corp_ids: HashSet::from([bystander_corp]),
            },
        );
        if let Some(c) = snap.corporations.get_mut(&bystander_corp) {
            c.alliance_id = Some(snap_only_aid);
        }
    });
    bystander_sess.alliance_id = Some(snap_only_aid);
    for svc in ["corpRegistry", "allianceRegistry"] {
        let snap_rows = call_social(
            &ctx,
            &mut bystander_sess,
            svc,
            "GetAllianceApplications",
            vec![PyRep::Integer(snap_only_aid)],
        )
        .await
        .unwrap();
        assert_eq!(rs_rows(&snap_rows).len(), 0);
        assert_eq!(
            call_social(
                &ctx,
                &mut app_sess,
                svc,
                "GetAllianceApplications",
                vec![PyRep::Integer(snap_only_aid)],
            )
            .await
            .unwrap_err()
            .code,
            "CrpAccessDenied"
        );
    }
}

#[tokio::test]
async fn require_director_and_executor_use_db_first_and_enforce_corp_members() {
    let (ctx, _clock, mut exec, _memb, _) = setup_alliance_sandbox();
    let created = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "CreateAlliance",
        vec![PyRep::String("DB Guard Alliance".into()), PyRep::String("DBGA".into())],
    )
    .await
    .unwrap();
    let aid = created.dict_get("allianceID").unwrap().as_i64().unwrap();
    exec.alliance_id = Some(aid);

    let exec_corp = 98_100_001_i64;
    let omitted_char = 91_000_099_i64;
    let npc_char = 91_000_100_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            omitted_char,
            eve_db::CharacterRecord {
                character_id: omitted_char,
                corporation_id: exec_corp,
                alliance_id: Some(aid),
                ..Default::default()
            },
        );
        if let Some(corp) = snap.corporations.get_mut(&exec_corp) {
            corp.member_roles
                .insert(omitted_char, eve_db::corp_roles::ROLE_DIRECTOR);
        }
        snap.characters.insert(
            npc_char,
            eve_db::CharacterRecord {
                character_id: npc_char,
                corporation_id: 1_000_009,
                alliance_id: None,
                ..Default::default()
            },
        );
    });

    // Omitted from non-empty corp.members is denied director & executor RPCs even with ROLE_DIRECTOR.
    let mut omitted_sess = SessionState::new(91_099);
    omitted_sess.user_id = Some(99);
    omitted_sess.character_id = Some(omitted_char);
    omitted_sess.corporation_id = Some(exec_corp);
    omitted_sess.alliance_id = Some(aid);
    omitted_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        call_social(&ctx, &mut omitted_sess, "allianceRegistry", "GetAllianceApplications", vec![])
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut omitted_sess,
            "allianceRegistry",
            "CreateLabel",
            vec![PyRep::String("Denied".into()), PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Stale session corporation_id / alliance_id cannot spoof executor or director status over DB character.
    let mut stale_sess = SessionState::new(91_100);
    stale_sess.user_id = Some(100);
    stale_sess.character_id = Some(npc_char);
    stale_sess.corporation_id = Some(exec_corp);
    stale_sess.alliance_id = Some(aid);
    stale_sess.corp_role = u64::MAX;
    assert_eq!(
        call_social(&ctx, &mut stale_sess, "allianceRegistry", "GetAllianceApplications", vec![])
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut stale_sess,
            "allianceRegistry",
            "CreateLabel",
            vec![PyRep::String("Denied".into()), PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "NotInAlliance"
    );
}

#[tokio::test]
async fn alliance_kick_and_delete_member_db_first_rbac_blocks_stale_session() {
    let (ctx, _clock, mut exec, memb, _) = setup_alliance_sandbox();
    let created = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "CreateAlliance",
        vec![PyRep::String("Kick Leave Guard".into()), PyRep::String("KLG".into())],
    )
    .await
    .unwrap();
    let aid = created.dict_get("allianceID").unwrap().as_i64().unwrap();
    let exec_corp = exec.corporation_id.unwrap();
    let memb_corp = memb.corporation_id.unwrap();
    let memb_char = memb.character_id.unwrap();
    let non_dir_char = 91_000_201_i64;
    let rogue_corp = 98_100_299_i64;
    let rogue_char = 91_000_202_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(a) = snap.alliances.get_mut(&aid) {
            a.member_corp_ids.insert(memb_corp);
        }
        if let Some(c) = snap.corporations.get_mut(&memb_corp) {
            c.alliance_id = Some(aid);
        }
        if let Some(ch) = snap.characters.get_mut(&memb_char) {
            ch.alliance_id = Some(aid);
        }
        if let Some(c) = snap.corporations.get_mut(&exec_corp) {
            c.members.insert(non_dir_char);
        }
        snap.characters.insert(
            non_dir_char,
            eve_db::CharacterRecord {
                character_id: non_dir_char,
                corporation_id: exec_corp,
                alliance_id: Some(aid),
                ..Default::default()
            },
        );
        let mut rcorp = eve_db::CorporationRecord::default();
        rcorp.corporation_id = rogue_corp;
        rcorp.name = "Rogue Corp".into();
        rcorp.ticker = "ROG".into();
        rcorp.ceo_id = rogue_char;
        rcorp.members.insert(rogue_char);
        snap.corporations.insert(rogue_corp, rcorp);
        snap.characters.insert(
            rogue_char,
            eve_db::CharacterRecord {
                character_id: rogue_char,
                corporation_id: rogue_corp,
                alliance_id: None,
                ..Default::default()
            },
        );
    });

    // 1. Non-director member of executor corp with stale session.corp_role = ROLE_DIRECTOR is denied KickMember & DeleteMember.
    let mut non_dir_sess = SessionState::new(91_201);
    non_dir_sess.user_id = Some(201);
    non_dir_sess.character_id = Some(non_dir_char);
    non_dir_sess.corporation_id = Some(exec_corp);
    non_dir_sess.alliance_id = Some(aid);
    non_dir_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        call_social(&ctx, &mut non_dir_sess, "allianceRegistry", "KickMember", vec![PyRep::Integer(memb_corp)])
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_social(&ctx, &mut non_dir_sess, "allianceRegistry", "DeleteMember", vec![PyRep::Integer(exec_corp)])
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    // 2. Non-executor member corp CEO spoofing session.corporation_id = exec_corp cannot KickMember or withdraw exec_corp.
    let mut spoof_exec_sess = memb.clone();
    spoof_exec_sess.corporation_id = Some(exec_corp);
    spoof_exec_sess.alliance_id = Some(aid);
    spoof_exec_sess.corp_role = u64::MAX;
    assert_eq!(
        call_social(&ctx, &mut spoof_exec_sess, "allianceRegistry", "KickMember", vec![PyRep::Integer(memb_corp)])
            .await
            .unwrap_err()
            .code,
        "NotAllianceExecutor"
    );
    assert_eq!(
        call_social(&ctx, &mut spoof_exec_sess, "allianceRegistry", "DeleteMember", vec![PyRep::Integer(exec_corp)])
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    // 3. Outside corp CEO spoofing session.corporation_id = exec_corp & session.alliance_id = aid is denied both.
    let mut rogue_sess = SessionState::new(91_202);
    rogue_sess.user_id = Some(202);
    rogue_sess.character_id = Some(rogue_char);
    rogue_sess.corporation_id = Some(exec_corp);
    rogue_sess.alliance_id = Some(aid);
    rogue_sess.corp_role = u64::MAX;
    assert_eq!(
        call_social(&ctx, &mut rogue_sess, "allianceRegistry", "KickMember", vec![PyRep::Integer(memb_corp)])
            .await
            .unwrap_err()
            .code,
        "NotInAlliance"
    );
    assert_eq!(
        call_social(&ctx, &mut rogue_sess, "allianceRegistry", "DeleteMember", vec![PyRep::Integer(rogue_corp)])
            .await
            .unwrap_err()
            .code,
        "NotInAlliance"
    );

    // 4. Legitimate executor CEO with stale/cleared session.alliance_id resolves DB alliance for KickMember & DeleteMember.
    let mut exec_stale_sess = exec.clone();
    exec_stale_sess.alliance_id = Some(99_999_999);
    let kicked = call_social(
        &ctx,
        &mut exec_stale_sess,
        "allianceRegistry",
        "KickMember",
        vec![PyRep::Integer(memb_corp)],
    )
    .await
    .unwrap();
    assert_eq!(kicked.dict_get("allianceID").and_then(PyRep::as_i64), Some(aid));
    assert_eq!(kicked.dict_get("removed").and_then(PyRep::as_bool), Some(true));

    exec_stale_sess.alliance_id = None;
    let leave = call_social(
        &ctx,
        &mut exec_stale_sess,
        "allianceRegistry",
        "DeleteMember",
        vec![PyRep::Integer(exec_corp)],
    )
    .await
    .unwrap();
    assert_eq!(leave.dict_get("allianceID").and_then(PyRep::as_i64), Some(aid));
    assert_eq!(leave.dict_get("corporationID").and_then(PyRep::as_i64), Some(exec_corp));
}
