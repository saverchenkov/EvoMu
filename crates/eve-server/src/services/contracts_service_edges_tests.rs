//! Behavioral edge-case tests for `contracts_service.rs`.

#![forbid(unsafe_code)]

use super::handle;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use eve_db::{
    AccountRecord, CorporationRecord, InventoryFlag, IskCents, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn call_contract(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: Vec<PyRep>,
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    handle(ctx, sess, "contractProxy", method, &args, kwargs)
        .unwrap_or_else(|| panic!("unhandled contractProxy.{method}"))
}

fn setup_sandbox() -> (
    ServerContext,
    SessionState,
    i64,
    SessionState,
    i64,
    SessionState,
    i64,
    i64,
    i64,
) {
    let (mut ctx, mut issuer_sess, issuer_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let station_id = 1_000_000_000_001_i64;
    let dest_station_id = 1_000_000_000_002_i64;
    let acceptor_id = 90_000_002_i64;
    let acceptor_ship_id = 100_000_002_i64;
    let stranger_id = 90_000_003_i64;
    let stranger_ship_id = 100_000_003_i64;
    let acceptor_corp = 98_300_001_i64;

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut
        .types
        .insert(34, make_fighter_type(34, 18, 4, 0.01, vec![]));
    sde_mut
        .types
        .insert(35, make_fighter_type(35, 18, 4, 0.01, vec![]));

    issuer_sess.station_id = Some(station_id);
    let mut acceptor_sess = issuer_sess.clone();
    acceptor_sess.character_id = Some(acceptor_id);
    acceptor_sess.user_id = Some(42);
    acceptor_sess.corporation_id = Some(acceptor_corp);
    acceptor_sess.corp_role = 0;
    acceptor_sess.ship_id = Some(acceptor_ship_id);

    let mut stranger_sess = issuer_sess.clone();
    stranger_sess.character_id = Some(stranger_id);
    stranger_sess.user_id = Some(43);
    stranger_sess.corporation_id = Some(98_300_002);
    stranger_sess.corp_role = 0;
    stranger_sess.ship_id = Some(stranger_ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.get(&issuer_id).cloned().unwrap();
        if let Some(ch) = snap.characters.get_mut(&issuer_id) {
            ch.station_id = Some(station_id);
            ch.balance = IskCents::from_isk(500_000_000);
        }
        // Ensure issuer account has role=0 (non-GM) for GM_ExpireContract test
        for acc in snap.accounts.values_mut() {
            if acc.user_id == base_char.user_id {
                acc.role = 0;
            }
        }
        for (cid, uid, name, corp_id, ship_id) in [
            (acceptor_id, 42_u64, "Acceptor", acceptor_corp, acceptor_ship_id),
            (stranger_id, 43_u64, "Stranger", 98_300_002, stranger_ship_id),
        ] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.name = name.into();
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(500_000_000);
            snap.characters.insert(cid, ch);
            snap.accounts.insert(
                name.to_lowercase(),
                AccountRecord {
                    user_id: uid,
                    username: name.to_lowercase(),
                    password_token: "h".into(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            snap.items.insert(
                ship_id,
                make_fighter_item(ship_id, 600, cid, station_id, InventoryFlag::Hangar, 1),
            );
        }
        snap.corporations.insert(
            acceptor_corp,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = acceptor_corp; __corp.name = "Acceptor Corp".into(); __corp.ticker = "ACCC".into(); __corp.ceo_id = stranger_id; __corp.alliance_id = None; __corp.tax_rate = 0.05; __corp.members = HashSet::from([acceptor_id, stranger_id]); __corp.member_roles = HashMap::from([(acceptor_id, 0u64), (stranger_id, u64::MAX)]); __corp.applications = HashMap::new(); __corp.wallet_divisions = CorporationRecord::default_wallet_divisions(
                    IskCents::from_isk(500_000_000),
                ); __corp.description = String::new(); __corp.url = String::new(); __corp.hq_station_id = Some(station_id); let __mw = IskCents::from_isk(500_000_000); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp },
        );
        for sid in [station_id, dest_station_id] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35832,
                    owner_corp_id: 1_000_009,
                    solar_system_id: sys_id,
                    name: format!("Citadel {sid}"),
                    state: "ShieldVulnerable".into(),
                    position: [0.0; 3],
                    quantum_core_installed: true,
                    ..Default::default()
                },
            );
        }
    });

    (
        ctx,
        issuer_sess,
        issuer_id,
        acceptor_sess,
        acceptor_id,
        stranger_sess,
        stranger_id,
        station_id,
        dest_station_id,
    )
}

