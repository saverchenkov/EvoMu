use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use eve_db::{InventoryFlag, IskCents, UpwellStructureRecord};
use std::collections::HashMap;
use std::sync::Arc;

fn dict_i64(r: &PyRep, k: &str) -> Option<i64> {
    r.dict_get(k).and_then(|v| v.as_i64())
}
fn dict_str<'a>(r: &'a PyRep, k: &str) -> Option<&'a str> {
    r.dict_get(k).and_then(|v| v.as_str())
}
fn dict_bool(r: &PyRep, k: &str) -> Option<bool> {
    r.dict_get(k).and_then(|v| v.as_bool())
}

fn setup_contracts_ctx() -> (ServerContext, SessionState, i64, SessionState, i64, i64, i64) {
    let (mut ctx, mut issuer_sess, issuer_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let station_id = 1_000_000_000_001_i64;
    let dest_station_id = 1_000_000_000_002_i64;
    let acceptor_id = 90_000_002_i64;
    let acceptor_ship_id = 100_000_002_i64;

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut.types.insert(34, make_fighter_type(34, 18, 4, 0.01, vec![]));
    sde_mut.types.insert(35, make_fighter_type(35, 18, 4, 0.01, vec![]));

    issuer_sess.station_id = Some(station_id);
    let mut acceptor_sess = issuer_sess.clone();
    acceptor_sess.character_id = Some(acceptor_id);
    acceptor_sess.ship_id = Some(acceptor_ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.get(&issuer_id).cloned().unwrap();
        if let Some(ch) = snap.characters.get_mut(&issuer_id) {
            ch.station_id = Some(station_id);
            ch.balance = IskCents::from_isk(500_000_000);
        }
        let mut acc_char = base_char;
        acc_char.character_id = acceptor_id;
        acc_char.user_id = 42;
        acc_char.name = "Acceptor".into();
        acc_char.station_id = Some(station_id);
        acc_char.solar_system_id = sys_id;
        acc_char.active_ship_id = acceptor_ship_id;
        acc_char.balance = IskCents::from_isk(500_000_000);
        snap.characters.insert(acceptor_id, acc_char);
        snap.items.insert(
            acceptor_ship_id,
            make_fighter_item(
                acceptor_ship_id,
                600,
                acceptor_id,
                station_id,
                InventoryFlag::Hangar,
                1,
            ),
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
        station_id,
        dest_station_id,
    )
}

#[test]
fn test_item_exchange_and_auction_contract_lifecycle() {
    let (ctx, mut issuer_sess, issuer_id, mut acceptor_sess, acceptor_id, station_id, _) =
        setup_contracts_ctx();
    let kwargs = HashMap::new();

    let item1 = 800_001_i64;
    let item2 = 800_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item1,
            make_fighter_item(item1, 34, issuer_id, station_id, InventoryFlag::Hangar, 100),
        );
        snap.items.insert(
            item2,
            make_fighter_item(item2, 35, issuer_id, station_id, InventoryFlag::Hangar, 50),
        );
    });

    // 1. Create ItemExchange contract
    let create_payload = PyRep::keyval([
        ("contractType", PyRep::String("ItemExchange".into())),
        ("availability", PyRep::String("Public".into())),
        ("startStationID", PyRep::Integer(station_id)),
        ("price", PyRep::Real(2_000_000.0)),
        ("title", PyRep::String("Sell Tritanium".into())),
        ("itemList", PyRep::List(vec![PyRep::Integer(item1)])),
    ]);
    let created = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[create_payload],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let cid = dict_i64(&created, "contractID").unwrap();

    // GetContract & SearchContracts
    let info = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "GetContract",
        &[PyRep::Integer(cid)],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&info, "status"), Some("Outstanding"));

    let search = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "SearchContracts",
        &[],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(search, PyRep::List(ref l) if !l.is_empty()));

    // Accept ItemExchange contract
    let accepted = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(cid)],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&accepted, "status"), Some("Completed"));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&item1).map(|i| i.owner_id)),
        Some(acceptor_id)
    );

    // 2. Create Auction contract, place bid, then buyout
    let auc_payload = PyRep::keyval([
        ("contractType", PyRep::String("Auction".into())),
        ("availability", PyRep::String("Public".into())),
        ("startStationID", PyRep::Integer(station_id)),
        ("price", PyRep::Real(1_000_000.0)),
        ("collateral", PyRep::Real(5_000_000.0)), // buyout
        ("itemList", PyRep::List(vec![PyRep::Integer(item2)])),
    ]);
    let auc_created = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[auc_payload],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let auc_cid = dict_i64(&auc_created, "contractID").unwrap();

    let bid1 = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(1_500_000.0)],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_bool(&bid1, "boughtOut"), Some(false));

    let my_bids = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "GetMyBids",
        &[],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(my_bids, PyRep::List(ref l) if !l.is_empty()));

    let buyout = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(5_000_000.0)],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_bool(&buyout, "boughtOut"), Some(true));
    assert_eq!(dict_str(&buyout, "status"), Some("Completed"));
}

