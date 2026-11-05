//! Extra unit and mutation-killing tests for `social_corp_fleet.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::social_corp_fleet::handle;
use eve_db::{
    CorpWalletDivisionRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    VirtualServerClock, WarRecord, WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

async fn call_social(
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

fn rs_rows(rep: &PyRep) -> &[Vec<PyRep>] {
    match rep {
        PyRep::Rowset { rows, .. } => rows,
        _ => panic!("expected Rowset"),
    }
}

fn setup_sandbox() -> (ServerContext, SessionState, SessionState) {
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

#[tokio::test]
async fn alliance_getters_kick_war_and_last_corp_close_and_auxiliary_rpcs() {
    let (ctx, mut exec, memb) = setup_sandbox();
    let created = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "CreateAlliance",
        vec![PyRep::String("Star Fraction".into()), PyRep::String("STAR".into())],
    )
    .await
    .unwrap();
    let aid = created.dict_get("allianceID").unwrap().as_i64().unwrap();

    // 1. Direct property getters on allianceRegistry + GetAlliancePublicInfo + GetAllianceMembersOlderThan + GetDaysInAlliance
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "allianceID", vec![])
            .await
            .unwrap(),
        PyRep::Integer(aid)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "shortName", vec![])
            .await
            .unwrap(),
        PyRep::String("STAR".into())
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "executorCorpID", vec![])
            .await
            .unwrap(),
        PyRep::Integer(98_100_001)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "creatorCorpID", vec![])
            .await
            .unwrap(),
        PyRep::Integer(98_100_001)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "creatorCharID", vec![])
            .await
            .unwrap(),
        PyRep::Integer(91_000_001)
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "allowWar", vec![])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "GetAlliancePublicInfo",
            vec![PyRep::Integer(aid)],
        )
        .await
        .unwrap()
        .dict_get("shortName"),
        Some(&PyRep::String("STAR".into()))
    );
    assert_eq!(
        rs_rows(
            &call_social(
                &ctx,
                &mut exec,
                "allianceRegistry",
                "GetAllianceMembersOlderThan",
                vec![PyRep::Integer(aid)],
            )
            .await
            .unwrap()
        )
        .len(),
        1
    );
    assert_eq!(
        call_social(&ctx, &mut exec, "allianceRegistry", "GetDaysInAlliance", vec![])
            .await
            .unwrap(),
        PyRep::Integer(0)
    );

    // 2. Contacts (alliance, character, update existing) & AssignLabels scalar arg ordering & auxiliary alliance RPCs
    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "AddAllianceContact",
        vec![PyRep::Integer(99_000_999), PyRep::Real(5.0)],
    )
    .await
    .unwrap();
    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "EditAllianceContact",
        vec![PyRep::Integer(91_000_002), PyRep::Real(10.0)],
    )
    .await
    .unwrap();
    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "EditContactsRelationshipID",
        vec![PyRep::Integer(91_000_002), PyRep::Real(-5.0)],
    )
    .await
    .unwrap();
    // AssignLabels with (contact_id, label_mask) scalar ordering creates/updates contact
    call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "AssignLabels",
        vec![PyRep::Integer(98_100_002), PyRep::Integer(4)],
    )
    .await
    .unwrap();

    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "AddBulletin",
            vec![PyRep::String("".into()), PyRep::String("Body".into())],
        )
        .await
        .unwrap_err()
        .code,
        "BulletinTitleRequired"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "UpdateBulletin",
            vec![PyRep::Integer(999), PyRep::String("T".into()), PyRep::String("B".into())],
        )
        .await
        .unwrap_err()
        .code,
        "BulletinNotFound"
    );

    for m in [
        "GetAllianceBulletins",
        "GetAllianceSovereigntyStructuresInfo",
        "GetBills",
        "GetBillsReceivable",
        "CancelCapitalSystemTransition",
        "GMForceAcceptApplication",
        "PayBill",
        "SetPrimeHour",
        "UpdateBulletinOrder",
    ] {
        assert!(call_social(&ctx, &mut exec, "allianceRegistry", m, vec![]).await.is_ok());
    }

    // 3. CannotLeaveAllianceDuringWar when active war exists; removing war and deleting last member closes alliance
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            501,
            WarRecord {
                war_id: 501,
                aggressor_corp_id: aid,
                defender_corp_id: 98_100_002,
                mutual: true,
                active: true,
                started_unix: 1_000,
            },
        );
    });
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "allianceRegistry",
            "DeleteMember",
            vec![PyRep::Integer(98_100_001)],
        )
        .await
        .unwrap_err()
        .code,
        "CannotLeaveAllianceDuringWar"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.clear();
    });
    let pending_del = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "DeleteMember",
        vec![PyRep::Integer(98_100_001)],
    )
    .await
    .unwrap();
    assert_eq!(pending_del.dict_get("allianceClosed"), Some(&PyRep::Bool(false)));

    let closed = call_social(
        &ctx,
        &mut exec,
        "allianceRegistry",
        "KickMember",
        vec![PyRep::Integer(98_100_001)],
    )
    .await
    .unwrap();
    assert_eq!(closed.dict_get("allianceClosed"), Some(&PyRep::Bool(true)));
    let _ = memb;
}

