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
    CorpWalletDivisionRecord, CorporationRecord, InventoryFlag, IskCents,
    TrainedSkillRecord, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::Arc;

pub(super) fn dict_i64(r: &PyRep, k: &str) -> Option<i64> {
    r.dict_get(k).and_then(|v| v.as_i64())
}
pub(super) fn created_cid(r: &PyRep) -> Option<i64> {
    match r {
        PyRep::List(ids) => ids.first().and_then(|v| v.as_i64()),
        _ => dict_i64(r, "contractID"),
    }
}
pub(super) fn contract_entries(r: &PyRep) -> &[PyRep] {
    match r.dict_get("contracts") {
        Some(PyRep::List(rows)) => rows.as_slice(),
        _ => panic!("expected KeyVal with contracts list, got {r:?}"),
    }
}
pub(super) fn dict_str<'a>(r: &'a PyRep, k: &str) -> Option<&'a str> {
    r.dict_get(k).and_then(|v| v.as_str())
}

pub(super) fn setup_extra_contracts_env() -> (
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

    let (issuer_uid, acc_uid) = ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "ContractCorp".into();
                __corp.ticker = "CTRC".into();
                __corp.ceo_id = issuer_id;
                __corp.wallet_divisions = vec![CorpWalletDivisionRecord {
                    division_id: 1,
                    name: "Master".into(),
                    balance: IskCents::from_isk(500_000_000),
                }];
                let __mw = IskCents::from_isk(500_000_000);
                if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); }
                __corp },
        );
        let base_char = snap.characters[&issuer_id].clone();
        let issuer_uid = base_char.user_id;
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
        for uid in [issuer_uid, 42] {
            if !snap.accounts.values().any(|a| a.user_id == uid) {
                snap.accounts.insert(
                    format!("contract_user_{uid}"),
                    eve_db::AccountRecord {
                        user_id: uid,
                        username: format!("contract_user_{uid}"),
                        password_token: String::new(),
                        role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                        banned_until_unix: None,
                        ban_reason: None,
                    },
                );
            }
        }
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
        (issuer_uid, 42_u64)
    });
    let lock_now = ctx.now_unix();
    ctx.db
        .acquire_online_lock(issuer_uid, issuer_sess.session_id, Some(issuer_id), true, lock_now)
        .unwrap();
    ctx.db
        .acquire_online_lock(acc_uid, acceptor_sess.session_id, Some(acceptor_id), true, lock_now)
        .unwrap();

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

    let err_courier_non_empty = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(3)),
            ("startStationID", PyRep::Integer(station_id)),
            ("endStationID", PyRep::Integer(station_id)),
            ("volume", PyRep::Real(100.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(cont_id)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_courier_non_empty.code, "CannotContractNonEmptyContainer");

    for (iid, expected_code) in [
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

    // Non-empty container is valid on ItemExchange and includes child item metadata
    let cont_ie = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(1_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(cont_id)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let cont_cid = created_cid(&cont_ie).unwrap();
    handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "DeleteContract",
        &[PyRep::Integer(cont_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();

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
    assert!(created_cid(&wtb).is_some());

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
fn contract_notify_delta_acl_deposit_courier_expiry_break_wrap_and_search_edges() {
    use eve_db::{corp_roles, ContractRecord, VirtualServerClock};

    let (mut ctx, mut issuer_sess, issuer_id, mut acceptor_sess, acceptor_id, station_id, dest_id, corp_id) =
        setup_extra_contracts_env();
    let kw = HashMap::new();
    let now = 1_760_000_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now)));
    let reg_id = ctx
        .sde
        .solar_systems
        .get(&issuer_sess.solar_system_id.unwrap())
        .map(|s| i64::from(s.region_id))
        .unwrap();

    // Add Plastic Wrap (3468) + non-player-corp setup & extra corp wallet divisions
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        sde_mut.types.insert(3468, make_fighter_type(3468, 22, 4, 1.0, vec![]));
    }
    let corp_mate_id = 90_000_003i64;
    let unknown_char_id = 90_000_099i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ensure_wallet_divisions();
            corp.set_division_balance(3, IskCents::from_isk(5_000)); // too poor for 110k public corp fee+deposit
            corp.member_roles.insert(
                issuer_id,
                corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_CONTRACT_MANAGER,
            );
            corp.member_roles.insert(
                corp_mate_id,
                corp_roles::ROLE_CONTRACT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0],
            );
        }
        let mut mate = snap.characters[&acceptor_id].clone();
        mate.character_id = corp_mate_id;
        mate.user_id = 43;
        mate.corporation_id = corp_id;
        mate.station_id = Some(station_id);
        mate.balance = IskCents::from_isk(500_000_000);
        snap.characters.insert(corp_mate_id, mate);

        // Give acceptor corp roles on a separate buyer corp for corp-bid / auction expiry tests
        let buyer_corp = 98_000_888i64;
        let mut bc = snap.corporations[&corp_id].clone();
        bc.corporation_id = buyer_corp;
        bc.ceo_id = acceptor_id;
        bc.member_roles.clear();
        bc.member_roles.insert(acceptor_id, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_CONTRACT_MANAGER);
        bc.set_division_balance(1, IskCents::from_isk(50_000_000));
        snap.corporations.insert(buyer_corp, bc);
        if let Some(acc) = snap.characters.get_mut(&acceptor_id) {
            acc.corporation_id = buyer_corp;
        }
        if let Some(mate_ch) = snap.characters.get_mut(&corp_mate_id) {
            mate_ch.corporation_id = corp_id;
        }
        if !snap.accounts.values().any(|a| a.user_id == 43) {
            snap.accounts.insert(
                "contract_mate_43".into(),
                eve_db::AccountRecord {
                    user_id: 43,
                    username: "contract_mate_43".into(),
                    password_token: String::new(),
                    role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
    });
    ctx.db
        .acquire_online_lock(43, 9003, Some(corp_mate_id), true, ctx.now_unix())
        .unwrap();
    acceptor_sess.corporation_id = Some(98_000_888);
    acceptor_sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_CONTRACT_MANAGER;
    issuer_sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_CONTRACT_MANAGER;

    // 1. CreateContract: ACL denial triggers ContractItemLocked, Corp division 3 error mentions "division 3",
    //    funded public contract takes broker+deposit & ContractNotifyDelta emits OnItemChange + OnAccountChange!
    let acl_item = 821_001i64;
    let corp_div_item = 821_002i64;
    let pub_item = 821_003i64;
    let partial_stack_id = 821_004i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(acl_item, make_fighter_item(acl_item, 34, issuer_id, station_id, InventoryFlag::Hangar, 10));
        snap.items.insert(corp_div_item, make_fighter_item(corp_div_item, 34, corp_id, station_id, InventoryFlag::CorpSAG1, 10));
        snap.items.insert(pub_item, make_fighter_item(pub_item, 34, issuer_id, station_id, InventoryFlag::Hangar, 10));
        snap.items.insert(partial_stack_id, make_fighter_item(partial_stack_id, 34, acceptor_id, station_id, InventoryFlag::Hangar, 100));
        snap.structure_docking_acl_denied.entry(station_id).or_default().insert(issuer_id);
    });
    let err_acl = handle(
        &ctx, &mut issuer_sess, "contractProxy", "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("itemList", PyRep::List(vec![PyRep::Integer(acl_item)])),
        ])],
        &kw,
    ).unwrap().unwrap_err();
    assert_eq!(err_acl.code, "ContractItemLocked");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_docking_acl_denied.remove(&station_id);
    });

    // Corp upfront division 3 insufficient funds check ("division 3") then sufficient funds check
    issuer_sess.corp_account_key = 1002; // index 2 -> division 3
    let err_div3 = handle(
        &ctx, &mut issuer_sess, "contractProxy", "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("forCorp", PyRep::Bool(true)),
            ("startStationID", PyRep::Integer(station_id)),
            ("availability", PyRep::Integer(0)), // Public
            ("price", PyRep::Real(1_000_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(corp_div_item)])),
        ])],
        &kw,
    ).unwrap().unwrap_err();
    assert_eq!(err_div3.code, "InsufficientFunds");
    assert!(err_div3.message.contains("division 3"), "got: {}", err_div3.message);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().set_division_balance(3, IskCents::from_isk(1_000_000));
    });
    let _ = ctx.drain_notifications_for_char(issuer_id);
    let ok_div3 = handle(
        &ctx, &mut issuer_sess, "contractProxy", "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("forCorp", PyRep::Bool(true)),
            ("startStationID", PyRep::Integer(station_id)),
            ("availability", PyRep::Integer(0)),
            ("price", PyRep::Real(20_000_000.0)), // broker 200k + deposit 200k = 400k
            ("itemList", PyRep::List(vec![PyRep::Integer(corp_div_item)])),
        ])],
        &kw,
    ).unwrap().unwrap();
    let corp_cid = created_cid(&ok_div3).unwrap();
    issuer_sess.corp_account_key = 1000;
    ctx.db.with_snapshot(|snap| {
        let div3_rem = snap.corporations[&corp_id].division_balance(3);
        assert_eq!(div3_rem, IskCents::from_isk(600_000));
        assert_eq!(snap.fleet_and_social_ops.contract_deposits.get(&corp_cid).copied(), Some(200_000 * 100));
    });
    let create_notifs = ctx.drain_notifications_for_char(issuer_id);
    assert!(create_notifs.iter().any(|n| n.event == "OnItemChange"));
    assert!(create_notifs.iter().any(|n| n.event == "OnAccountChange"));

    // 2. AcceptContract: self-corp guard (corp_mate cannot Corp-accept corp_cid -> CannotAcceptOwnContract),
    //    expired contract status guard ("Expired"), and partial-stack WTB emission of new-item + qty diff OnItemChange!
    let mut mate_sess = issuer_sess.clone();
    mate_sess.character_id = Some(corp_mate_id);
    mate_sess.corp_role = corp_roles::ROLE_CONTRACT_MANAGER;
    let err_self_corp = handle(
        &ctx, &mut mate_sess, "contractProxy", "AcceptContract",
        &[PyRep::Integer(corp_cid), PyRep::Bool(true)], &kw,
    ).unwrap().unwrap_err();
    assert_eq!(err_self_corp.code, "CannotAcceptOwnContract");

    // Pre-set already "Expired" contract (with future expiration_unix so only c.status=="Expired" fires)
    let expired_status_cid = 822_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(expired_status_cid, ContractRecord {
            contract_id: expired_status_cid,
            issuer_id,
            contract_type: "ItemExchange".into(),
            status: "Expired".into(),
            availability: "Public".into(),
            start_station_id: station_id,
            end_station_id: station_id,
            expiration_unix: now + 10_000,
            ..Default::default()
        });
    });
    let err_exp_acc = handle(
        &ctx, &mut acceptor_sess, "contractProxy", "AcceptContract",
        &[PyRep::Integer(expired_status_cid)], &kw,
    ).unwrap().unwrap_err();
    assert_eq!(err_exp_acc.code, "ContractExpired");

    // Reject zero-quantity requested item before creating valid WTB with duplicate req_tid (15 + 25 = 40)
    // and accept against acceptor's 100-qty stack -> mutates partial_stack_id (100 -> 60) AND mints a new split item (> pre_next_entity_id)!
    assert_eq!(
        handle(
            &ctx, &mut issuer_sess, "contractProxy", "CreateContract",
            &[PyRep::keyval([
                ("contractType", PyRep::Integer(1)),
                ("startStationID", PyRep::Integer(station_id)),
                ("reward", PyRep::Real(100_000.0)),
                ("requestedItems", PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(0)]),
                ])),
            ])],
            &kw,
        ).unwrap().unwrap_err().code,
        "InvalidContractParameters"
    );
    let wtb_res = handle(
        &ctx, &mut issuer_sess, "contractProxy", "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("reward", PyRep::Real(100_000.0)),
            ("requestedItems", PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(15)]),
                PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(25)]),
            ])),
        ])],
        &kw,
    ).unwrap().unwrap();
    let wtb_cid = created_cid(&wtb_res).unwrap();
    let _ = ctx.drain_notifications_for_char(acceptor_id);
    let _ = ctx.drain_notifications_for_char(issuer_id);
    let wtb_acc = handle(
        &ctx, &mut acceptor_sess, "contractProxy", "AcceptContract",
        &[PyRep::Integer(wtb_cid)], &kw,
    ).unwrap().unwrap();
    assert_eq!(dict_str(&wtb_acc, "status"), Some("Completed"));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&partial_stack_id].quantity, 60);
        assert!(snap.items.values().any(|it| it.owner_id == issuer_id && it.type_id == 34 && it.quantity == 40 && it.item_id != partial_stack_id));
    });
    let acc_notifs = ctx.drain_notifications_for_char(acceptor_id);
    let issuer_notifs = ctx.drain_notifications_for_char(issuer_id);
    // Acceptor receives modified stack (100 -> 60) OnItemChange + OnAccountChange; issuer receives newly minted split item (40) OnItemChange
    assert_eq!(acc_notifs.iter().filter(|n| n.event == "OnItemChange").count(), 1);
    assert!(issuer_notifs.iter().any(|n| n.event == "OnItemChange"));
    assert!(acc_notifs.iter().any(|n| n.event == "OnAccountChange"));

    // 3. PlaceBid expired / CompleteContract & FailContract Failed+Courier expiration_unix boundaries
    let exp_auc_cid = 822_002i64;
    let failed_exp_courier_cid = 822_003i64;
    let failed_non_exp_courier_cid = 822_004i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(exp_auc_cid, ContractRecord {
            contract_id: exp_auc_cid,
            issuer_id,
            contract_type: "Auction".into(),
            status: "Expired".into(),
            availability: "Public".into(),
            start_station_id: station_id,
            end_station_id: station_id,
            expiration_unix: now + 10_000,
            price: IskCents::from_isk(1_000),
            ..Default::default()
        });
        snap.contracts.insert(failed_exp_courier_cid, ContractRecord {
            contract_id: failed_exp_courier_cid,
            issuer_id,
            acceptor_id: Some(acceptor_id),
            contract_type: "Courier".into(),
            status: "Failed".into(),
            availability: "Public".into(),
            start_station_id: station_id,
            end_station_id: dest_id,
            expiration_unix: now, // boundary: == now_unix -> ContractExpired on CompleteContract & Failed ok on FailContract!
            ..Default::default()
        });
        snap.contracts.insert(failed_non_exp_courier_cid, ContractRecord {
            contract_id: failed_non_exp_courier_cid,
            issuer_id,
            acceptor_id: Some(acceptor_id),
            contract_type: "Courier".into(),
            status: "Failed".into(),
            availability: "Public".into(),
            start_station_id: station_id,
            end_station_id: dest_id,
            expiration_unix: now + 1, // > now_unix -> NOT due
            ..Default::default()
        });
    });
    assert_eq!(
        handle(&ctx, &mut acceptor_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(exp_auc_cid), PyRep::Real(2_000.0)], &kw)
            .unwrap().unwrap_err().code,
        "ContractExpired"
    );
    assert_eq!(
        handle(&ctx, &mut acceptor_sess, "contractProxy", "CompleteContract", &[PyRep::Integer(failed_exp_courier_cid)], &kw)
            .unwrap().unwrap_err().code,
        "ContractExpired"
    );
    assert_eq!(
        handle(&ctx, &mut acceptor_sess, "contractProxy", "CompleteContract", &[PyRep::Integer(failed_non_exp_courier_cid)], &kw)
            .unwrap().unwrap_err().code,
        "InvalidContractState"
    );
    let fail_ok = handle(&ctx, &mut acceptor_sess, "contractProxy", "FailContract", &[PyRep::Integer(failed_exp_courier_cid)], &kw)
        .unwrap().unwrap();
    assert_eq!(dict_str(&fail_ok, "status"), Some("Failed"));
    assert_eq!(
        handle(&ctx, &mut acceptor_sess, "contractProxy", "FailContract", &[PyRep::Integer(failed_non_exp_courier_cid)], &kw)
            .unwrap().unwrap_err().code,
        "InvalidContractState"
    );

    // 4. BreakPlasticWrap via handle() deletes wrap (ContractNotifyDelta deletion OnItemChange!) & direct break_courier_plastic_wrap 3-tuple
    let courier_item_a = 821_101i64;
    let courier_item_b = 821_102i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(courier_item_a, make_fighter_item(courier_item_a, 34, issuer_id, station_id, InventoryFlag::Hangar, 5));
        snap.items.insert(courier_item_b, make_fighter_item(courier_item_b, 35, issuer_id, station_id, InventoryFlag::Hangar, 7));
    });
    let cour_res = handle(
        &ctx, &mut issuer_sess, "contractProxy", "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(3)), // Courier
            ("startStationID", PyRep::Integer(station_id)),
            ("endStationID", PyRep::Integer(dest_id)),
            ("reward", PyRep::Real(50_000.0)),
            ("collateral", PyRep::Real(200_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(courier_item_a), PyRep::Integer(courier_item_b)])),
        ])],
        &kw,
    ).unwrap().unwrap();
    let cour_cid = created_cid(&cour_res).unwrap();
    let wrap_id = handle(&ctx, &mut acceptor_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(cour_cid)], &kw)
        .unwrap().unwrap().dict_get("courierPackageID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(
        handle(&ctx, &mut acceptor_sess, "contractProxy", "BreakPlasticWrap", &[PyRep::Integer(0)], &kw)
            .unwrap().unwrap_err().code,
        "InvalidArgument"
    );
    let _ = ctx.drain_notifications_for_char(acceptor_id);
    let brk_res = handle(&ctx, &mut acceptor_sess, "contractProxy", "BreakPlasticWrap", &[PyRep::Integer(wrap_id)], &kw)
        .unwrap().unwrap();
    assert_eq!(dict_i64(&brk_res, "contractID"), Some(cour_cid));
    assert_eq!(dict_str(&brk_res, "status"), Some("Failed"));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&wrap_id));
        assert_eq!(snap.items[&courier_item_a].owner_id, acceptor_id);
        assert_eq!(snap.items[&courier_item_a].flag, InventoryFlag::Hangar);
        assert_eq!(snap.items[&courier_item_b].owner_id, acceptor_id);
    });
    // Deleted wrap + 2 extracted modified items emit >= 3 OnItemChange notices
    let brk_notifs = ctx.drain_notifications_for_char(acceptor_id);
    assert!(brk_notifs.iter().filter(|n| n.event == "OnItemChange").count() >= 3);

    // Direct break_courier_plastic_wrap returns exact (cid, issuer_id, item_ids) in Station Hangar branch
    let cour_item_c = 821_103i64;
    let wrap_hangar_id = 821_104i64;
    let direct_cid = 822_005i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(wrap_hangar_id, make_fighter_item(wrap_hangar_id, 3468, acceptor_id, station_id, InventoryFlag::Hangar, 1));
        snap.items.insert(cour_item_c, make_fighter_item(cour_item_c, 34, issuer_id, wrap_hangar_id, InventoryFlag::Locked, 3));
        snap.contracts.insert(direct_cid, ContractRecord {
            contract_id: direct_cid,
            issuer_id,
            acceptor_id: Some(acceptor_id),
            contract_type: "Courier".into(),
            status: "InProgress".into(),
            start_station_id: station_id,
            end_station_id: dest_id,
            item_ids: vec![cour_item_c],
            collateral: IskCents::from_isk(10_000),
            expiration_unix: now + 50_000,
            ..Default::default()
        });
    });
    let tuple = ctx.db.try_with_mut_snapshot(|snap| break_courier_plastic_wrap(snap, acceptor_id, wrap_hangar_id, now)).unwrap();
    assert_eq!(tuple, (direct_cid, issuer_id, vec![cour_item_c]));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&cour_item_c].flag, InventoryFlag::Hangar);
        assert_eq!(snap.items[&cour_item_c].location_id, station_id);
    });

    // 5. SearchContracts: unknown start_station_id falls through region filter (`else { true }`), while known structure filters by region!
    let ghost_station_id = 1_000_000_000_999i64;
    let ghost_cid = 822_006i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(ghost_cid, ContractRecord {
            contract_id: ghost_cid,
            issuer_id,
            contract_type: "ItemExchange".into(),
            status: "Outstanding".into(),
            availability: "Public".into(),
            start_station_id: ghost_station_id,
            end_station_id: ghost_station_id,
            price: IskCents::from_isk(77_777),
            expiration_unix: now + 50_000,
            ..Default::default()
        });
    });
    let srch_kw: HashMap<String, PyRep> = [
        ("regionID".into(), PyRep::Integer(reg_id + 555)),
        ("minPrice".into(), PyRep::Real(77_777.0)),
        ("maxPrice".into(), PyRep::Real(77_777.0)),
    ].into_iter().collect();
    let search_res = handle(&ctx, &mut acceptor_sess, "contractProxy", "SearchContracts", &[], &srch_kw).unwrap().unwrap();
    let ghost_rows = contract_entries(&search_res);
    assert!(ghost_rows.iter().any(|r| r.dict_get("contract").and_then(|c| dict_i64(c, "contractID")) == Some(ghost_cid)));
    assert!(!ghost_rows.iter().any(|r| r.dict_get("contract").and_then(|c| dict_i64(c, "contractID")) == Some(corp_cid)));
    let ghost_row = ghost_rows.iter().find(|r| r.dict_get("contract").and_then(|c| dict_i64(c, "contractID")) == Some(ghost_cid)).unwrap();
    let ghost_contract = ghost_row.dict_get("contract").unwrap();
    assert_eq!(dict_i64(ghost_contract, "startSolarSystemID"), Some(0));
    assert_eq!(dict_i64(ghost_contract, "startRegionID"), Some(0));

    // 6. sweep_expired_contracts_and_notify: no-due fast path, corp Courier Failed (issuer != acceptor + unknown acceptor char skipped),
    //    Auction Completed (char bidder vs corp bidder fallback notify_char), ItemExchange Expired with untracked location_id==cid item!
    assert_eq!(sweep_expired_contracts_and_notify(&ctx, now), 0);

    let _ = ctx.drain_notifications_for_char(issuer_id);
    let _ = ctx.drain_notifications_for_char(acceptor_id);
    let _ = ctx.drain_notifications_for_char(corp_mate_id);

    let sweep_cour_cid = 822_010i64;
    let sweep_auc_char_cid = 822_011i64;
    let sweep_auc_corp_cid = 822_012i64;
    let sweep_ie_untracked_cid = 822_013i64;
    let sweep_zero_exp_cid = 822_014i64;
    let cour_untracked_item = 821_201i64;
    let auc_char_item = 821_202i64;
    let auc_corp_item = 821_203i64;
    let ie_listed_item = 821_204i64;
    let ie_untracked_item = 821_205i64;

    ctx.db.with_mut_snapshot(|snap| {
        // Make sure acceptor_id has character balance changes + missing-char branch tested via sweep_cour_cid & separate unknown acceptor
        snap.items.insert(cour_untracked_item, make_fighter_item(cour_untracked_item, 34, issuer_id, sweep_cour_cid, InventoryFlag::Locked, 1));
        snap.items.insert(auc_char_item, make_fighter_item(auc_char_item, 34, issuer_id, sweep_auc_char_cid, InventoryFlag::Locked, 1));
        snap.items.insert(auc_corp_item, make_fighter_item(auc_corp_item, 34, issuer_id, sweep_auc_corp_cid, InventoryFlag::Locked, 1));
        snap.items.insert(ie_listed_item, make_fighter_item(ie_listed_item, 34, corp_id, sweep_ie_untracked_cid, InventoryFlag::Locked, 1));
        snap.items.insert(ie_untracked_item, make_fighter_item(ie_untracked_item, 35, corp_id, sweep_ie_untracked_cid, InventoryFlag::Locked, 2));

        // 1) InProgress Courier expired -> Failed, for_corp=true with collateral & reward crediting corp div 1, acceptor=acceptor_id
        snap.contracts.insert(sweep_cour_cid, ContractRecord {
            contract_id: sweep_cour_cid,
            issuer_id,
            issuer_corp_id: corp_id,
            for_corp: true,
            acceptor_id: Some(acceptor_id),
            contract_type: "Courier".into(),
            status: "InProgress".into(),
            start_station_id: station_id,
            end_station_id: dest_id,
            collateral: IskCents::from_isk(50_000),
            reward: IskCents::from_isk(10_000),
            expiration_unix: now + 10,
            ..Default::default()
        });
        // 2) Auction won by corp_mate_id (a real character != issuer)
        snap.contracts.insert(sweep_auc_char_cid, ContractRecord {
            contract_id: sweep_auc_char_cid,
            issuer_id,
            contract_type: "Auction".into(),
            status: "Outstanding".into(),
            start_station_id: station_id,
            end_station_id: station_id,
            item_ids: vec![auc_char_item],
            current_bidder_id: Some(corp_mate_id),
            current_bid: IskCents::from_isk(120_000),
            expiration_unix: now + 10,
            ..Default::default()
        });
        // 3) Auction won by buyer_corp (not in characters -> item.owner_id becomes corp, notify_char falls back to issuer_id!)
        snap.contracts.insert(sweep_auc_corp_cid, ContractRecord {
            contract_id: sweep_auc_corp_cid,
            issuer_id,
            contract_type: "Auction".into(),
            status: "Outstanding".into(),
            start_station_id: station_id,
            end_station_id: station_id,
            item_ids: vec![auc_corp_item],
            current_bidder_id: Some(98_000_888),
            current_bid: IskCents::from_isk(90_000),
            expiration_unix: now + 10,
            ..Default::default()
        });
        // 4) Outstanding corp ItemExchange with reward + untracked item (location_id == cid) + missing acceptor char (skipped)
        snap.contracts.insert(sweep_ie_untracked_cid, ContractRecord {
            contract_id: sweep_ie_untracked_cid,
            issuer_id,
            issuer_corp_id: corp_id,
            for_corp: true,
            acceptor_id: Some(unknown_char_id),
            contract_type: "ItemExchange".into(),
            status: "Outstanding".into(),
            start_station_id: station_id,
            end_station_id: station_id,
            item_ids: vec![ie_listed_item],
            reward: IskCents::from_isk(15_000),
            expiration_unix: now + 10,
            ..Default::default()
        });
        // 5) expiration_unix == 0 must NOT be swept
        snap.contracts.insert(sweep_zero_exp_cid, ContractRecord {
            contract_id: sweep_zero_exp_cid,
            issuer_id,
            contract_type: "ItemExchange".into(),
            status: "Outstanding".into(),
            expiration_unix: 0,
            ..Default::default()
        });
    });

    // At now+9, none are due (< expiration_unix); at now+10 (== expiration_unix), all 4 expire!
    assert_eq!(sweep_expired_contracts_and_notify(&ctx, now + 9), 0);
    assert_eq!(sweep_expired_contracts_and_notify(&ctx, now + 10), 4);

    let issuer_sw = ctx.drain_notifications_for_char(issuer_id);
    let acc_sw = ctx.drain_notifications_for_char(acceptor_id);
    let mate_sw = ctx.drain_notifications_for_char(corp_mate_id);
    assert_eq!(ctx.drain_notifications_for_char(unknown_char_id).len(), 0);

    // OnContractFailed sent to both issuer and acceptor
    assert!(issuer_sw.iter().any(|n| n.event == "OnContractFailed"));
    assert!(acc_sw.iter().any(|n| n.event == "OnContractFailed"));
    // OnContractCompleted sent to both issuer and winning char bidder (corp_mate_id)
    assert!(issuer_sw.iter().any(|n| n.event == "OnContractCompleted"));
    assert!(mate_sw.iter().any(|n| n.event == "OnContractCompleted"));
    // OnContractCacheCleared for ItemExchange Expired
    assert!(issuer_sw.iter().any(|n| n.event == "OnContractCacheCleared"));
    // Corp + char OnAccountChange + fallback OnItemChange (for corp winner + untracked item)
    assert!(issuer_sw.iter().any(|n| n.event == "OnAccountChange"));
    assert!(issuer_sw.iter().any(|n| n.event == "OnItemChange"));
    assert!(mate_sw.iter().any(|n| n.event == "OnItemChange"));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ie_untracked_item].location_id, station_id);
        assert_eq!(snap.items[&ie_untracked_item].flag, InventoryFlag::Hangar);
        assert_eq!(snap.contracts[&sweep_zero_exp_cid].status, "Outstanding");
    });
}

