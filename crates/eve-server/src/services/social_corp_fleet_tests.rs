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
                CorporationRecord {
                    corporation_id: corp_id,
                    name: format!("Corp {corp_id}"),
                    ticker: if corp_id == 98_100_001 { "EXEC".into() } else { "MEMB".into() },
                    ceo_id: cid,
                    alliance_id: None,
                    tax_rate: 0.05,
                    master_wallet: IskCents::from_isk(5_000_000_000),
                    members: HashSet::from([cid]),
                    member_roles: HashMap::from([(cid, u64::MAX)]),
                    applications: HashMap::new(),
                    wallet_divisions: divs,
                    description: String::new(),
                    url: String::new(),
                    hq_station_id: Some(60_003_760),
                },
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

#[tokio::test]
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
        ctx.db.with_snapshot(|s| s.corporations[&98_100_001].master_wallet.0),
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

#[tokio::test]
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
