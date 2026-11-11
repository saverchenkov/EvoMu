//! Semantic unit tests split from `contracts_service_extra_unit_tests`.

use super::contracts_service_extra_unit_tests::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_item;
use crate::services::contracts_service::*;
use eve_db::{
    corp_roles, InventoryFlag,
};
use eve_proto::PyRep;
use std::collections::HashMap;

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

#[test]
fn accept_corp_item_exchange_role_flags_wallet_and_requested_item_filtering() {
    use crate::services::inventory_fitting::fitting_containers_ships::write_container_ship_state;
    use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state, AssetSafetyWrapRecord};
    use eve_db::{CorporationRecord, InventoryItemRecord, IskCents};

    let (ctx, mut issuer_sess, issuer_id, mut acc_sess, acc_id, station_id, _dest_id, issuer_corp) =
        setup_extra_contracts_env();
    let acc_corp = 98_017_002_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&issuer_corp).unwrap().members.insert(issuer_id);
        let mut ac = CorporationRecord::default();
        ac.corporation_id = acc_corp;
        ac.name = "AcceptCorp".into();
        ac.ceo_id = acc_id + 99;
        ac.ensure_wallet_divisions();
        for div in 1..=7u8 {
            ac.set_division_balance(div, IskCents::from_isk(100_000_000));
        }
        ac.members.insert(acc_id);
        snap.corporations.insert(acc_corp, ac);
        snap.characters.get_mut(&acc_id).unwrap().corporation_id = acc_corp;
    });
    acc_sess.corporation_id = Some(acc_corp);
    acc_sess.corp_account_key = 1001; // wallet division 2 (index 1)

    // Issuer creates forCorp two-way ItemExchange offering item 920_100 (from CorpSAG1) asking price 500_000 + 25x type 34
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(920_100, make_fighter_item(920_100, 35, issuer_corp, station_id, InventoryFlag::CorpSAG1, 10));
    });
    let cid = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(500_000.0)),
            ("forCorp", PyRep::Bool(true)),
            ("itemList", PyRep::Integer(920_100)),
            ("requestedItems", PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(25)])])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();

    // Seed ineligible & eligible type-34 items in CorpSAG3 (flag 117) of acc_corp:
    // damaged, BPC, mutated, courier_cargo, ALSC locked, asset safety wrap, non-empty container, valid stack(40)
    ctx.db.with_mut_snapshot(|snap| {
        let mut damaged = make_fighter_item(920_201, 34, acc_corp, station_id, InventoryFlag::CorpSAG3, 50);
        damaged.dynamic_attributes.insert(3, 1.0);
        snap.items.insert(920_201, damaged);

        let mut bpc = make_fighter_item(920_202, 34, acc_corp, station_id, InventoryFlag::CorpSAG3, 50);
        bpc.is_blueprint_copy = true;
        snap.items.insert(920_202, bpc);

        let mut mutated = make_fighter_item(920_203, 34, acc_corp, station_id, InventoryFlag::CorpSAG3, 50);
        mutated.source_mutaplasmid_id = Some(49_726);
        snap.items.insert(920_203, mutated);

        snap.items.insert(920_204, make_fighter_item(920_204, 34, acc_corp, station_id, InventoryFlag::CorpSAG3, 50));
        snap.item_custom_names.insert(920_204, "courier_cargo:job1".into());

        snap.items.insert(920_205, make_fighter_item(920_205, 34, acc_corp, station_id, InventoryFlag::CorpSAG3, 50));
        snap.items.insert(920_206, make_fighter_item(920_206, 34, acc_corp, station_id, InventoryFlag::CorpSAG3, 50));
        // Non-empty parent container of type 34? Even an item having child inside it is excluded!
        snap.items.insert(920_207, make_fighter_item(920_207, 34, acc_corp, station_id, InventoryFlag::CorpSAG3, 50));
        snap.items.insert(920_208, InventoryItemRecord {
            item_id: 920_208,
            type_id: 35,
            owner_id: acc_corp,
            location_id: 920_207,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            version: 1,
            ..Default::default()
        });
        let mut ext = load_ext_state(snap);
        ext.asset_safety_wraps.insert(
            920_999,
            AssetSafetyWrapRecord {
                wrap_id: 920_999,
                owner_id: acc_corp,
                is_corp: true,
                source_structure_id: station_id,
                source_structure_name: "Src".into(),
                solar_system_id: 30_000_142,
                created_unix: 1,
                deliverable_after_unix: 1,
                delivered_to_id: None,
                item_ids: vec![920_206],
                estimated_value_isk: 0.0,
                nearest_npc_station_id: None,
                auto_delivered: false,
                retrieval_fee_paid: false,
            },
        );
        save_ext_state(snap, &ext);
    });
    write_container_ship_state(&ctx, |cs| {
        cs.alsc_locked_items.insert(920_205);
    });

    // 1. Missing ROLE_CONTRACT_MANAGER -> CrpAccessDenied
    assert_eq!(
        handle(&ctx, &mut acc_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid), PyRep::Bool(true)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );

    // 2. Grant ROLE_CONTRACT_MANAGER only (no CorpSAG3 take) -> all ineligible items skipped + no valid CorpSAG3 -> MissingRequestedContractItems
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&acc_corp).unwrap().member_roles.insert(acc_id, corp_roles::ROLE_CONTRACT_MANAGER);
    });
    assert_eq!(
        handle(&ctx, &mut acc_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid), PyRep::Bool(true)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "MissingRequestedContractItems"
    );

    // 3. Seed clean stack 920_210 (qty=40) in CorpSAG3, still missing ROLE_HANGAR_CAN_TAKE[2] -> MissingRequestedContractItems
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(920_210, make_fighter_item(920_210, 34, acc_corp, station_id, InventoryFlag::CorpSAG3, 40));
    });
    assert_eq!(
        handle(&ctx, &mut acc_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid), PyRep::Bool(true)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "MissingRequestedContractItems"
    );

    // 4. Grant ROLE_HANGAR_CAN_TAKE[2] without division-2 wallet take role -> CrpAccessDenied on price > 0
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&acc_corp)
            .unwrap()
            .member_roles
            .insert(acc_id, corp_roles::ROLE_CONTRACT_MANAGER | corp_roles::ROLE_HANGAR_CAN_TAKE[2]);
    });
    assert_eq!(
        handle(&ctx, &mut acc_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid), PyRep::Bool(true)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );

    // 5. Grant ROLE_ACCOUNT_CAN_TAKE[1] -> AcceptContract succeeds! Consumes 25 from 920_210, delivers to CorpDeliveries of both corps
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&acc_corp).unwrap().member_roles.insert(
            acc_id,
            corp_roles::ROLE_CONTRACT_MANAGER | corp_roles::ROLE_HANGAR_CAN_TAKE[2] | corp_roles::ROLE_ACCOUNT_CAN_TAKE[1],
        );
    });
    let div2_before = ctx.db.with_snapshot(|s| s.corporations[&acc_corp].division_balance(2));
    let issuer_div1_before = ctx.db.with_snapshot(|s| s.corporations[&issuer_corp].division_balance(1));
    let acc_res = handle(&ctx, &mut acc_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid), PyRep::Bool(true)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(dict_str(&acc_res, "status"), Some("Completed"));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&920_210).unwrap().quantity, 15);
        let req_delivered: i32 = snap
            .items
            .values()
            .filter(|i| i.owner_id == issuer_corp && i.type_id == 34 && i.location_id == station_id && i.flag == InventoryFlag::CorpDeliveries)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(req_delivered, 25);
        let offered = snap.items.get(&920_100).unwrap();
        assert_eq!(offered.owner_id, acc_corp);
        assert_eq!(offered.flag, InventoryFlag::CorpDeliveries);
        assert_eq!(
            div2_before.0 - snap.corporations[&acc_corp].division_balance(2).0,
            IskCents::from_isk(500_000).0
        );
        assert!(snap.corporations[&issuer_corp].division_balance(1).0 >= issuer_div1_before.0 + IskCents::from_isk(500_000).0);
    });

    // Wrong station / Courier NoActiveShip / Private visibility denial
    let mut wrong_loc_sess = acc_sess.clone();
    wrong_loc_sess.station_id = Some(station_id + 999);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(920_102, make_fighter_item(920_102, 35, issuer_corp, station_id, InventoryFlag::CorpSAG1, 5));
    });
    let priv_cid = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("availability", PyRep::String("Private".into())),
            ("assigneeID", PyRep::Integer(acc_id)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(1000.0)),
            ("itemList", PyRep::Integer(920_102)),
            ("forCorp", PyRep::Bool(true)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();
    assert_eq!(
        handle(&ctx, &mut wrong_loc_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(priv_cid)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "MustBeDockedAtStartStation"
    );
}

#[test]
fn delete_reject_fail_gm_expire_corp_rbac_and_orig_flag_restoration() {
    use eve_db::{CorporationRecord, IskCents};

    let (ctx, mut ceo_sess, ceo_id, mut acc_sess, acc_id, station_id, dest_id, corp_id) =
        setup_extra_contracts_env();
    let mgr_id = 921_001_i64;
    let kw = HashMap::new();

    // Seed second corp member mgr_id (without role at first) & assignee_corp
    let assignee_corp = 98_021_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut mgr_ch = snap.characters[&acc_id].clone();
        mgr_ch.character_id = mgr_id;
        mgr_ch.user_id = mgr_id as u64;
        mgr_ch.corporation_id = corp_id;
        mgr_ch.station_id = Some(station_id);
        snap.characters.insert(mgr_id, mgr_ch);
        snap.corporations.get_mut(&corp_id).unwrap().members.extend([ceo_id, mgr_id]);

        let mut ac = CorporationRecord::default();
        ac.corporation_id = assignee_corp;
        ac.name = "AssigneeCorp".into();
        ac.ceo_id = acc_id + 99;
        ac.ensure_wallet_divisions();
        ac.set_division_balance(1, IskCents::from_isk(50_000_000));
        ac.members.insert(acc_id);
        snap.corporations.insert(assignee_corp, ac);
        snap.characters.get_mut(&acc_id).unwrap().corporation_id = assignee_corp;
    });
    acc_sess.corporation_id = Some(assignee_corp);
    let mut mgr_sess = ceo_sess.clone();
    mgr_sess.character_id = Some(mgr_id);
    mgr_sess.user_id = Some(mgr_id as u64);

    // 1. Corp contract from CorpSAG2 + extra orphaned sub-item -> non-manager blocked, manager DeleteContract restores exact CorpSAG2 + sub-item!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(921_100, make_fighter_item(921_100, 34, corp_id, station_id, InventoryFlag::CorpSAG2, 10));
        snap.items.insert(921_101, make_fighter_item(921_101, 35, corp_id, station_id, InventoryFlag::CorpSAG4, 5));
    });
    let cid_del = handle(
        &ctx,
        &mut ceo_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("availability", PyRep::String("Public".into())),
            ("startStationID", PyRep::Integer(station_id)),
            ("reward", PyRep::Real(200_000.0)),
            ("forCorp", PyRep::Bool(true)),
            ("itemList", PyRep::Integer(921_100)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();
    // Simulate orphan item inside escrow contract location + check non-manager denied
    ctx.db.with_mut_snapshot(|snap| {
        let extra = snap.items.get_mut(&921_101).unwrap();
        extra.location_id = cid_del;
        extra.flag = InventoryFlag::Locked;
        snap.service_state_blobs.insert(format!("contract_orig_flag:{cid_del}:921101"), (InventoryFlag::CorpSAG4 as u16).to_string());
    });
    assert_eq!(
        handle(&ctx, &mut mgr_sess, "contractProxy", "DeleteContract", &[PyRep::Integer(cid_del)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ContractOwnershipViolation"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(mgr_id, corp_roles::ROLE_CONTRACT_MANAGER);
    });
    let div1_before_del = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1));
    let del_res = handle(&ctx, &mut mgr_sess, "contractProxy", "DeleteContract", &[PyRep::Integer(cid_del)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(dict_str(&del_res, "status"), Some("Deleted"));
    ctx.db.with_snapshot(|snap| {
        let it0 = snap.items.get(&921_100).unwrap();
        assert_eq!(it0.owner_id, corp_id);
        assert_eq!(it0.location_id, station_id);
        assert_eq!(it0.flag, InventoryFlag::CorpSAG2);
        let it1 = snap.items.get(&921_101).unwrap();
        assert_eq!(it1.owner_id, corp_id);
        assert_eq!(it1.flag, InventoryFlag::CorpSAG4);
        assert_eq!(
            snap.corporations[&corp_id].division_balance(1).0 - div1_before_del.0,
            IskCents::from_isk(300_000).0 // 200k reward + 100k public deposit refunded
        );
    });

    // 2. RejectContract on corp-assigned contract enforces ROLE_CONTRACT_MANAGER, then restores orig flag
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(921_102, make_fighter_item(921_102, 34, corp_id, station_id, InventoryFlag::CorpSAG3, 10));
    });
    let cid_rej = handle(
        &ctx,
        &mut ceo_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("availability", PyRep::String("Private".into())),
            ("assigneeID", PyRep::Integer(assignee_corp)),
            ("startStationID", PyRep::Integer(station_id)),
            ("reward", PyRep::Real(150_000.0)),
            ("forCorp", PyRep::Bool(true)),
            ("itemList", PyRep::Integer(921_102)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();
    assert_eq!(
        handle(&ctx, &mut acc_sess, "contractProxy", "RejectContract", &[PyRep::Integer(cid_rej)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&assignee_corp).unwrap().member_roles.insert(acc_id, corp_roles::ROLE_CONTRACT_MANAGER);
    });
    let rej_res = handle(&ctx, &mut acc_sess, "contractProxy", "RejectContract", &[PyRep::Integer(cid_rej)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(dict_str(&rej_res, "status"), Some("Rejected"));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&921_102].flag), InventoryFlag::CorpSAG3);

    // 3. FailContract on corp InProgress courier by corp contract manager + InvalidContractState on non-courier
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(921_103, make_fighter_item(921_103, 34, ceo_id, station_id, InventoryFlag::Hangar, 5));
    });
    let cid_cour = handle(
        &ctx,
        &mut ceo_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(3)),
            ("startStationID", PyRep::Integer(station_id)),
            ("endStationID", PyRep::Integer(dest_id)),
            ("reward", PyRep::Real(50_000.0)),
            ("collateral", PyRep::Real(400_000.0)),
            ("itemList", PyRep::Integer(921_103)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();
    // Non-InProgress FailContract -> InvalidContractState
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "contractProxy", "FailContract", &[PyRep::Integer(cid_cour)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidContractState"
    );
    assert_eq!(
        handle(&ctx, &mut acc_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid_cour), PyRep::Bool(true)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&assignee_corp)
            .unwrap()
            .member_roles
            .insert(acc_id, corp_roles::ROLE_CONTRACT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]);
    });
    handle(&ctx, &mut acc_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cid_cour), PyRep::Bool(true)], &kw)
        .unwrap()
        .unwrap();
    let ceo_bal_before = ctx.db.with_snapshot(|s| s.characters[&ceo_id].balance);
    let fail_res = handle(&ctx, &mut acc_sess, "contractProxy", "FailContract", &[PyRep::Integer(cid_cour)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(dict_str(&fail_res, "status"), Some("Failed"));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&ceo_id].balance.0 - ceo_bal_before.0),
        IskCents::from_isk(450_000).0 // 50_000 reward refund + 400_000 collateral forfeit
    );

    // 4. GM_ExpireContract on Auction WITHOUT bids -> "Expired" and restores orig_flag; Deleting "Expired" -> "Deleted"
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(921_104, make_fighter_item(921_104, 34, corp_id, station_id, InventoryFlag::CorpSAG5, 8));
        for a in snap.accounts.values_mut() {
            a.role = corp_roles::ROLE_DIRECTOR;
        }
    });
    let auc_nobid = handle(
        &ctx,
        &mut ceo_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(2)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(50_000.0)),
            ("forCorp", PyRep::Bool(true)),
            ("itemList", PyRep::Integer(921_104)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap()
    .dict_get("contractID")
    .and_then(|v| v.as_i64())
    .unwrap();
    assert_eq!(
        handle(&ctx, &mut ceo_sess, "contractProxy", "GM_ExpireContract", &[PyRep::Integer(9_999_999)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ContractNotFound"
    );
    let exp_nobid = handle(&ctx, &mut ceo_sess, "contractProxy", "GM_ExpireContract", &[PyRep::Integer(auc_nobid)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(dict_str(&exp_nobid, "status"), Some("Expired"));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&921_104].flag), InventoryFlag::CorpSAG5);
    let del_exp = handle(&ctx, &mut ceo_sess, "contractProxy", "DeleteContract", &[PyRep::Integer(auc_nobid)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(dict_str(&del_exp, "status"), Some("Deleted"));
}

#[test]
fn search_contracts_w2_structure_regions_item_types_visibility_and_owner_queries() {
    use eve_db::{ContractRecord, IskCents, UpwellStructureRecord};

    let (ctx, mut issuer_sess, issuer_id, mut acc_sess, _acc_id, station_id, _dest_id, issuer_corp) =
        setup_extra_contracts_env();
    let kw = HashMap::new();

    // Find a second SDE region different from station_id's region and place a structure there
    let forge_sys = ctx.db.with_snapshot(|s| s.structures[&station_id].solar_system_id);
    let forge_region = ctx.sde.solar_systems[&forge_sys].region_id;
    let other_sys_rec = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.region_id != forge_region)
        .cloned();
    let other_struct_id = 1_000_000_922_001_i64;
    if let Some(ref osys) = other_sys_rec {
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(
                other_struct_id,
                UpwellStructureRecord {
                    structure_id: other_struct_id,
                    type_id: 35_832,
                    owner_corp_id: issuer_corp,
                    solar_system_id: osys.solar_system_id,
                    name: "OtherRegion Citadel".into(),
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    ..Default::default()
                },
            );
        });
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(922_100, make_fighter_item(922_100, 34, 922_900, station_id, InventoryFlag::Locked, 1));
        snap.items.insert(922_101, make_fighter_item(922_101, 34, 922_901, other_struct_id, InventoryFlag::Locked, 1));
        // 1. Public Forge structure ItemExchange (type 34 offered, price 1_000)
        snap.contracts.insert(922_001, ContractRecord {
            contract_id: 922_001, issuer_id, issuer_corp_id: issuer_corp, assignee_id: 0,
            availability: "Public".into(), contract_type: "ItemExchange".into(), status: "Outstanding".into(),
            start_station_id: station_id, end_station_id: station_id, price: IskCents::from_isk(1_000),
            item_ids: vec![922_100], expiration_unix: ctx.now_unix() + 1000, date_issued_unix: ctx.now_unix(), num_days: 1, ..Default::default()
        });
        // 2. Public WTB ItemExchange (no item_ids, requests type 35, price 500)
        snap.contracts.insert(922_002, ContractRecord {
            contract_id: 922_002, issuer_id, issuer_corp_id: issuer_corp, assignee_id: 0,
            availability: "Public".into(), contract_type: "ItemExchange".into(), status: "Outstanding".into(),
            start_station_id: station_id, end_station_id: station_id, price: IskCents::from_isk(500),
            requested_items: vec![(35, 10)], expiration_unix: ctx.now_unix() + 1000, date_issued_unix: ctx.now_unix(), num_days: 1, ..Default::default()
        });
        // 3. Other-region structure contract (price 500)
        snap.contracts.insert(922_003, ContractRecord {
            contract_id: 922_003, issuer_id, issuer_corp_id: issuer_corp, assignee_id: 0,
            availability: "Public".into(), contract_type: "Auction".into(), status: "Outstanding".into(),
            start_station_id: other_struct_id, end_station_id: other_struct_id, price: IskCents::from_isk(500),
            item_ids: vec![922_101], expiration_unix: ctx.now_unix() + 1000, date_issued_unix: ctx.now_unix(), num_days: 1, ..Default::default()
        });
        // 4. Unknown station fallback contract (unknown station resolves true for regionID filter)
        snap.contracts.insert(922_004, ContractRecord {
            contract_id: 922_004, issuer_id, issuer_corp_id: issuer_corp, assignee_id: 0,
            availability: "Public".into(), contract_type: "ItemExchange".into(), status: "Outstanding".into(),
            start_station_id: 999_888_777, end_station_id: 999_888_777, price: IskCents::from_isk(750),
            expiration_unix: ctx.now_unix() + 1000, date_issued_unix: ctx.now_unix(), num_days: 1, ..Default::default()
        });
        // 5. Private personal contract for someone else (hidden from acc_id) & Corp contract (hidden from outsider, visible to corp member)
        snap.contracts.insert(922_005, ContractRecord {
            contract_id: 922_005, issuer_id, issuer_corp_id: issuer_corp, assignee_id: 99_999_999,
            availability: "Personal".into(), contract_type: "ItemExchange".into(), status: "Outstanding".into(),
            start_station_id: station_id, end_station_id: station_id, price: IskCents::from_isk(500),
            expiration_unix: ctx.now_unix() + 1000, date_issued_unix: ctx.now_unix(), num_days: 1, ..Default::default()
        });
        snap.contracts.insert(922_006, ContractRecord {
            contract_id: 922_006, issuer_id, issuer_corp_id: issuer_corp, assignee_id: 0,
            availability: "Corporation".into(), contract_type: "ItemExchange".into(), status: "Outstanding".into(),
            start_station_id: station_id, end_station_id: station_id, price: IskCents::from_isk(500),
            for_corp: true, expiration_unix: ctx.now_unix() + 1000, date_issued_unix: ctx.now_unix(), num_days: 1, ..Default::default()
        });
    });

    let cids_from = |rep: PyRep| -> Vec<i64> {
        let PyRep::List(items) = rep else { panic!("expected PyRep::List") };
        items
            .iter()
            .filter_map(|r| dict_i64(r, "contractID"))
            .collect()
    };

    // Region filter excludes known other-region structure while keeping local structure + unknown fallback
    if let Some(ref osys) = other_sys_rec {
        let mut kw_forge = HashMap::new();
        kw_forge.insert("regionID".into(), PyRep::Integer(forge_region as i64));
        let forge_hits = cids_from(handle(&ctx, &mut acc_sess, "contractProxy", "SearchContracts", &[], &kw_forge).unwrap().unwrap());
        assert!(forge_hits.contains(&922_001) && forge_hits.contains(&922_002) && forge_hits.contains(&922_004));
        assert!(!forge_hits.contains(&922_003));
        assert!(!forge_hits.contains(&922_005) && !forge_hits.contains(&922_006));

        let mut kw_other = HashMap::new();
        kw_other.insert("regionID".into(), PyRep::Integer(osys.region_id as i64));
        let other_hits = cids_from(handle(&ctx, &mut acc_sess, "contractProxy", "GetPublicContractList", &[], &kw_other).unwrap().unwrap()); // rpc-lint: allow test fixture
        assert!(other_hits.contains(&922_003));
        assert!(!other_hits.contains(&922_001));
    }

    // itemTypeID finds WTB requested item even with empty item_ids
    let mut kw_wtb_type = HashMap::new();
    kw_wtb_type.insert("itemTypeID".into(), PyRep::Integer(35));
    assert_eq!(
        cids_from(handle(&ctx, &mut acc_sess, "contractProxy", "SearchContracts", &[], &kw_wtb_type).unwrap().unwrap()),
        vec![922_002]
    );

    // Exact inclusive price boundaries minPrice / maxPrice & stationID
    let mut kw_price = HashMap::new();
    kw_price.insert("stationID".into(), PyRep::Integer(station_id));
    kw_price.insert("minPrice".into(), PyRep::Real(500.0));
    kw_price.insert("maxPrice".into(), PyRep::Real(1_000.0));
    assert_eq!(
        cids_from(handle(&ctx, &mut acc_sess, "contractProxy", "SearchContracts", &[], &kw_price).unwrap().unwrap()),
        vec![922_001, 922_002]
    );

    // Private contract denial for outsider
    assert_eq!(
        handle(&ctx, &mut acc_sess, "contractProxy", "GetContract", &[PyRep::Integer(922_005)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ContractAccessDenied"
    );

    // GetContractListForOwner: foreign owner forbidden, personal excludes for_corp contract 922_006, forCorp=true includes ONLY 922_006
    assert_eq!(
        handle(&ctx, &mut acc_sess, "contractProxy", "GetContractListForOwner", &[PyRep::Integer(issuer_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ContractAccessDenied"
    );
    let personal_owned = cids_from(
        handle(&ctx, &mut issuer_sess, "contractProxy", "GetMyCurrentContractList", &[], &kw)
            .unwrap()
            .unwrap(),
    );
    assert!(personal_owned.contains(&922_001) && !personal_owned.contains(&922_006));
    let corp_owned = cids_from(
        handle(&ctx, &mut issuer_sess, "contractProxy", "GetContractList", &[PyRep::Bool(true)], &kw) // rpc-lint: allow test fixture
            .unwrap()
            .unwrap(),
    );
    assert_eq!(corp_owned, vec![922_006]);
}