#[test]
fn contract_fitted_ship_and_non_empty_container_lifecycle_and_validation() {
    let (
        mut ctx,
        mut issuer_sess,
        issuer_id,
        mut acceptor_sess,
        acceptor_id,
        station_id,
        _dest_id,
        _corp_id,
    ) = setup_extra_contracts_env();
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut
        .types
        .insert(587, make_fighter_type(587, 25, 6, 28_500.0, vec![]));
    sde_mut
        .types
        .insert(485, make_fighter_type(485, 55, 7, 5.0, vec![]));
    sde_mut
        .types
        .insert(31_716, make_fighter_type(31_716, 773, 7, 5.0, vec![]));
    sde_mut
        .types
        .insert(2_454, make_fighter_type(2_454, 100, 18, 5.0, vec![]));

    let kw = HashMap::new();
    let ship_id = 830_001i64;
    let mod_id = 830_002i64;
    let rig_id = 830_003i64;
    let cargo_id = 830_004i64;
    let drone_id = 830_005i64;
    let cont_id = 830_010i64;
    let cont_child_id = 830_011i64;
    let bad_alsc_cont = 830_020i64;
    let bad_alsc_child = 830_021i64;
    let bad_courier_cont = 830_030i64;
    let bad_courier_child = 830_031i64;
    let auc_ship_buyout = 830_040i64;
    let auc_mod_buyout = 830_041i64;
    let auc_ship_finish = 830_050i64;
    let auc_mod_finish = 830_051i64;
    let auc_ship_sweep = 830_060i64;
    let auc_mod_sweep = 830_061i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&issuer_id) {
            ch.balance = IskCents::from_isk(50_000_000);
        }
        if let Some(ch) = snap.characters.get_mut(&acceptor_id) {
            ch.balance = IskCents::from_isk(50_000_000);
        }
        let mk = |item_id, type_id, location_id, flag, quantity, singleton| eve_db::InventoryItemRecord {
            item_id,
            type_id,
            owner_id: issuer_id,
            location_id,
            flag,
            quantity,
            singleton,
            version: 1,
            ..Default::default()
        };
        snap.items.insert(ship_id, mk(ship_id, 587, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(mod_id, mk(mod_id, 485, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(rig_id, mk(rig_id, 31_716, ship_id, InventoryFlag::RigSlot0, 1, true));
        snap.items.insert(cargo_id, mk(cargo_id, 34, ship_id, InventoryFlag::Cargo, 25, false));
        snap.items.insert(drone_id, mk(drone_id, 2_454, ship_id, InventoryFlag::DroneBay, 2, false));
        snap.items.insert(cont_id, mk(cont_id, 17_366, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(cont_child_id, mk(cont_child_id, 35, cont_id, InventoryFlag::Hangar, 12, false));

        snap.items.insert(bad_alsc_cont, mk(bad_alsc_cont, 17_366, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(bad_alsc_child, mk(bad_alsc_child, 34, bad_alsc_cont, InventoryFlag::Hangar, 1, false));
        crate::services::fitting_containers_ships::write_container_ship_state_snap(snap, |st| {
            st.alsc_locked_items.insert(bad_alsc_child);
        });

        snap.items.insert(bad_courier_cont, mk(bad_courier_cont, 17_366, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(bad_courier_child, mk(bad_courier_child, 34, bad_courier_cont, InventoryFlag::Hangar, 1, false));
        snap.item_custom_names.insert(bad_courier_child, "courier_cargo:m1".into());

        snap.items.insert(auc_ship_buyout, mk(auc_ship_buyout, 587, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(auc_mod_buyout, mk(auc_mod_buyout, 485, auc_ship_buyout, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(auc_ship_finish, mk(auc_ship_finish, 587, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(auc_mod_finish, mk(auc_mod_finish, 485, auc_ship_finish, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(auc_ship_sweep, mk(auc_ship_sweep, 587, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(auc_mod_sweep, mk(auc_mod_sweep, 485, auc_ship_sweep, InventoryFlag::HiSlot0, 1, true));
        snap.sp6_lifecycle.self_destructs.insert(
            mod_id,
            eve_db::SelfDestructPersistedEntry {
                ship_id,
                char_id: issuer_id,
                solar_system_id: 30_000_142,
                started_unix: 10,
                detonate_at_unix: 130,
            },
        );
    });

    // Rejections on invalid children or Courier contract
    let err_alsc = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("itemList", PyRep::List(vec![PyRep::Integer(bad_alsc_cont)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_alsc.code, "ContractItemLocked");

    let err_courier_child = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("itemList", PyRep::List(vec![PyRep::Integer(bad_courier_cont)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_courier_child.code, "ItemCannotBeContracted");

    let err_courier_ship = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(3)),
            ("startStationID", PyRep::Integer(station_id)),
            ("endStationID", PyRep::Integer(station_id)),
            ("volume", PyRep::Real(100.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(ship_id)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_courier_ship.code, "CannotContractNonEmptyContainer");

    // Create ItemExchange containing both fitted ship and non-empty container
    let ie_rep = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(100_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(ship_id), PyRep::Integer(cont_id)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let ie_cid = created_cid(&ie_rep).unwrap();

    // Verify detailed items serialization (`parentID`, `flagID`, `inCrate`) and child-type search
    let get_rep = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "GetContract",
        &[PyRep::Integer(ie_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let items_list = match get_rep.dict_get("items") {
        Some(PyRep::List(l)) => l.clone(),
        other => panic!("expected items list, got {other:?}"),
    };
    let find_item = |iid: i64| -> &PyRep {
        items_list
            .iter()
            .find(|r| dict_i64(r, "itemID") == Some(iid))
            .unwrap_or_else(|| panic!("missing item {iid} in GetContract.items"))
    };
    assert_eq!(dict_i64(find_item(ship_id), "parentID"), Some(0));
    assert_eq!(dict_i64(find_item(ship_id), "flagID"), Some(0));
    assert_eq!(find_item(ship_id).dict_get("inCrate").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(dict_i64(find_item(mod_id), "parentID"), Some(ship_id));
    assert_eq!(dict_i64(find_item(mod_id), "flagID"), Some(i64::from(InventoryFlag::HiSlot0 as u16)));
    assert_eq!(dict_i64(find_item(rig_id), "parentID"), Some(ship_id));
    assert_eq!(dict_i64(find_item(rig_id), "flagID"), Some(i64::from(InventoryFlag::RigSlot0 as u16)));
    assert_eq!(dict_i64(find_item(cargo_id), "parentID"), Some(ship_id));
    assert_eq!(dict_i64(find_item(cargo_id), "flagID"), Some(i64::from(InventoryFlag::Cargo as u16)));
    assert_eq!(dict_i64(find_item(drone_id), "parentID"), Some(ship_id));
    assert_eq!(dict_i64(find_item(drone_id), "flagID"), Some(i64::from(InventoryFlag::DroneBay as u16)));
    assert_eq!(dict_i64(find_item(cont_id), "parentID"), Some(0));
    assert_eq!(dict_i64(find_item(cont_child_id), "parentID"), Some(cont_id));
    assert_eq!(dict_i64(find_item(cont_child_id), "flagID"), Some(i64::from(InventoryFlag::Hangar as u16)));

    let search_child_kw = HashMap::from([("itemTypeID".to_string(), PyRep::Integer(485))]);
    let search_child = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "SearchContracts",
        &[],
        &search_child_kw,
    )
    .unwrap()
    .unwrap();
    assert!(contract_entries(&search_child)
        .iter()
        .any(|r| r.dict_get("contract").and_then(|c| dict_i64(c, "contractID")) == Some(ie_cid)));

    let search_miss_kw = HashMap::from([("itemTypeID".to_string(), PyRep::Integer(999_999))]);
    let search_miss = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "SearchContracts",
        &[],
        &search_miss_kw,
    )
    .unwrap()
    .unwrap();
    assert!(contract_entries(&search_miss)
        .iter()
        .all(|r| r.dict_get("contract").and_then(|c| dict_i64(c, "contractID")) != Some(ie_cid)));

    let _ = ctx.drain_notifications_for_char(acceptor_id);
    handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(ie_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Verify ownership transferred and fittings/container hierarchy preserved + OnItemChange emitted
    let acc_notifs = ctx.drain_notifications_for_char(acceptor_id);
    for expected_changed in [ship_id, mod_id, rig_id, cargo_id, drone_id, cont_id, cont_child_id] {
        assert!(
            acc_notifs.iter().any(|n| {
                n.event == "OnItemChange"
                    && n.args
                        .first()
                        .and_then(|row| row.dict_get("itemID"))
                        .and_then(|v| v.as_i64())
                        == Some(expected_changed)
            }),
            "expected OnItemChange for item {expected_changed}"
        );
    }
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ship_id].owner_id, acceptor_id);
        assert_eq!(snap.items[&ship_id].location_id, station_id);
        assert_eq!(snap.items[&ship_id].flag, InventoryFlag::Hangar);
        assert_eq!(snap.items[&mod_id].owner_id, acceptor_id);
        assert_eq!(snap.items[&mod_id].location_id, ship_id);
        assert_eq!(snap.items[&mod_id].flag, InventoryFlag::HiSlot0);
        assert_eq!(snap.items[&rig_id].owner_id, acceptor_id);
        assert_eq!(snap.items[&rig_id].location_id, ship_id);
        assert_eq!(snap.items[&rig_id].flag, InventoryFlag::RigSlot0);
        assert_eq!(snap.items[&cargo_id].owner_id, acceptor_id);
        assert_eq!(snap.items[&cargo_id].location_id, ship_id);
        assert_eq!(snap.items[&cargo_id].flag, InventoryFlag::Cargo);
        assert_eq!(snap.items[&drone_id].owner_id, acceptor_id);
        assert_eq!(snap.items[&drone_id].location_id, ship_id);
        assert_eq!(snap.items[&drone_id].flag, InventoryFlag::DroneBay);
        assert_eq!(snap.items[&cont_id].owner_id, acceptor_id);
        assert_eq!(snap.items[&cont_id].location_id, station_id);
        assert_eq!(snap.items[&cont_id].flag, InventoryFlag::Hangar);
        assert_eq!(snap.items[&cont_child_id].owner_id, acceptor_id);
        assert_eq!(snap.items[&cont_child_id].location_id, cont_id);
        assert!(!snap.sp6_lifecycle.self_destructs.contains_key(&mod_id));
    });

    // Auction buyout, FinishAuction, and sweep expiry all transfer child items
    let auc_buy_rep = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(2)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(10_000.0)),
            ("collateral", PyRep::Real(20_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(auc_ship_buyout)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let auc_buy_cid = created_cid(&auc_buy_rep).unwrap();
    handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_buy_cid), PyRep::Real(20_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap();

    let auc_fin_rep = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(2)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(10_000.0)),
            ("collateral", PyRep::Real(50_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(auc_ship_finish)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let auc_fin_cid = created_cid(&auc_fin_rep).unwrap();
    handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_fin_cid), PyRep::Real(12_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    ctx.clock().advance_secs(200_000);
    crate::services::market_contracts::handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "FinishAuction",
        &[PyRep::Integer(auc_fin_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();

    let auc_sw_rep = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(2)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(10_000.0)),
            ("collateral", PyRep::Real(50_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(auc_ship_sweep)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let auc_sw_cid = created_cid(&auc_sw_rep).unwrap();
    handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_sw_cid), PyRep::Real(12_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let exp_sw = ctx.db.with_snapshot(|s| s.contracts[&auc_sw_cid].expiration_unix);
    let _ = ctx.drain_notifications_for_char(acceptor_id);
    assert_eq!(sweep_expired_contracts_and_notify(&ctx, exp_sw + 1), 1);
    let sw_notifs = ctx.drain_notifications_for_char(acceptor_id);
    assert!(sw_notifs.iter().any(|n| {
        n.event == "OnItemChange"
            && n.args
                .first()
                .and_then(|row| row.dict_get("itemID"))
                .and_then(|v| v.as_i64())
                == Some(auc_mod_sweep)
    }));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&auc_mod_buyout].owner_id, acceptor_id);
        assert_eq!(snap.items[&auc_mod_buyout].location_id, auc_ship_buyout);
        assert_eq!(snap.items[&auc_mod_finish].owner_id, acceptor_id);
        assert_eq!(snap.items[&auc_mod_finish].location_id, auc_ship_finish);
        assert_eq!(snap.items[&auc_mod_sweep].owner_id, acceptor_id);
        assert_eq!(snap.items[&auc_mod_sweep].location_id, auc_ship_sweep);
    });

    // Unsold ItemExchange with fitted ship returns ship to issuer on expiry while keeping modules intact
    let exp_ship = 830_070i64;
    let exp_mod = 830_071i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            exp_ship,
            eve_db::InventoryItemRecord {
                item_id: exp_ship,
                type_id: 587,
                owner_id: issuer_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            exp_mod,
            eve_db::InventoryItemRecord {
                item_id: exp_mod,
                type_id: 485,
                owner_id: issuer_id,
                location_id: exp_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let exp_ie_rep = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(250_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(exp_ship)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let exp_ie_cid = created_cid(&exp_ie_rep).unwrap();
    let exp_ie_ts = ctx.db.with_snapshot(|s| s.contracts[&exp_ie_cid].expiration_unix);
    assert_eq!(sweep_expired_contracts_and_notify(&ctx, exp_ie_ts + 1), 1);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.contracts[&exp_ie_cid].status, "Expired");
        assert_eq!(snap.items[&exp_ship].owner_id, issuer_id);
        assert_eq!(snap.items[&exp_ship].location_id, station_id);
        assert_eq!(snap.items[&exp_ship].flag, InventoryFlag::Hangar);
        assert_eq!(snap.items[&exp_mod].owner_id, issuer_id);
        assert_eq!(snap.items[&exp_mod].location_id, exp_ship);
        assert_eq!(snap.items[&exp_mod].flag, InventoryFlag::HiSlot0);
    });

    // Two-level nesting (ship -> container -> item): duplicate rejection, ancestor lock check,
    // nested parentID chain in GetContract, and recursive ownership transfer on AcceptContract.
    let nest_ship = 830_080i64;
    let nest_cont = 830_081i64;
    let nest_item = 830_082i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mk_item = |item_id, type_id, location_id, flag| eve_db::InventoryItemRecord {
            item_id,
            type_id,
            owner_id: issuer_id,
            location_id,
            flag,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        };
        snap.items.insert(nest_ship, mk_item(nest_ship, 587, station_id, InventoryFlag::Hangar));
        snap.items.insert(nest_cont, mk_item(nest_cont, 17_366, nest_ship, InventoryFlag::Cargo));
        snap.items.insert(nest_item, mk_item(nest_item, 485, nest_cont, InventoryFlag::Cargo));
    });
    let dup_err = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(50_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(nest_ship), PyRep::Integer(nest_ship)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(dup_err.code, "InvalidContractParameters");

    let nest_ie_rep = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(50_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(nest_ship)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let nest_ie_cid = created_cid(&nest_ie_rep).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(snap.is_item_locked_in_contract_or_job(nest_ship));
        assert!(snap.is_item_locked_in_contract_or_job(nest_cont));
        assert!(snap.is_item_locked_in_contract_or_job(nest_item));
    });
    let nest_get = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "GetContract",
        &[PyRep::Integer(nest_ie_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let nest_items = match nest_get.dict_get("items") {
        Some(PyRep::List(l)) => l.clone(),
        other => panic!("expected nested items list, got {other:?}"),
    };
    let find_nest = |iid: i64| -> &PyRep {
        nest_items
            .iter()
            .find(|r| dict_i64(r, "itemID") == Some(iid))
            .unwrap_or_else(|| panic!("missing nested item {iid}"))
    };
    assert_eq!(dict_i64(find_nest(nest_ship), "parentID"), Some(0));
    assert_eq!(dict_i64(find_nest(nest_cont), "parentID"), Some(nest_ship));
    assert_eq!(dict_i64(find_nest(nest_cont), "flagID"), Some(i64::from(InventoryFlag::Cargo as u16)));
    assert_eq!(dict_i64(find_nest(nest_item), "parentID"), Some(nest_cont));
    handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(nest_ie_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&nest_ship].owner_id, acceptor_id);
        assert_eq!(snap.items[&nest_ship].location_id, station_id);
        assert_eq!(snap.items[&nest_cont].owner_id, acceptor_id);
        assert_eq!(snap.items[&nest_cont].location_id, nest_ship);
        assert_eq!(snap.items[&nest_item].owner_id, acceptor_id);
        assert_eq!(snap.items[&nest_item].location_id, nest_cont);
        assert!(!snap.is_item_locked_in_contract_or_job(nest_ship));
        assert!(!snap.is_item_locked_in_contract_or_job(nest_item));
    });

    // RejectContract settles escrowed fitted ship back to issuer and clears self_destructs while preserving modules.
    let rej_ship = 830_090i64;
    let rej_mod = 830_091i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            rej_ship,
            eve_db::InventoryItemRecord {
                item_id: rej_ship,
                type_id: 587,
                owner_id: issuer_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            rej_mod,
            eve_db::InventoryItemRecord {
                item_id: rej_mod,
                type_id: 485,
                owner_id: issuer_id,
                location_id: rej_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for iid in [rej_ship, rej_mod] {
            snap.sp6_lifecycle.self_destructs.insert(
                iid,
                eve_db::SelfDestructPersistedEntry {
                    ship_id: rej_ship,
                    char_id: issuer_id,
                    solar_system_id: 30_000_142,
                    started_unix: 100,
                    detonate_at_unix: 999_999_999,
                },
            );
        }
    });
    let rej_rep = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(1)),
            ("availability", PyRep::Integer(1)),
            ("assigneeID", PyRep::Integer(acceptor_id)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(150_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(rej_ship)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let rej_cid = created_cid(&rej_rep).unwrap();
    handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "RejectContract",
        &[PyRep::Integer(rej_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.contracts[&rej_cid].status, "Rejected");
        assert_eq!(snap.items[&rej_ship].owner_id, issuer_id);
        assert_eq!(snap.items[&rej_ship].location_id, station_id);
        assert_eq!(snap.items[&rej_ship].flag, InventoryFlag::Hangar);
        assert_eq!(snap.items[&rej_mod].owner_id, issuer_id);
        assert_eq!(snap.items[&rej_mod].location_id, rej_ship);
        assert_eq!(snap.items[&rej_mod].flag, InventoryFlag::HiSlot0);
        assert!(!snap.sp6_lifecycle.self_destructs.contains_key(&rej_ship));
        assert!(!snap.sp6_lifecycle.self_destructs.contains_key(&rej_mod));
        assert!(snap.kv(eve_db::Ns::ContractOrigFlag).get::<u16>(rej_cid, &rej_ship.to_string()).is_none());
    });

    // FinishAuction delivers fitted ship to CorpDeliveries and transfers fitted modules to the winning corporation.
    let corp_auc_ship = 830_100i64;
    let corp_auc_mod = 830_101i64;
    let corp_bid_id = 98_000_888i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_bid_id;
        corp.name = "BidCorp".into();
        corp.ticker = "BIDC".into();
        corp.ceo_id = acceptor_id;
        corp.wallet_divisions = vec![CorpWalletDivisionRecord {
            division_id: 1,
            name: "Master".into(),
            balance: IskCents::from_isk(50_000_000),
        }];
        corp.set_division_balance(1, IskCents::from_isk(50_000_000));
        snap.corporations.insert(corp_bid_id, corp);
        if let Some(ch) = snap.characters.get_mut(&acceptor_id) {
            ch.corporation_id = corp_bid_id;
        }
        snap.items.insert(
            corp_auc_ship,
            eve_db::InventoryItemRecord {
                item_id: corp_auc_ship,
                type_id: 587,
                owner_id: issuer_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            corp_auc_mod,
            eve_db::InventoryItemRecord {
                item_id: corp_auc_mod,
                type_id: 485,
                owner_id: issuer_id,
                location_id: corp_auc_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut corp_bid_sess = acceptor_sess.clone();
    corp_bid_sess.corporation_id = Some(corp_bid_id);
    corp_bid_sess.corp_account_key = 1000;
    let corp_auc_rep = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::Integer(2)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(10_000.0)),
            ("collateral", PyRep::Real(50_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(corp_auc_ship)])),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let corp_auc_cid = created_cid(&corp_auc_rep).unwrap();
    handle(
        &ctx,
        &mut corp_bid_sess,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(corp_auc_cid), PyRep::Real(12_000.0), PyRep::Bool(true)],
        &kw,
    )
    .unwrap()
    .unwrap();
    ctx.clock().advance_secs(200_000);
    crate::services::market_contracts::handle(
        &ctx,
        &mut corp_bid_sess,
        "contractProxy",
        "FinishAuction",
        &[PyRep::Integer(corp_auc_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.contracts[&corp_auc_cid].status, "Finished");
        assert_eq!(snap.items[&corp_auc_ship].owner_id, corp_bid_id);
        assert_eq!(snap.items[&corp_auc_ship].location_id, station_id);
        assert_eq!(snap.items[&corp_auc_ship].flag, InventoryFlag::CorpDeliveries);
        assert_eq!(snap.items[&corp_auc_mod].owner_id, corp_bid_id);
        assert_eq!(snap.items[&corp_auc_mod].location_id, corp_auc_ship);
        assert_eq!(snap.items[&corp_auc_mod].flag, InventoryFlag::HiSlot0);
        assert!(snap.kv(eve_db::Ns::ContractBidDiv).get::<u8>(corp_auc_cid, &corp_bid_id.to_string()).is_none());
    });
}