#[tokio::test]
async fn deliver_to_corp_hangar_and_member_error_guards() {
    let (ctx, mut exec, _memb) = setup_sandbox();

    // 1. NotDocked guard
    exec.station_id = None;
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "invbroker",
            "DeliverToCorpHangar",
            vec![PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "NotDocked"
    );
    exec.station_id = Some(60_003_760);

    // 2. Empty item list -> ItemNotFound
    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "invbroker",
            "DeliverToCorpHangar",
            vec![PyRep::List(vec![])],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    // 3. TargetNotInCorporation for DeliverToCorpMember
    let valid_item = 66_100_001_i64;
    let container_id = 66_100_002_i64;
    let inside_item = 66_100_003_i64;
    let courier_item = 66_100_004_i64;
    let fitted_item = 66_100_005_i64;
    let corp_div_item = 66_100_006_i64;
    let active_ship = 55_000_000 + 91_000_001;

    ctx.db.with_mut_snapshot(|snap| {
        for (id, owner, loc, flag, typ) in [
            (valid_item, 91_000_001, 60_003_760, InventoryFlag::Hangar, 34_u32),
            (container_id, 91_000_001, 60_003_760, InventoryFlag::Hangar, 3_293),
            (inside_item, 91_000_001, container_id, InventoryFlag::Cargo, 34),
            (courier_item, 91_000_001, 60_003_760, InventoryFlag::Hangar, 24_445),
            (fitted_item, 91_000_001, 60_003_760, InventoryFlag::HiSlot0, 34),
            (corp_div_item, 98_100_001, 60_003_760, InventoryFlag::CorpSAG2, 34),
            (active_ship, 91_000_001, 60_003_760, InventoryFlag::Hangar, 606),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: typ,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.item_custom_names
            .insert(courier_item, "courier_cargo:test".into());
    });

    assert_eq!(
        call_social(
            &ctx,
            &mut exec,
            "invbroker",
            "DeliverToCorpMember",
            vec![PyRep::Integer(valid_item), PyRep::Integer(91_000_002)],
        )
        .await
        .unwrap_err()
        .code,
        "TargetNotInCorporation"
    );

    // 4. ItemInsideContainer, ItemCannotBeTransferred (courier & fitted), CannotDeliverActiveShip
    for (item_id, expected_code) in [
        (inside_item, "ItemInsideContainer"),
        (courier_item, "ItemCannotBeTransferred"),
        (active_ship, "CannotDeliverActiveShip"),
        (fitted_item, "ItemCannotBeTransferred"),
    ] {
        assert_eq!(
            call_social(
                &ctx,
                &mut exec,
                "invbroker",
                "DeliverToCorpHangar",
                vec![PyRep::Integer(item_id)],
            )
            .await
            .unwrap_err()
            .code,
            expected_code
        );
    }

    // 5. Line member without Trader/division-take role cannot deliver corp-owned item
    let mut line_sess = exec.clone();
    line_sess.character_id = Some(91_000_003);
    line_sess.corp_role = 0;
    assert_eq!(
        call_social(
            &ctx,
            &mut line_sess,
            "invbroker",
            "DeliverToCorpMember",
            vec![PyRep::Integer(corp_div_item), PyRep::Integer(91_000_001)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
}
