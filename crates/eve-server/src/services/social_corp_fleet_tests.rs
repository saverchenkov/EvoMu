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
