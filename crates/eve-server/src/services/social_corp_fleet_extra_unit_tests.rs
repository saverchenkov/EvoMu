//! Extra unit and mutation-killing tests for `social_corp_fleet.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::social_corp_fleet::handle;
use eve_db::{
    CorpWalletDivisionRecord, CorporationRecord, IskCents,
    VirtualServerClock, WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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

pub(super) fn rs_rows(rep: &PyRep) -> &[Vec<PyRep>] {
    match rep {
        PyRep::Rowset { rows, .. } => rows,
        _ => panic!("expected Rowset"),
    }
}

pub(super) fn setup_sandbox() -> (ServerContext, SessionState, SessionState) {
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000));
    let ctx = ServerContext::new();
    ctx.set_clock(clock);
    ctx.db.with_mut_snapshot(|snap: &mut WorldStateSnapshot| {
        let tpl = snap.characters.values().next().unwrap().clone();
        for (cid, name, corp_id) in [
            (91_000_001_i64, "Exec CEO", 98_100_001_i64),
            (91_000_002_i64, "Member CEO", 98_100_002_i64),
            (91_000_003_i64, "Line Member", 98_100_001_i64),
        ] {
            let mut ch = tpl.clone();
            ch.character_id = cid;
            ch.name = name.to_string();
            ch.corporation_id = corp_id;
            ch.alliance_id = None;
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
            ch.active_ship_id = 55_000_000 + cid;
            ch.balance = IskCents::from_isk(5_000_000_000);
            snap.characters.insert(cid, ch);
        }
        for (corp_id, ceo_id, ticker, members) in [
            (98_100_001_i64, 91_000_001_i64, "EXEC", vec![91_000_001, 91_000_003]),
            (98_100_002_i64, 91_000_002_i64, "MEMB", vec![91_000_002]),
        ] {
            let mut divs =
                CorporationRecord::default_wallet_divisions(IskCents::from_isk(5_000_000_000));
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
                    ticker: ticker.into(),
                    ceo_id,
                    alliance_id: None,
                    tax_rate: 0.05,
                    master_wallet: IskCents::from_isk(5_000_000_000),
                    members: members.into_iter().collect::<HashSet<_>>(),
                    member_roles: HashMap::from([(ceo_id, u64::MAX), (91_000_003, 0)]),
                    applications: HashMap::new(),
                    wallet_divisions: divs,
                    description: String::new(),
                    url: String::new(),
                    hq_station_id: Some(60_003_760),
                },
            );
        }
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

    (ctx, exec, memb)
}

#[tokio::test]
async fn fleet_init_and_alliance_creation_application_error_branches() {
    let (ctx, mut exec, mut memb) = setup_sandbox();

    // 1. fleetSvc.InitFleet: stale fleet_id is cleaned up and new fleet created; second call -> AlreadyInFleet
    exec.fleet_id = Some(999_888_777);
    let fl = call_social(&ctx, &mut exec, "fleetSvc", "InitFleet", vec![])
        .await
        .unwrap();
    assert!(fl.dict_get("fleetID").and_then(|v| v.as_i64()).is_some());
    assert_eq!(
        call_social(&ctx, &mut exec, "fleetSvc", "InitFleet", vec![])
            .await
            .unwrap_err()
            .code,
        "AlreadyInFleet"
    );

    // 2. CreateAlliance preconditions: CorpRoleDenied, CorporationNotFound, non-CEO, InsufficientFunds, duplicate name/shortName
    let saved_corp = exec.corporation_id;
    exec.corporation_id = Some(1_000_009);
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "CreateAlliance",
            vec![PyRep::String("Valid".into()), PyRep::String("VAL".into())],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    exec.corporation_id = Some(99_999_999);
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "CreateAlliance",
            vec![PyRep::String("Valid".into()), PyRep::String("VAL".into())],
        )
        .await
        .unwrap_err()
        .code,
        "CorporationNotFound"
    );
    exec.corporation_id = saved_corp;

    let mut line_sess = exec.clone();
    line_sess.character_id = Some(91_000_003);
    assert_eq!(
        call_social(
            &ctx,
            &mut line_sess,
            "allianceRegistry",
            "CreateAlliance",
            vec![PyRep::String("Valid".into()), PyRep::String("VAL".into())],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // InsufficientFunds when wallet < 1B ISK
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&98_100_001).unwrap();
        corp.master_wallet = IskCents::from_isk(100_000);
        corp.wallet_divisions[0].balance = IskCents::from_isk(100_000);
    });
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "CreateAlliance",
            vec![PyRep::String("Valid".into()), PyRep::String("VAL".into())],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&98_100_001).unwrap();
        corp.master_wallet = IskCents::from_isk(5_000_000_000);
        corp.wallet_divisions[0].balance = IskCents::from_isk(5_000_000_000);
    });

    let created = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "CreateAlliance",
        vec![PyRep::String("Northern Coalition".into()), PyRep::String("NC".into())],
    )
    .await
    .unwrap();
    let aid = created.dict_get("allianceID").unwrap().as_i64().unwrap();

    // AlreadyInAlliance & AllianceNameTaken
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "CreateAlliance",
            vec![PyRep::String("Other".into()), PyRep::String("OTH".into())],
        )
        .await
        .unwrap_err()
        .code,
        "AlreadyInAlliance"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut memb,
            "allianceRegistry",
            "CreateAlliance",
            vec![PyRep::String("Northern Coalition".into()), PyRep::String("OTH".into())],
        )
        .await
        .unwrap_err()
        .code,
        "AllianceNameTaken"
    );

    // 3. ApplyToJoinAlliance & UpdateApplication error branches
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "corpRegistry",
            "ApplyToJoinAlliance",
            vec![PyRep::Integer(aid), PyRep::String("Hi".into())],
        )
        .await
        .unwrap_err()
        .code,
        "AlreadyInAlliance"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut memb,
            "corpRegistry",
            "ApplyToJoinAlliance",
            vec![PyRep::Integer(999_888_777), PyRep::String("Hi".into())],
        )
        .await
        .unwrap_err()
        .code,
        "AllianceNotFound"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut memb,
            "allianceRegistry",
            "UpdateApplication",
            vec![PyRep::Integer(98_100_002), PyRep::String("Note".into()), PyRep::Integer(2)],
        )
        .await
        .unwrap_err()
        .code,
        "NotInAlliance"
    );
}