#[test]
fn test_courier_contract_complete_and_break_plastic_wrap() {
    let (ctx, mut issuer_sess, issuer_id, mut acceptor_sess, acceptor_id, station_id, dest_id) =
        setup_contracts_ctx();
    let kwargs = HashMap::new();
    let item1 = 810_001_i64;
    let item2 = 810_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item1,
            make_fighter_item(item1, 34, issuer_id, station_id, InventoryFlag::Hangar, 10),
        );
        snap.items.insert(
            item2,
            make_fighter_item(item2, 35, issuer_id, station_id, InventoryFlag::Hangar, 10),
        );
    });

    // 1. Courier contract accepted and completed at destination
    let courier1 = PyRep::keyval([
        ("contractType", PyRep::String("Courier".into())),
        ("startStationID", PyRep::Integer(station_id)),
        ("endStationID", PyRep::Integer(dest_id)),
        ("reward", PyRep::Real(500_000.0)),
        ("collateral", PyRep::Real(1_000_000.0)),
        ("itemList", PyRep::List(vec![PyRep::Integer(item1)])),
    ]);
    let c1 = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[courier1],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let cid1 = dict_i64(&c1, "contractID").unwrap();
    let acc1 = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(cid1)],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let wrap1 = dict_i64(&acc1, "courierPackageID").unwrap();

    // Move acceptor + ship + wrap to destination station
    let acc_ship = acceptor_sess.ship_id.unwrap();
    acceptor_sess.station_id = Some(dest_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&acceptor_id) {
            ch.station_id = Some(dest_id);
        }
        if let Some(sh) = snap.items.get_mut(&acc_ship) {
            sh.location_id = dest_id;
        }
        if let Some(w) = snap.items.get_mut(&wrap1) {
            w.location_id = dest_id;
        }
    });
    let comp1 = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "CompleteContract",
        &[PyRep::Integer(cid1)],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&comp1, "status"), Some("Completed"));

    // 2. Courier contract accepted and BreakPlasticWrap called
    acceptor_sess.station_id = Some(station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&acceptor_id) {
            ch.station_id = Some(station_id);
        }
    });
    let courier2 = PyRep::keyval([
        ("contractType", PyRep::String("Courier".into())),
        ("startStationID", PyRep::Integer(station_id)),
        ("endStationID", PyRep::Integer(dest_id)),
        ("reward", PyRep::Real(500_000.0)),
        ("collateral", PyRep::Real(1_000_000.0)),
        ("itemList", PyRep::List(vec![PyRep::Integer(item2)])),
    ]);
    let c2 = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[courier2],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let cid2 = dict_i64(&c2, "contractID").unwrap();
    let acc2 = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(cid2)],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let wrap2 = dict_i64(&acc2, "courierPackageID").unwrap();

    let broken = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "BreakPlasticWrap",
        &[PyRep::Integer(wrap2)],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_str(&broken, "status"), Some("Failed"));
}

