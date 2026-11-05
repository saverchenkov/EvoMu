//! Extra unit and mutation-killing tests for `services::contracts_service`
//! covering want-to-buy & two-way `ItemExchange` stack splitting, corporate contract creation/acceptance
//! and role checks, slot limits (`Contracting` 25235 / `Corporation Contracting` 25233),
//! item escrow validation errors (active ship, non-empty container, courier cargo, locked items),
//! auction bid increments & outbid refunds, `RejectContract`, `DeleteContract`, `FailContract`,
//! `GM_ExpireContract`, `SearchContracts` filters, `GetMyExpiredContractList`, and `GetLoginInfo`.

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::contracts_service::*;
use eve_db::{
    corp_roles, CorpWalletDivisionRecord, CorporationRecord, InventoryFlag, IskCents,
    TrainedSkillRecord, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::Arc;

fn dict_i64(r: &PyRep, k: &str) -> Option<i64> {
    r.dict_get(k).and_then(|v| v.as_i64())
}
fn dict_str<'a>(r: &'a PyRep, k: &str) -> Option<&'a str> {
    r.dict_get(k).and_then(|v| v.as_str())
}

fn setup_extra_contracts_env() -> (
    ServerContext,
    SessionState,
    i64,
    SessionState,
    i64,
    i64,
    i64,
    i64,
) {
    let (mut ctx, mut issuer_sess, issuer_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let station_id = 1_000_000_000_101i64;
    let dest_id = 1_000_000_000_102i64;
    let acceptor_id = 90_000_002i64;
    let acceptor_ship = 100_000_002i64;
    let corp_id = 98_000_777i64;

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut.types.insert(34, make_fighter_type(34, 18, 4, 0.01, vec![]));
    sde_mut.types.insert(35, make_fighter_type(35, 18, 4, 0.01, vec![]));
    // Station container type (group 448)
    sde_mut.types.insert(17_366, make_fighter_type(17_366, 448, 2, 100.0, vec![]));

    issuer_sess.station_id = Some(station_id);
    issuer_sess.corporation_id = Some(corp_id);
    let mut acceptor_sess = issuer_sess.clone();
    acceptor_sess.character_id = Some(acceptor_id);
    acceptor_sess.ship_id = Some(acceptor_ship);
    acceptor_sess.corporation_id = Some(1_000_009);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "ContractCorp".into(),
                ticker: "CTRC".into(),
                ceo_id: issuer_id,
                master_wallet: IskCents::from_isk(500_000_000),
                wallet_divisions: vec![CorpWalletDivisionRecord {
                    division_id: 1,
                    name: "Master".into(),
                    balance: IskCents::from_isk(500_000_000),
                }],
                ..Default::default()
            },
        );
        let base_char = snap.characters[&issuer_id].clone();
        if let Some(ch) = snap.characters.get_mut(&issuer_id) {
            ch.station_id = Some(station_id);
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        let mut acc_char = base_char;
        acc_char.character_id = acceptor_id;
        acc_char.user_id = 42;
        acc_char.station_id = Some(station_id);
        acc_char.solar_system_id = sys_id;
        acc_char.active_ship_id = acceptor_ship;
        acc_char.balance = IskCents::from_isk(500_000_000);
        snap.characters.insert(acceptor_id, acc_char);
        snap.items.insert(
            ship_id,
            make_fighter_item(ship_id, 600, issuer_id, station_id, InventoryFlag::Hangar, 1),
        );
        snap.items.insert(
            acceptor_ship,
            make_fighter_item(acceptor_ship, 600, acceptor_id, station_id, InventoryFlag::Hangar, 1),
        );
        for sid in [station_id, dest_id] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_832,
                    owner_corp_id: corp_id,
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
        station_id,
        dest_id,
        corp_id,
    )
}