#[test]
fn reject_fail_delete_and_gm_expire_contract_edge_cases() {
    let (
        ctx,
        mut issuer_sess,
        issuer_id,
        mut acceptor_sess,
        acceptor_id,
        mut stranger_sess,
        _stranger_id,
        station_id,
        dest_station_id,
    ) = setup_sandbox();
    let kw = HashMap::new();
    let acceptor_corp = 98_300_001_i64;

    let (item_personal, item_corp, item_courier, item_exp) =
        (710_001_i64, 710_002_i64, 710_003_i64, 710_004_i64);
    ctx.db.with_mut_snapshot(|snap| {
        for iid in [item_personal, item_corp, item_courier, item_exp] {
            snap.items.insert(
                iid,
                make_fighter_item(iid, 34, issuer_id, station_id, InventoryFlag::Hangar, 5),
            );
        }
    });

    // 1. Private ItemExchange assigned to acceptor -> stranger cannot reject; acceptor rejects and restores item + reward/deposit
    let issuer_bal_before = ctx
        .db
        .with_snapshot(|s| s.characters.get(&issuer_id).unwrap().balance.0);
    let cid_personal = call_contract(
        &ctx,
        &mut issuer_sess,
        "CreateContract",
        vec![PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("availability", PyRep::Integer(1)),
            ("assigneeID", PyRep::Integer(acceptor_id)),
            ("price", PyRep::Real(0.0)),
            ("reward", PyRep::Real(250_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(item_personal)])),
            ("title", PyRep::String("Private Offer".into())),
        ])],
        &kw,
    )
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();

    assert_eq!(
        call_contract(
            &ctx,
            &mut stranger_sess,
            "RejectContract",
            vec![PyRep::Integer(cid_personal)],
            &kw,
        )
        .unwrap_err()
        .code,
        "ContractOwnershipViolation"
    );
    assert_eq!(
        call_contract(
            &ctx,
            &mut stranger_sess,
            "GetContractInfo",
            vec![PyRep::Integer(cid_personal)],
            &kw,
        )
        .unwrap_err()
        .code,
        "ContractAccessDenied"
    );

    let rej = call_contract(
        &ctx,
        &mut acceptor_sess,
        "RejectContract",
        vec![PyRep::Integer(cid_personal)],
        &kw,
    )
    .unwrap();
    assert_eq!(rej.dict_get("status").and_then(|v| v.as_str()), Some("Rejected"));
    ctx.db.with_snapshot(|s| {
        let it = s.items.get(&item_personal).unwrap();
        assert_eq!(it.owner_id, issuer_id);
        assert_eq!(it.flag, InventoryFlag::Hangar);
        // Deposit (10_000 ISK min non-refundable broker fee) stays deducted; reward (250_000 ISK) is refunded
        assert_eq!(
            s.characters.get(&issuer_id).unwrap().balance.0,
            issuer_bal_before - IskCents::from_isk(10_000).0
        );
    });
    // Cannot reject a second time once status != Outstanding
    assert_eq!(
        call_contract(
            &ctx,
            &mut acceptor_sess,
            "RejectContract",
            vec![PyRep::Integer(cid_personal)],
            &kw,
        )
        .unwrap_err()
        .code,
        "ContractOwnershipViolation"
    );

    // 2. Contract assigned to acceptor_corp: non-director corp member lacks role -> CrpAccessDenied
    let cid_corp = call_contract(
        &ctx,
        &mut issuer_sess,
        "CreateContract",
        vec![PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("availability", PyRep::Integer(1)),
            ("assigneeID", PyRep::Integer(acceptor_corp)),
            ("price", PyRep::Real(100_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(item_corp)])),
        ])],
        &kw,
    )
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();
    assert_eq!(
        call_contract(
            &ctx,
            &mut acceptor_sess,
            "RejectContract",
            vec![PyRep::Integer(cid_corp)],
            &kw,
        )
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // 3. FailContract on InProgress Courier vs non-Courier; DeleteContract ownership violation
    let cid_courier = call_contract(
        &ctx,
        &mut issuer_sess,
        "CreateContract",
        vec![PyRep::keyval([
            ("contractType", PyRep::Integer(3)),
            ("startStationID", PyRep::Integer(station_id)),
            ("endStationID", PyRep::Integer(dest_station_id)),
            ("reward", PyRep::Real(500_000.0)),
            ("collateral", PyRep::Real(1_000_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(item_courier)])),
        ])],
        &kw,
    )
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();

    // Stranger cannot delete someone else's contract
    assert_eq!(
        call_contract(
            &ctx,
            &mut stranger_sess,
            "DeleteContract",
            vec![PyRep::Integer(cid_courier)],
            &kw,
        )
        .unwrap_err()
        .code,
        "ContractOwnershipViolation"
    );
    // Cannot FailContract while still Outstanding
    assert_eq!(
        call_contract(
            &ctx,
            &mut issuer_sess,
            "FailContract",
            vec![PyRep::Integer(cid_courier)],
            &kw,
        )
        .unwrap_err()
        .code,
        "InvalidContractState"
    );

    // Accept courier, then acceptor calls FailContract -> status Failed
    call_contract(
        &ctx,
        &mut acceptor_sess,
        "AcceptContract",
        vec![PyRep::Integer(cid_courier)],
        &kw,
    )
    .unwrap();
    let failed = call_contract(
        &ctx,
        &mut acceptor_sess,
        "FailContract",
        vec![PyRep::Integer(cid_courier)],
        &kw,
    )
    .unwrap();
    assert_eq!(failed.dict_get("status").and_then(|v| v.as_str()), Some("Failed"));

    // 4. GM_ExpireContract: non-GM denied, missing contract -> ContractNotFound, GM expires -> Expired, then issuer deletes
    let cid_exp = call_contract(
        &ctx,
        &mut issuer_sess,
        "CreateContract",
        vec![PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("price", PyRep::Real(200_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(item_exp)])),
        ])],
        &kw,
    )
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();

    assert_eq!(
        call_contract(
            &ctx,
            &mut issuer_sess,
            "GM_ExpireContract",
            vec![PyRep::Integer(cid_exp)],
            &kw,
        )
        .unwrap_err()
        .code,
        "GMRoleRequired"
    );
    ctx.db.with_mut_snapshot(|snap| {
        for acc in snap.accounts.values_mut() {
            if acc.user_id == issuer_sess.user_id.unwrap() {
                acc.role = eve_db::service_roles::ROLE_GMH;
            }
        }
    });
    assert_eq!(
        call_contract(
            &ctx,
            &mut issuer_sess,
            "GM_ExpireContract",
            vec![PyRep::Integer(999_888)],
            &kw,
        )
        .unwrap_err()
        .code,
        "ContractNotFound"
    );
    let exp_res = call_contract(
        &ctx,
        &mut issuer_sess,
        "GM_ExpireContract",
        vec![PyRep::Integer(cid_exp)],
        &kw,
    )
    .unwrap();
    assert_eq!(exp_res.dict_get("status").and_then(|v| v.as_str()), Some("Expired"));

    let del_exp = call_contract(
        &ctx,
        &mut issuer_sess,
        "DeleteContract",
        vec![PyRep::Integer(cid_exp)],
        &kw,
    )
    .unwrap();
    assert_eq!(del_exp.dict_get("status").and_then(|v| v.as_str()), Some("Deleted"));

    // 5. GetMyExpiredContractList & GetMyCurrentContractList foreign owner guard
    assert_eq!(
        call_contract(
            &ctx,
            &mut stranger_sess,
            "GetMyCurrentContractList",
            vec![PyRep::Integer(issuer_id)],
            &kw,
        )
        .unwrap_err()
        .code,
        "ContractAccessDenied"
    );
    let PyRep::List(expired_rows) =
        call_contract(&ctx, &mut issuer_sess, "GetMyExpiredContractList", vec![], &kw).unwrap()
    else {
        panic!("expected list");
    };
    assert!(expired_rows.iter().any(|r| {
        r.dict_get("contractID").and_then(|v| v.as_i64()) == Some(cid_personal)
    }));
}