#[test]
fn test_contract_item_flags_asset_safety_alsc_and_corp_rbac_security() {
    use eve_db::corp_roles;
    let (ctx, mut issuer_sess, issuer_id, mut acceptor_sess, acceptor_id, station_id, _dest_id) =
        setup_contracts_ctx();
    let kwargs = HashMap::new();
    let corp_id = 98_900_001_i64;
    let other_ceo = 90_000_999_i64;
    let buyer_corp_id = 98_900_002_i64;
    let fitted_rig_id = 820_001_i64;
    let qcore_id = 820_002_i64;
    let wrap_item_id = 820_003_i64;
    let alsc_item_id = 820_004_i64;
    let corp_sag2_item_id = 820_005_i64;
    let corp_sag2_item_2 = 820_006_i64;
    let buyer_sag2_req_id = 820_007_i64;
    let personal_offer_id = 820_008_i64;
    let auc_item_id = 820_009_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Security Test Corp".into();
        corp.ticker = "SEC".into();
        corp.ceo_id = other_ceo;
        corp.members = [other_ceo, issuer_id].into_iter().collect();
        corp.ensure_wallet_divisions();
        corp.set_division_balance(1, IskCents::from_isk(50_000_000));
        snap.corporations.insert(corp_id, corp);

        let mut buyer_corp = eve_db::CorporationRecord::default();
        buyer_corp.corporation_id = buyer_corp_id;
        buyer_corp.name = "Buyer Corp".into();
        buyer_corp.ticker = "BUY".into();
        buyer_corp.ceo_id = other_ceo;
        buyer_corp.members = [other_ceo, acceptor_id].into_iter().collect();
        buyer_corp.ensure_wallet_divisions();
        buyer_corp.set_division_balance(1, IskCents::from_isk(50_000_000));
        snap.corporations.insert(buyer_corp_id, buyer_corp);

        if let Some(ch) = snap.characters.get_mut(&issuer_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(50_000_000);
        }
        if let Some(ch) = snap.characters.get_mut(&acceptor_id) {
            ch.corporation_id = buyer_corp_id;
            ch.balance = IskCents::from_isk(50_000_000);
        }

        snap.items.insert(
            fitted_rig_id,
            make_fighter_item(fitted_rig_id, 34, issuer_id, station_id, InventoryFlag::RigSlot0, 1),
        );
        snap.items.insert(
            qcore_id,
            make_fighter_item(qcore_id, 42_685, corp_id, station_id, InventoryFlag::QuantumCoreRoom, 1),
        );
        snap.items.insert(
            wrap_item_id,
            make_fighter_item(wrap_item_id, 60, issuer_id, station_id, InventoryFlag::Hangar, 1),
        );
        snap.items.insert(
            alsc_item_id,
            make_fighter_item(alsc_item_id, 34, issuer_id, station_id, InventoryFlag::Hangar, 5),
        );
        snap.items.insert(
            corp_sag2_item_id,
            make_fighter_item(corp_sag2_item_id, 34, corp_id, station_id, InventoryFlag::CorpSAG2, 10),
        );
        snap.items.insert(
            corp_sag2_item_2,
            make_fighter_item(corp_sag2_item_2, 34, corp_id, station_id, InventoryFlag::CorpSAG2, 10),
        );
        snap.items.insert(
            buyer_sag2_req_id,
            make_fighter_item(buyer_sag2_req_id, 35, buyer_corp_id, station_id, InventoryFlag::CorpSAG2, 20),
        );
        snap.items.insert(
            personal_offer_id,
            make_fighter_item(personal_offer_id, 34, issuer_id, station_id, InventoryFlag::Hangar, 5),
        );
        snap.items.insert(
            auc_item_id,
            make_fighter_item(auc_item_id, 34, issuer_id, station_id, InventoryFlag::Hangar, 5),
        );
    });
    crate::services::inventory_fitting::fitting_containers_ships::write_container_ship_state(
        &ctx,
        |cs| {
            cs.alsc_locked_items.insert(alsc_item_id);
        },
    );

    // 1. Fitted rig/module rejected for personal contract
    let err_rig = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("ItemExchange".into())),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(100_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(fitted_rig_id)])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_rig.code, "ContractItemFlagInvalid");

    // 2. AssetSafetyWrap (type_id 60) rejected
    let err_wrap = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("ItemExchange".into())),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(100_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(wrap_item_id)])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_wrap.code, "ItemCannotBeContracted");

    // 3. ALSC locked item rejected
    let err_alsc = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("ItemExchange".into())),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(100_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(alsc_item_id)])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_alsc.code, "ContractItemLocked");

    // 4. Stale session.corp_role ignored when DB has no corp role; QuantumCoreRoom rejected even for Director
    issuer_sess.corporation_id = Some(corp_id);
    issuer_sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_CONTRACT_MANAGER;
    let err_stale = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("ItemExchange".into())),
            ("forCorp", PyRep::Bool(true)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(100_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(corp_sag2_item_id)])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_stale.code, "CrpAccessDenied");

    // Grant ROLE_CONTRACT_MANAGER + ROLE_ACCOUNT_CAN_TAKE[0] + ROLE_HANGAR_CAN_TAKE[0] (wrong div!) + pending 24h ROLE_HANGAR_CAN_TAKE[1]
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles.insert(
                issuer_id,
                corp_roles::ROLE_CONTRACT_MANAGER
                    | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]
                    | corp_roles::ROLE_HANGAR_CAN_TAKE[0],
            );
        }
        snap.corp_governance.entry(corp_id).or_default().pending_role_grants.insert(
            issuer_id,
            (corp_roles::ROLE_HANGAR_CAN_TAKE[1], now + 86_400),
        );
    });

    // QuantumCoreRoom rejected even with roles
    let err_qcore = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("ItemExchange".into())),
            ("forCorp", PyRep::Bool(true)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(100_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(qcore_id)])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_qcore.code, "ContractItemFlagInvalid");

    // 24h stasis blocks CorpSAG2 take role before maturation
    let err_stasis = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("ItemExchange".into())),
            ("forCorp", PyRep::Bool(true)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(100_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(corp_sag2_item_id)])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_stasis.code, "CrpAccessDenied");

    // Advance clock 24h + 1s -> pending grant matures -> CreateContract succeeds, and DeleteContract restores CorpSAG2!
    ctx.advance_virtual_clock(86_401);
    let created_del = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("ItemExchange".into())),
            ("forCorp", PyRep::Bool(true)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(100_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(corp_sag2_item_id)])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let cid_del = dict_i64(&created_del, "contractID").unwrap();
    handle(&ctx, &mut issuer_sess, "contractProxy", "DeleteContract", &[PyRep::Integer(cid_del)], &kwargs)
        .unwrap()
        .unwrap();
    assert_eq!(ctx.db.get_item(corp_sag2_item_id).unwrap().flag, InventoryFlag::CorpSAG2);

    // Expiry ALSO restores CorpSAG2!
    let created_exp = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("ItemExchange".into())),
            ("forCorp", PyRep::Bool(true)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(100_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(corp_sag2_item_2)])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let cid_exp = dict_i64(&created_exp, "contractID").unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.get_mut(&cid_exp).unwrap().expiration_unix = 1;
        assert_eq!(snap.resolve_expired_contract(cid_exp, ctx.now_unix()), Some("Expired"));
    });
    assert_eq!(ctx.db.get_item(corp_sag2_item_2).unwrap().flag, InventoryFlag::CorpSAG2);

    // 5. Corp AcceptContract requires division take role for requested_items & ROLE_ACCOUNT_CAN_TAKE for required_isk
    let personal_req = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("ItemExchange".into())),
            ("forCorp", PyRep::Bool(false)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(250_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(personal_offer_id)])),
            ("requestedItems", PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(35), PyRep::Integer(5)])])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let cid_req = dict_i64(&personal_req, "contractID").unwrap();

    acceptor_sess.corporation_id = Some(buyer_corp_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&buyer_corp_id) {
            c.member_roles.insert(acceptor_id, corp_roles::ROLE_CONTRACT_MANAGER);
        }
    });
    let err_acc_div = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(cid_req), PyRep::Bool(true)],
        &kwargs,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_acc_div.code, "MissingRequestedContractItems");

    // Grant CorpSAG2 take role, still missing ROLE_ACCOUNT_CAN_TAKE[0] -> CrpAccessDenied
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&buyer_corp_id) {
            c.member_roles.insert(
                acceptor_id,
                corp_roles::ROLE_CONTRACT_MANAGER | corp_roles::ROLE_HANGAR_CAN_TAKE[1],
            );
        }
    });
    let err_acc_acct = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(cid_req), PyRep::Bool(true)],
        &kwargs,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_acc_acct.code, "CrpAccessDenied");

    // 6. PlaceBid(forCorp = true) requires ROLE_ACCOUNT_CAN_TAKE[0], then succeeds when granted
    let auc_res = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("Auction".into())),
            ("forCorp", PyRep::Bool(false)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(100_000.0)),
            ("collateral", PyRep::Real(500_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(auc_item_id)])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let auc_cid = dict_i64(&auc_res, "contractID").unwrap();
    let err_bid_acct = handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(150_000.0), PyRep::Bool(true)],
        &kwargs,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_bid_acct.code, "CrpAccessDenied");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&buyer_corp_id) {
            c.member_roles.insert(
                acceptor_id,
                corp_roles::ROLE_CONTRACT_MANAGER
                    | corp_roles::ROLE_HANGAR_CAN_TAKE[1]
                    | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0],
            );
        }
    });
    handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "AcceptContract",
        &[PyRep::Integer(cid_req), PyRep::Bool(true)],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    handle(
        &ctx,
        &mut acceptor_sess,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_cid), PyRep::Real(150_000.0), PyRep::Bool(true)],
        &kwargs,
    )
    .unwrap()
    .unwrap();
}