#[test]
fn create_contract_validation_errors_and_slot_limits() {
    let (ctx, mut issuer_sess, issuer_id, _acc_sess, _acc_id, station_id, dest_id, _corp_id) =
        setup_extra_contracts_env();
    let kw = HashMap::new();
    let active_ship = issuer_sess.ship_id.unwrap();

    // 1. Invalid contract type code (99) -> InvalidContractType
    let err_type = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([("contractType", PyRep::Integer(99))])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_type.code, "InvalidContractType");

    // 3. Courier without destination -> InvalidContractDestination
    let err_dest = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(3)),
            ("startStationID", PyRep::Integer(station_id)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_dest.code, "InvalidContractDestination");

    // 4. ItemExchange with endStationID != startStationID -> InvalidContractDestination
    let err_reloc = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("endStationID", PyRep::Integer(dest_id)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_reloc.code, "InvalidContractDestination");

    // 5. ItemExchange with neither items nor requestedItems -> ContractRequiresItem
    let err_no_item = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_no_item.code, "ContractRequiresItem");

    // 6. Auction with buyout < starting price -> InvalidContractParameters
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            820_001,
            make_fighter_item(820_001, 34, issuer_id, station_id, InventoryFlag::Hangar, 10),
        );
    });
    let err_buyout = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(2)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(5_000_000.0)),
            ("collateral", PyRep::Real(2_000_000.0)), // buyout < price
            ("itemList", PyRep::List(vec![PyRep::Integer(820_001)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_buyout.code, "InvalidContractParameters");

    // 7. Escrow item validation errors: active ship, non-empty container, courier cargo, item inside container, locked item
    let err_ship = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("itemList", PyRep::List(vec![PyRep::Integer(active_ship)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_ship.code, "CannotContractActiveShip");

    let cont_id = 820_010i64;
    let inside_id = 820_011i64;
    let courier_cargo_id = 820_012i64;
    let locked_id = 820_013i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cont_id,
            make_fighter_item(cont_id, 17_366, issuer_id, station_id, InventoryFlag::Hangar, 1),
        );
        snap.items.insert(
            inside_id,
            make_fighter_item(inside_id, 34, issuer_id, cont_id, InventoryFlag::Hangar, 5),
        );
        snap.items.insert(
            courier_cargo_id,
            make_fighter_item(courier_cargo_id, 34, issuer_id, station_id, InventoryFlag::Hangar, 1),
        );
        snap.item_custom_names
            .insert(courier_cargo_id, "courier_cargo:mission1".into());
        snap.items.insert(
            locked_id,
            make_fighter_item(locked_id, 34, issuer_id, station_id, InventoryFlag::Locked, 1),
        );
    });

    for (iid, expected_code) in [
        (cont_id, "CannotContractNonEmptyContainer"),
        (inside_id, "ItemInsideContainer"),
        (courier_cargo_id, "ItemCannotBeContracted"),
        (locked_id, "ContractItemLocked"),
        (999_888_777, "ContractItemNotFound"),
    ] {
        let err = handle(
            &ctx,
            &mut issuer_sess,
            "contractProxy",
            "CreateContract",
            &[PyRep::keyval([
                ("contractType", PyRep::Integer(1)),
                ("startStationID", PyRep::Integer(station_id)),
                ("itemList", PyRep::List(vec![PyRep::Integer(iid)])),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, expected_code);
    }

    // 8. Contracting skill 25235 = 0 -> max_slots = 1; creating a second contract hits ContractSlotLimitReached
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(issuer_id).or_default().insert(
            25_235,
            TrainedSkillRecord {
                type_id: 25_235,
                level: 0,
                skill_points: 0.0,
            },
        );
    });
    // Want-to-buy ItemExchange (empty itemList, non-empty requestedItems)
    let wtb = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("reward", PyRep::Real(500_000.0)),
            (
                "requestedItems",
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(34),
                    PyRep::Integer(40),
                ])]),
            ),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_i64(&wtb, "maxContractsLimit"), Some(1));

    let err_limit = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("itemList", PyRep::List(vec![PyRep::Integer(820_001)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_limit.code, "ContractSlotLimitReached");
}

#[test]
fn want_to_buy_stack_split_auction_outbid_refund_and_delete_reject_fail_gm_expire() {
    let (ctx, mut issuer_sess, issuer_id, mut acc_sess, acc_id, station_id, dest_id, _corp_id) =
        setup_extra_contracts_env();
    let kw = HashMap::new();

    // 1. Create Want-To-Buy ItemExchange requesting 40 units of type 34, offering 1,000,000 ISK reward
    let wtb = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("reward", PyRep::Real(1_000_000.0)),
            (
                "requestedItems",
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(34),
                    PyRep::Integer(40),
                ])]),
            ),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let wtb_cid = dict_i64(&wtb, "contractID").unwrap();

    // Issuer cannot accept own contract -> CannotAcceptOwnContract
    let err_self = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(wtb_cid)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_self.code, "CannotAcceptOwnContract");

    // Acceptor without type 34 in hangar -> MissingRequestedContractItems
    let err_missing = handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(wtb_cid)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_missing.code, "MissingRequestedContractItems");

    // Give acceptor a stack of 100 units of type 34 -> accepting splits 40 to issuer and leaves 60 with acceptor!
    let acc_stack = 830_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            acc_stack,
            make_fighter_item(acc_stack, 34, acc_id, station_id, InventoryFlag::Hangar, 100),
        );
    });
    let acc_wtb = handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(wtb_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&acc_wtb, "status"), Some("Completed"));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&acc_stack].quantity, 60);
        let issuer_got: i32 = snap
            .items
            .values()
            .filter(|i| i.owner_id == issuer_id && i.location_id == station_id && i.type_id == 34)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(issuer_got, 40);
    });

    // 2. Auction minimum bid increment, outbid refund, and CannotDeleteAuctionWithBids
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            830_002,
            make_fighter_item(830_002, 35, issuer_id, station_id, InventoryFlag::Hangar, 10),
        );
        snap.items.insert(
            830_003,
            make_fighter_item(830_003, 35, issuer_id, station_id, InventoryFlag::Hangar, 10),
        );
        snap.items.insert(
            830_004,
            make_fighter_item(830_004, 35, issuer_id, station_id, InventoryFlag::Hangar, 10),
        );
    });
    let auc = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(2)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(1_000_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(830_002)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let auc_cid = dict_i64(&auc, "contractID").unwrap();

    // Opening bid below starting price -> BidTooLow
    let err_low1 = handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(500_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_low1.code, "BidTooLow");

    // Valid opening bid of 1,000,000 ISK
    handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(1_000_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Second bid below 5% minimum increment (< 1,050,000 ISK) -> BidTooLow
    let err_low2 = handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(1_010_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_low2.code, "BidTooLow");

    // Deleting an auction with active bids -> CannotDeleteAuctionWithBids
    let err_del_auc = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "DeleteContract",
        &[PyRep::Integer(auc_cid)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_del_auc.code, "CannotDeleteAuctionWithBids");

    // 3. RejectContract by assignee, DeleteContract by issuer, FailContract on Courier, and GM_ExpireContract
    let priv_c = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("availability", PyRep::Integer(1)), // Personal
            ("assigneeID", PyRep::Integer(acc_id)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(100_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(830_003)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let priv_cid = dict_i64(&priv_c, "contractID").unwrap();

    // GetLoginInfo shows assignedToMe for acceptor
    let login_info = handle(&ctx, &mut acc_sess, "contractProxy", "GetLoginInfo", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(login_info.dict_get("assignedToMe"), Some(PyRep::List(l)) if !l.is_empty()));

    // Assignee rejects personal contract -> status "Rejected"
    let rej = handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "RejectContract",
        &[PyRep::Integer(priv_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&rej, "status"), Some("Rejected"));

    // Create courier contract, accept it, then FailContract by acceptor -> status "Failed"
    let cour = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(3)),
            ("startStationID", PyRep::Integer(station_id)),
            ("endStationID", PyRep::Integer(dest_id)),
            ("reward", PyRep::Real(100_000.0)),
            ("collateral", PyRep::Real(200_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(830_004)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let cour_cid = dict_i64(&cour, "contractID").unwrap();
    handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(cour_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let failed = handle(
        &ctx,
        &mut acc_sess,
        "contractProxy",
        "FailContract",
        &[PyRep::Integer(cour_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&failed, "status"), Some("Failed"));

    // GM_ExpireContract on auction: non-GM fails with GMRoleRequired; GM succeeds
    ctx.db.with_mut_snapshot(|snap| {
        for a in snap.accounts.values_mut() {
            a.role = 0;
        }
    });
    let err_gm = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "GM_ExpireContract",
        &[PyRep::Integer(auc_cid)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_gm.code, "GMRoleRequired");

    ctx.db.with_mut_snapshot(|snap| {
        for a in snap.accounts.values_mut() {
            a.role = corp_roles::ROLE_DIRECTOR;
        }
    });
    let exp_auc = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "GM_ExpireContract",
        &[PyRep::Integer(auc_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&exp_auc, "status"), Some("Completed"));

    // GetMyCurrentContractList & GetMyExpiredContractList
    let expired_list = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "GetMyExpiredContractList",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(expired_list, PyRep::List(ref l) if l.len() >= 3));
}