#[test]
fn complete_contract_validation_and_auction_bid_buyout_edges() {
    let (
        ctx,
        mut issuer_sess,
        issuer_id,
        mut acceptor_sess,
        acceptor_id,
        mut stranger_sess,
        _stranger_id,
        station_id,
        dest_station_id,
    ) = setup_sandbox();
    let kw = HashMap::new();
    let (item_courier, item_auction) = (710_101_i64, 710_102_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_courier,
            make_fighter_item(item_courier, 34, issuer_id, station_id, InventoryFlag::Hangar, 10),
        );
        snap.items.insert(
            item_auction,
            make_fighter_item(item_auction, 35, issuer_id, station_id, InventoryFlag::Hangar, 1),
        );
    });

    // 1. Courier CompleteContract error guards
    let cid_c = call_contract(
        &ctx,
        &mut issuer_sess,
        "CreateContract",
        vec![PyRep::keyval([
            ("contractType", PyRep::Integer(3)),
            ("startStationID", PyRep::Integer(station_id)),
            ("endStationID", PyRep::Integer(dest_station_id)),
            ("reward", PyRep::Real(100_000.0)),
            ("collateral", PyRep::Real(200_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(item_courier)])),
        ])],
        &kw,
    )
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();

    call_contract(
        &ctx,
        &mut acceptor_sess,
        "AcceptContract",
        vec![PyRep::Integer(cid_c)],
        &kw,
    )
    .unwrap();

    // Still at start station -> CourierNotAtDestinationStation
    assert_eq!(
        call_contract(
            &ctx,
            &mut acceptor_sess,
            "CompleteContract",
            vec![PyRep::Integer(cid_c)],
            &kw,
        )
        .unwrap_err()
        .code,
        "CourierNotAtDestinationStation"
    );

    // At destination station, no ship -> NoActiveShip
    acceptor_sess.station_id = Some(dest_station_id);
    let saved_ship = acceptor_sess.ship_id.take();
    assert_eq!(
        call_contract(
            &ctx,
            &mut acceptor_sess,
            "CompleteContract",
            vec![PyRep::Integer(cid_c)],
            &kw,
        )
        .unwrap_err()
        .code,
        "NoActiveShip"
    );

    // Ship still at start station -> DeliveryShipNotAtDestination
    acceptor_sess.ship_id = saved_ship;
    assert_eq!(
        call_contract(
            &ctx,
            &mut acceptor_sess,
            "CompleteContract",
            vec![PyRep::Integer(cid_c)],
            &kw,
        )
        .unwrap_err()
        .code,
        "DeliveryShipNotAtDestination"
    );

    // Move ship to destination, but remove plastic wrap from ship/dest -> CourierPackageMissing
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&saved_ship.unwrap())
            .unwrap()
            .location_id = dest_station_id;
        for it in snap.items.values_mut() {
            if it.type_id == 3468 {
                it.location_id = station_id; // left behind at start
            }
        }
    });
    assert_eq!(
        call_contract(
            &ctx,
            &mut acceptor_sess,
            "CompleteContract",
            vec![PyRep::Integer(cid_c)],
            &kw,
        )
        .unwrap_err()
        .code,
        "CourierPackageMissing"
    );

    // Expire courier deadline -> ContractExpired
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.get_mut(&cid_c).unwrap().expiration_unix = 1;
    });
    assert_eq!(
        call_contract(
            &ctx,
            &mut acceptor_sess,
            "CompleteContract",
            vec![PyRep::Integer(cid_c)],
            &kw,
        )
        .unwrap_err()
        .code,
        "ContractExpired"
    );

    // 2. Auction PlaceBid: BidTooLow, InsufficientFunds, self-outbid refund, CannotDeleteAuctionWithBids, buyout
    let cid_a = call_contract(
        &ctx,
        &mut issuer_sess,
        "CreateContract",
        vec![PyRep::keyval([
            ("contractType", PyRep::Integer(2)),
            ("price", PyRep::Real(1_000_000.0)),
            ("collateral", PyRep::Real(3_000_000.0)), // buyout = 3M
            ("itemList", PyRep::List(vec![PyRep::Integer(item_auction)])),
            ("title", PyRep::String("Rare Mineral".into())),
        ])],
        &kw,
    )
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();

    // Opening bid below starting price -> BidTooLow
    assert_eq!(
        call_contract(
            &ctx,
            &mut acceptor_sess,
            "PlaceBid",
            vec![PyRep::Integer(cid_a), PyRep::Real(900_000.0)],
            &kw,
        )
        .unwrap_err()
        .code,
        "BidTooLow"
    );

    // Valid opening bid 1_000_000
    call_contract(
        &ctx,
        &mut acceptor_sess,
        "PlaceBid",
        vec![PyRep::Integer(cid_a), PyRep::Real(1_000_000.0)],
        &kw,
    )
    .unwrap();

    // Issuer cannot delete auction once bid exists
    assert_eq!(
        call_contract(
            &ctx,
            &mut issuer_sess,
            "DeleteContract",
            vec![PyRep::Integer(cid_a)],
            &kw,
        )
        .unwrap_err()
        .code,
        "CannotDeleteAuctionWithBids"
    );

    // Subsequent bid below 5% increment (1_050_000 required) -> BidTooLow
    assert_eq!(
        call_contract(
            &ctx,
            &mut stranger_sess,
            "PlaceBid",
            vec![PyRep::Integer(cid_a), PyRep::Real(1_020_000.0)],
            &kw,
        )
        .unwrap_err()
        .code,
        "BidTooLow"
    );

    // Self-raise: acceptor sets balance to exact delta (200_000 ISK) to raise 1.0M -> 1.2M
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&acceptor_id).unwrap().balance = IskCents::from_isk(200_000);
    });
    call_contract(
        &ctx,
        &mut acceptor_sess,
        "PlaceBid",
        vec![PyRep::Integer(cid_a), PyRep::Real(1_200_000.0)],
        &kw,
    )
    .unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters.get(&acceptor_id).unwrap().balance.0),
        0
    );

    // Stranger bid exceeding funds (< buyout so not clamped down) -> InsufficientFunds
    let stranger_id = stranger_sess.character_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&stranger_id).unwrap().balance = IskCents::from_isk(100_000);
    });
    assert_eq!(
        call_contract(
            &ctx,
            &mut stranger_sess,
            "PlaceBid",
            vec![PyRep::Integer(cid_a), PyRep::Real(1_500_000.0)],
            &kw,
        )
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&stranger_id).unwrap().balance = IskCents::from_isk(500_000_000);
    });

    // SearchContracts with itemTypeID & minPrice/maxPrice finds the auction
    let mut skw = HashMap::new();
    skw.insert("itemTypeID".into(), PyRep::Integer(35));
    skw.insert("minPrice".into(), PyRep::Real(500_000.0));
    skw.insert("maxPrice".into(), PyRep::Real(2_000_000.0));
    let PyRep::List(found) =
        call_contract(&ctx, &mut stranger_sess, "SearchContracts", vec![], &skw).unwrap()
    else {
        panic!("expected list");
    };
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].dict_get("contractID").and_then(|v| v.as_i64()),
        Some(cid_a)
    );

    // Stranger buyout (>= 3_000_000) completes auction immediately and transfers item
    let buy = call_contract(
        &ctx,
        &mut stranger_sess,
        "PlaceBid",
        vec![PyRep::Integer(cid_a), PyRep::Real(5_000_000.0)],
        &kw,
    )
    .unwrap();
    assert_eq!(buy.dict_get("status").and_then(|v| v.as_str()), Some("Completed"));
    assert_eq!(buy.dict_get("boughtOut").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(buy.dict_get("currentBidISK").and_then(|v| v.as_f64()), Some(3_000_000.0));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&item_auction).unwrap().owner_id),
        stranger_sess.character_id.unwrap()
    );

    // GetLoginInfo reflects assigned/needsAttention/inProgress and filters dismissed contracts
    let info = call_contract(&ctx, &mut acceptor_sess, "GetLoginInfo", vec![], &kw).unwrap();
    assert!(info.dict_get("assignedToMe").is_some());
    assert!(info.dict_get("needsAttention").is_some());
    assert!(info.dict_get("inProgress").is_some());
}