#[test]
fn sweep_expired_contracts_and_notify_emits_expiry_and_returns_escrow() {
    let (ctx, mut issuer_sess, issuer_id, _acc_sess, _acc_id, station_id, _dest_id) =
        setup_contracts_ctx();
    let now = 1_750_000_000_u64;
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(now)));
    ctx.db.acquire_online_lock(1, 501, Some(issuer_id), true, now).unwrap();

    let item_id = 888_111_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            make_fighter_item(item_id, 34, issuer_id, station_id, InventoryFlag::Hangar, 100),
        );
    });
    let kwargs = HashMap::new();
    let res = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("ItemExchange".into())),
            ("forCorp", PyRep::Bool(false)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(100_000.0)),
            ("reward", PyRep::Real(25_000.0)),
            ("expiretime", PyRep::Integer(1440)),
            ("itemList", PyRep::List(vec![PyRep::Integer(item_id)])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let cid = dict_i64(&res, "contractID").unwrap();
    let exp_unix = ctx.db.with_snapshot(|s| s.contracts[&cid].expiration_unix);
    let _ = ctx.drain_notifications_for_char(issuer_id);

    let swept = sweep_expired_contracts_and_notify(&ctx, exp_unix);
    assert_eq!(swept, 1);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.contracts[&cid].status, "Expired");
        assert_eq!(snap.items[&item_id].owner_id, issuer_id);
        assert_eq!(snap.items[&item_id].flag, InventoryFlag::Hangar);
    });
    let notifs = ctx.drain_notifications_for_char(issuer_id);
    assert!(notifs.iter().any(|n| n.event == "OnContractCacheCleared"));
    assert!(notifs.iter().any(|n| n.event == "OnItemChange"));
}