#[test]
fn contract_notification_tracker_item_changed_and_location_ids_pin_all_fields() {
    let base = make_fighter_item(1, 34, 90_000_001, 60_003_760, InventoryFlag::Hangar, 10);
    assert!(!super::ContractNotifyDelta::item_changed(&base, &base));

    let mut diff_owner = base.clone();
    diff_owner.owner_id = 90_000_002;
    assert!(super::ContractNotifyDelta::item_changed(&base, &diff_owner));

    let mut diff_loc = base.clone();
    diff_loc.location_id = 60_003_761;
    assert!(super::ContractNotifyDelta::item_changed(&base, &diff_loc));

    let mut diff_flag = base.clone();
    diff_flag.flag = InventoryFlag::Cargo;
    assert!(super::ContractNotifyDelta::item_changed(&base, &diff_flag));

    let mut diff_qty = base.clone();
    diff_qty.quantity = 11;
    assert!(super::ContractNotifyDelta::item_changed(&base, &diff_qty));

    let mut diff_sing = base.clone();
    diff_sing.singleton = !base.singleton;
    assert!(super::ContractNotifyDelta::item_changed(&base, &diff_sing));

    let mut diff_tid = base.clone();
    diff_tid.type_id = 35;
    assert!(super::ContractNotifyDelta::item_changed(&base, &diff_tid));

    let (ctx, _, _, _, _, _, _, station_id, _) = setup_sandbox();
    let struct_id = 1_055_777_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                owner_corp_id: 98_000_001,
                type_id: 35832,
                name: "Test Astrahus".into(),
                solar_system_id: 30_000_142,
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    ctx.db.with_snapshot(|snap| {
        let (st_sys, st_reg) = super::contract_location_ids(&ctx.sde, snap, station_id);
        assert_eq!(st_sys, 30_000_142);
        assert!(st_reg > 0);
        let (str_sys, str_reg) = super::contract_location_ids(&ctx.sde, snap, struct_id);
        assert_eq!(str_sys, 30_000_142);
        assert_eq!(str_reg, st_reg);
        assert_eq!(super::contract_location_ids(&ctx.sde, snap, 999_999_999), (0, 0));
    });
}
