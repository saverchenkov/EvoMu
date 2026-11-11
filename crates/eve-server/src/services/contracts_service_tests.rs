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

    // Second sweep at same timestamp finds 0 newly expired contracts
    assert_eq!(sweep_expired_contracts_and_notify(&ctx, exp_unix), 0);
}

#[test]
fn test_contracts_error_guards_lists_corp_divisions_and_courier_wrap_break() {
    use eve_db::{corp_roles, ContractRecord, InventoryFlag};

    let (mut ctx, mut issuer_sess, issuer_id, mut acceptor_sess, acceptor_id, station_id, dest_station_id) =
        setup_contracts_ctx();
    let kwargs = HashMap::new();
    let now = ctx.now_unix();
    let corp_id = 98_444_001_i64;
    let buyer_corp_id = 98_444_002_i64;

    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        sde_mut.types.insert(3468, make_fighter_type(3468, 22, 4, 1.0, vec![])); // plastic wrap
    }

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&issuer_id) {
            ch.corporation_id = corp_id;
        }
        if let Some(ch) = snap.characters.get_mut(&acceptor_id) {
            ch.corporation_id = buyer_corp_id;
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Issuer Corp".into(); __corp.ticker = "ISSC".into(); __corp.ceo_id = issuer_id; __corp.wallet_divisions = vec![
                    eve_db::CorpWalletDivisionRecord {
                        division_id: 1,
                        name: "Master".into(),
                        balance: IskCents::from_isk(50_000_000),
                    },
                    eve_db::CorpWalletDivisionRecord {
                        division_id: 2,
                        name: "Div2".into(),
                        balance: IskCents::from_isk(100), // too poor for reward
                    },
                ]; __corp.members = [issuer_id].into_iter().collect(); __corp.member_roles = [(
                    issuer_id,
                    corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_CONTRACT_MANAGER,
                )]
                .into_iter()
                .collect(); let __mw = IskCents::from_isk(50_000_000); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp },
        );
        snap.corporations.insert(
            buyer_corp_id,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = buyer_corp_id; __corp.name = "Buyer Corp".into(); __corp.ticker = "BYRC".into(); __corp.ceo_id = acceptor_id; __corp.wallet_divisions = vec![eve_db::CorpWalletDivisionRecord {
                    division_id: 1,
                    name: "Master".into(),
                    balance: IskCents::from_isk(100_000_000),
                }]; __corp.members = [acceptor_id].into_iter().collect(); __corp.member_roles = [(
                    acceptor_id,
                    corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_CONTRACT_MANAGER,
                )]
                .into_iter()
                .collect(); let __mw = IskCents::from_isk(100_000_000); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp },
        );
    });
    issuer_sess.corporation_id = Some(corp_id);
    issuer_sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_CONTRACT_MANAGER;
    acceptor_sess.corporation_id = Some(buyer_corp_id);
    acceptor_sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_CONTRACT_MANAGER;

    // 1. CreateContract corp division 2 InsufficientFunds + Sag1..Sag7 flag checks
    let item_sag3 = 810_001_i64;
    let item_sag5 = 810_002_i64;
    let item_courier = 810_003_i64;
    let item_req_split = 810_004_i64;
    let item_auc = 810_005_i64;
    let item_exp = 810_006_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_sag3, make_fighter_item(item_sag3, 34, corp_id, station_id, InventoryFlag::CorpSAG3, 10));
        snap.items.insert(item_sag5, make_fighter_item(item_sag5, 34, corp_id, station_id, InventoryFlag::CorpSAG5, 10));
        snap.items.insert(item_courier, make_fighter_item(item_courier, 34, corp_id, station_id, InventoryFlag::CorpDeliveries, 20));
        snap.items.insert(item_req_split, make_fighter_item(item_req_split, 35, buyer_corp_id, station_id, InventoryFlag::CorpSAG1, 100));
        snap.items.insert(item_auc, make_fighter_item(item_auc, 34, corp_id, station_id, InventoryFlag::CorpSAG2, 15));
        snap.items.insert(item_exp, make_fighter_item(item_exp, 34, issuer_id, station_id, InventoryFlag::Hangar, 5));
    });

    issuer_sess.corp_account_key = 1001; // division 2 (poor balance = 100 ISK)
    let err_div2 = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("Courier".into())),
            ("forCorp", PyRep::Bool(true)),
            ("startStationID", PyRep::Integer(station_id)),
            ("endStationID", PyRep::Integer(dest_station_id)),
            ("reward", PyRep::Real(500_000.0)),
            ("collateral", PyRep::Real(100_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(item_sag3)])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_div2.code, "InsufficientFunds");

    issuer_sess.corp_account_key = 1000; // reset to master division 1
    // Create corp ItemExchange from SAG3 requesting 20 of type 35 (acceptor has 100 -> tests partial stack split + corp credit)
    let corp_ix = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("ItemExchange".into())),
            ("forCorp", PyRep::Bool(true)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(100_000.0)),
            ("reward", PyRep::Real(50_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(item_sag3), PyRep::Integer(item_sag5)])),
            ("requestItemTypeList", PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(35), PyRep::Integer(20)])])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let corp_ix_cid = dict_i64(&corp_ix, "contractID").unwrap();
    assert!(
        handle(&ctx, &mut acceptor_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(corp_ix_cid), PyRep::Bool(true)], &kwargs)
            .unwrap()
            .is_ok()
    );

    // 2. AcceptContract error paths: ContractNotFound, InvalidContractState, InvalidContractType, MustBeDockedAtStartStation, NoActiveShip, ContractExpired
    assert_eq!(
        handle(&ctx, &mut acceptor_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(999_888_777)], &kwargs)
            .unwrap()
            .unwrap_err()
            .code,
        "ContractNotFound"
    );
    assert_eq!(
        handle(&ctx, &mut acceptor_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(corp_ix_cid)], &kwargs)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidContractState"
    );
    let exp_res = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("Courier".into())),
            ("startStationID", PyRep::Integer(station_id)),
            ("endStationID", PyRep::Integer(dest_station_id)),
            ("reward", PyRep::Real(50_000.0)),
            ("collateral", PyRep::Real(100_000.0)),
            ("numDays", PyRep::Integer(1)),
            ("itemList", PyRep::List(vec![PyRep::Integer(item_exp)])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let exp_cid = dict_i64(&exp_res, "contractID").unwrap();
    // Accept from wrong station -> MustBeDockedAtStartStation
    acceptor_sess.station_id = Some(dest_station_id);
    assert_eq!(
        handle(&ctx, &mut acceptor_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(exp_cid)], &kwargs)
            .unwrap()
            .unwrap_err()
            .code,
        "MustBeDockedAtStartStation"
    );
    // Accept Courier without active ship -> NoActiveShip
    acceptor_sess.station_id = Some(station_id);
    let saved_ship = acceptor_sess.ship_id;
    acceptor_sess.ship_id = None;
    assert_eq!(
        handle(&ctx, &mut acceptor_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(exp_cid)], &kwargs)
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    acceptor_sess.ship_id = saved_ship;
    // Force expiration in snapshot -> AcceptContract returns ContractExpired
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.contracts.get_mut(&exp_cid) {
            c.expiration_unix = now.saturating_sub(100);
        }
    });
    assert_eq!(
        handle(&ctx, &mut acceptor_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(exp_cid)], &kwargs)
            .unwrap()
            .unwrap_err()
            .code,
        "ContractExpired"
    );

    // 3. PlaceBid on Auction: corp bid then buyout refunds corp bid and transfers item + InsufficientFunds
    let corp_auc = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("Auction".into())),
            ("forCorp", PyRep::Bool(true)),
            ("startStationID", PyRep::Integer(station_id)),
            ("price", PyRep::Real(100_000.0)),
            ("collateral", PyRep::Real(500_000.0)),
            ("itemList", PyRep::List(vec![PyRep::Integer(item_auc)])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let corp_auc_cid = dict_i64(&corp_auc, "contractID").unwrap();
    // Corp places bid
    assert!(handle(&ctx, &mut acceptor_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(corp_auc_cid), PyRep::Real(150_000.0), PyRep::Bool(true)], &kwargs).unwrap().is_ok());
    // InsufficientFunds bid from poor 3rd char
    let third_id = 90_000_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&acceptor_id).cloned().unwrap();
        ch.character_id = third_id;
        ch.balance = IskCents::from_isk(10);
        snap.characters.insert(third_id, ch);
    });
    let mut third_sess = acceptor_sess.clone();
    third_sess.character_id = Some(third_id);
    assert_eq!(
        handle(&ctx, &mut third_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(corp_auc_cid), PyRep::Real(200_000.0), PyRep::Bool(false)], &kwargs)
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );
    // Query GetMyBids with isAccepted = false while Outstanding
    let my_bids_open = handle(&ctx, &mut acceptor_sess, "contractProxy", "GetMyBids", &[PyRep::Bool(false), PyRep::Integer(buyer_corp_id)], &kwargs)
        .unwrap()
        .unwrap();
    assert!(matches!(my_bids_open, PyRep::List(l) if !l.is_empty()));

    // Outbid + buyout by third_id (give them funds)
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&third_id).unwrap().balance = IskCents::from_isk(10_000_000);
    });
    assert!(handle(&ctx, &mut third_sess, "contractProxy", "PlaceBid", &[PyRep::Integer(corp_auc_cid), PyRep::Real(500_000.0), PyRep::Bool(false)], &kwargs).unwrap().is_ok());
    // Verify GetMyBids(isAccepted=true) for third_sess
    let my_bids_won = handle(&ctx, &mut third_sess, "contractProxy", "GetMyBids", &[PyRep::Bool(true)], &kwargs)
        .unwrap()
        .unwrap();
    assert!(matches!(my_bids_won, PyRep::List(l) if !l.is_empty()));

    // 4. Corp Courier: Accept + FinishContract error guards + happy FinishContract + break_courier_plastic_wrap_and_fail_contract
    let corp_cour = handle(
        &ctx,
        &mut issuer_sess,
        "contractProxy",
        "CreateContract",
        &[PyRep::keyval([
            ("contractType", PyRep::String("Courier".into())),
            ("forCorp", PyRep::Bool(true)),
            ("startStationID", PyRep::Integer(station_id)),
            ("endStationID", PyRep::Integer(dest_station_id)),
            ("reward", PyRep::Real(100_000.0)),
            ("collateral", PyRep::Real(500_000.0)),
            ("numDays", PyRep::Integer(2)),
            ("itemList", PyRep::List(vec![PyRep::Integer(item_courier)])),
        ])],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let corp_cour_cid = dict_i64(&corp_cour, "contractID").unwrap();
    let acc_res = handle(&ctx, &mut acceptor_sess, "contractProxy", "AcceptContract", &[PyRep::Integer(corp_cour_cid), PyRep::Bool(true)], &kwargs)
        .unwrap()
        .unwrap();
    let wrap_id = dict_i64(&acc_res, "courierPackageID").unwrap();

    // Break wrap error: wrong location, then move wrap into ship cargo and break wrap to fail contract
    acceptor_sess.station_id = Some(dest_station_id);
    assert_eq!(
        ctx.db.try_with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&acceptor_id) {
                ch.station_id = Some(dest_station_id);
            }
            break_courier_plastic_wrap(snap, acceptor_id, wrap_id, now)
        })
        .unwrap_err()
        .code,
        "CustomInfo"
    );
    // Move character & wrap into active_ship cargo and break wrap!
    let acc_ship = acceptor_sess.ship_id.unwrap();
    let broken = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&acceptor_id) {
            ch.station_id = Some(station_id);
        }
        if let Some(sh) = snap.items.get_mut(&acc_ship) {
            sh.location_id = station_id;
        }
        if let Some(w) = snap.items.get_mut(&wrap_id) {
            w.location_id = acc_ship;
        }
        break_courier_plastic_wrap(snap, acceptor_id, wrap_id, now)
    })
    .unwrap();
    assert_eq!(broken.0, corp_cour_cid);
    assert_eq!(broken.2, vec![item_courier]);

    // Non-existent wrap -> ItemNotFound; item not wrap -> InvalidPlasticWrap
    assert_eq!(
        ctx.db.try_with_mut_snapshot(|snap| break_courier_plastic_wrap(snap, acceptor_id, 999_777, now))
            .unwrap_err()
            .code,
        "ItemNotFound"
    );

    // 5. GetMyCurrentContractList (forCorp, foreign error), GetMyExpiredContractList, GetLoginInfo (needsAttention & inProgress)
    assert_eq!(
        handle(&ctx, &mut acceptor_sess, "contractProxy", "GetMyCurrentContractList", &[PyRep::Integer(99_999_888)], &kwargs)
            .unwrap()
            .unwrap_err()
            .code,
        "ContractAccessDenied"
    );
    assert!(handle(&ctx, &mut issuer_sess, "contractProxy", "GetMyCurrentContractList", &[PyRep::Bool(true)], &kwargs).unwrap().is_ok());
    assert!(handle(&ctx, &mut acceptor_sess, "contractProxy", "GetMyExpiredContractList", &[], &kwargs).unwrap().is_ok());
    // Seed an expired Outstanding and InProgress contract to hit GetLoginInfo branches
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(
            999_111,
            ContractRecord {
                contract_id: 999_111,
                issuer_id,
                issuer_corp_id: corp_id,
                assignee_id: acceptor_id,
                acceptor_id: None,
                contract_type: "Courier".into(),
                availability: "Private".into(),
                status: "Outstanding".into(),
                start_station_id: station_id,
                end_station_id: dest_station_id,
                expiration_unix: now.saturating_sub(5),
                ..Default::default()
            },
        );
        snap.contracts.insert(
            999_112,
            ContractRecord {
                contract_id: 999_112,
                issuer_id: third_id,
                issuer_corp_id: 0,
                assignee_id: 0,
                acceptor_id: Some(issuer_id),
                contract_type: "Courier".into(),
                availability: "Public".into(),
                status: "InProgress".into(),
                start_station_id: station_id,
                end_station_id: dest_station_id,
                expiration_unix: now + 3600,
                ..Default::default()
            },
        );
    });
    let login = handle(&ctx, &mut issuer_sess, "contractProxy", "GetLoginInfo", &[], &kwargs)
        .unwrap()
        .unwrap();
    assert!(login.dict_get("needsAttention").is_some());
    assert!(login.dict_get("inProgress").is_some());

    // Cover check_contract_visibility branches, is_item_in_asset_safety_wrap, RejectContract & DeleteContract errors/extra items,
    // SearchContracts kwargs (regionID, itemTypeID, minPrice, maxPrice), GetMyCurrentContractList forCorp, and GetMyBids owner/role/dismissed.
    let corp_id = 98_555_001_i64;
    let ally_id = 99_555_001_i64;
    let mut corp_sess = issuer_sess.clone();
    corp_sess.user_id = Some(2);
    corp_sess.character_id = Some(third_id);
    corp_sess.corporation_id = Some(corp_id);
    corp_sess.alliance_id = Some(ally_id);
    corp_sess.corp_role = 0;

    let wrap_ext = crate::services::station_structure::structure_upwell_ops::StructureExtendedState {
        asset_safety_wraps: [(
            777_001,
            crate::services::station_structure::structure_upwell_ops::AssetSafetyWrapRecord {
                wrap_id: 777_001,
                owner_id: issuer_id,
                is_corp: false,
                source_structure_id: station_id,
                source_structure_name: "Src".into(),
                solar_system_id: 30_000_142,
                created_unix: now,
                deliverable_after_unix: now,
                delivered_to_id: None,
                item_ids: vec![777_002],
                estimated_value_isk: 100.0,
                nearest_npc_station_id: Some(station_id),
                auto_delivered: false,
                retrieval_fee_paid: false,
            },
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    assert!(is_item_in_asset_safety_wrap(&wrap_ext, 777_002, station_id, 588));
    assert!(is_item_in_asset_safety_wrap(&wrap_ext, 777_001, station_id, 60));
    assert!(!is_item_in_asset_safety_wrap(&wrap_ext, 999_888, station_id, 588));

    let mut c_priv = ContractRecord {
        contract_id: 999_201,
        issuer_id,
        issuer_corp_id: corp_id,
        assignee_id: ally_id,
        availability: "Alliance".into(),
        contract_type: "ItemExchange".into(),
        status: "Outstanding".into(),
        price: IskCents(500_000_00),
        start_station_id: station_id,
        end_station_id: station_id,
        item_ids: vec![999_211],
        requested_items: vec![(34, 10)],
        expiration_unix: now + 3600,
        ..Default::default()
    };
    assert!(check_contract_visibility(&c_priv, third_id, corp_id, Some(ally_id)));
    c_priv.assignee_id = 0;
    c_priv.availability = "Corporation".into();
    assert!(check_contract_visibility(&c_priv, third_id, corp_id, None));
    c_priv.availability = "Alliance".into();
    assert!(!check_contract_visibility(&c_priv, third_id, corp_id, Some(ally_id)));
    c_priv.availability = "Personal".into();
    assert!(!check_contract_visibility(&c_priv, third_id, corp_id, None));

    // Seed corp auction + outstanding courier with deposit & extra item + bids
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.entry(corp_id).or_default();
        c.corporation_id = corp_id;
        c.ceo_id = issuer_id;
        c.members.insert(issuer_id);
        c.members.insert(third_id);
        if !snap.characters.contains_key(&third_id) {
            if let Some(mut base) = snap.characters.get(&issuer_id).cloned() {
                base.character_id = third_id;
                snap.characters.insert(third_id, base);
            }
        }
        if let Some(ch) = snap.characters.get_mut(&issuer_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(ally_id);
        }
        if let Some(ch) = snap.characters.get_mut(&third_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(ally_id);
        }
        snap.items.insert(999_211, InventoryItemRecord {
            item_id: 999_211, type_id: 588, owner_id: 999_201, location_id: 999_201,
            quantity: 1, ..Default::default()
        });
        snap.contracts.insert(999_201, ContractRecord {
            contract_id: 999_201,
            issuer_id: third_id,
            issuer_corp_id: corp_id,
            assignee_id: corp_id,
            for_corp: true,
            availability: "Private".into(),
            contract_type: "Courier".into(),
            status: "Outstanding".into(),
            price: IskCents(200_00),
            reward: IskCents(100_00),
            start_station_id: station_id,
            end_station_id: dest_station_id,
            item_ids: vec![999_211],
            expiration_unix: now + 3600,
            ..Default::default()
        });
        snap.fleet_and_social_ops.contract_deposits.insert(999_201, 50_000);
        snap.contracts.insert(999_202, ContractRecord {
            contract_id: 999_202,
            issuer_id,
            issuer_corp_id: corp_id,
            for_corp: true,
            availability: "Public".into(),
            contract_type: "Auction".into(),
            status: "Outstanding".into(),
            price: IskCents(10_000_00),
            current_bidder_id: Some(corp_id),
            bid_history: vec![(corp_id, IskCents(5_000_00), now)],
            start_station_id: station_id,
            end_station_id: station_id,
            item_ids: vec![999_212],
            requested_items: vec![(34, 5)],
            expiration_unix: now + 3600,
            ..Default::default()
        });
        snap.items.insert(999_212, InventoryItemRecord {
            item_id: 999_212, type_id: 34, owner_id: issuer_id, location_id: 999_202, quantity: 5, ..Default::default()
        });
        snap.contracts.insert(999_203, ContractRecord {
            contract_id: 999_203,
            issuer_id,
            issuer_corp_id: corp_id,
            for_corp: true,
            availability: "Public".into(),
            contract_type: "Auction".into(),
            status: "Finished".into(),
            acceptor_id: Some(corp_id),
            current_bidder_id: Some(corp_id),
            start_station_id: station_id,
            end_station_id: station_id,
            expiration_unix: now + 3600,
            ..Default::default()
        });
        snap.quasar_state_blobs.insert("contract_dismissed_notifs".into(), format!("[\"{corp_id}:999203\"]"));
    });

    // RejectContract without corp contract role -> CrpAccessDenied; DeleteContract wrong caller -> ContractOwnershipViolation
    assert_eq!(handle(&ctx, &mut corp_sess, "contractProxy", "RejectContract", &[PyRep::Integer(999_201)], &kwargs).unwrap().unwrap_err().code, "CrpAccessDenied");
    assert_eq!(handle(&ctx, &mut issuer_sess, "contractProxy", "RejectContract", &[PyRep::Integer(999_202)], &kwargs).unwrap().unwrap_err().code, "ContractOwnershipViolation");
    // Issuer CEO (has role) rejects 999_201 (hits deposit refund)
    let mut ceo_sess = issuer_sess.clone();
    ceo_sess.corporation_id = Some(corp_id);
    assert!(handle(&ctx, &mut ceo_sess, "contractProxy", "RejectContract", &[PyRep::Integer(999_201)], &kwargs).unwrap().is_ok());

    // DeleteContract on Auction with bids -> CannotDeleteAuctionWithBids; DeleteContract on 999_203 Finished -> InvalidContractState
    assert_eq!(handle(&ctx, &mut ceo_sess, "contractProxy", "DeleteContract", &[PyRep::Integer(999_202)], &kwargs).unwrap().unwrap_err().code, "CannotDeleteAuctionWithBids");
    assert_eq!(handle(&ctx, &mut ceo_sess, "contractProxy", "DeleteContract", &[PyRep::Integer(999_203)], &kwargs).unwrap().unwrap_err().code, "InvalidContractState");
    // DeleteContract on new contract with extra item in contract location
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.insert(999_204, ContractRecord {
            contract_id: 999_204, issuer_id, availability: "Public".into(), contract_type: "ItemExchange".into(),
            status: "Outstanding".into(), start_station_id: station_id, end_station_id: station_id, expiration_unix: now + 3600, ..Default::default()
        });
        snap.items.insert(999_214, InventoryItemRecord { item_id: 999_214, type_id: 34, owner_id: issuer_id, location_id: 999_204, quantity: 1, ..Default::default() });
    });
    assert!(handle(&ctx, &mut ceo_sess, "contractProxy", "DeleteContract", &[PyRep::Integer(999_204)], &kwargs).unwrap().is_ok());

    // SearchContracts with regionID, itemTypeID, minPrice, maxPrice, GetMyCurrentContractList forCorp, GetMyBids ownerID
    let reg_id = ctx.sde.stations.get(&station_id).and_then(|s| ctx.sde.solar_systems.get(&s.solar_system_id)).map(|sys| sys.region_id).unwrap_or(10_000_002);
    let mut skw = HashMap::new();
    skw.insert("regionID".into(), PyRep::Integer(i64::from(reg_id)));
    skw.insert("itemTypeID".into(), PyRep::Integer(34));
    skw.insert("minPrice".into(), PyRep::Real(1.0));
    skw.insert("maxPrice".into(), PyRep::Real(999_999.0));
    skw.insert("stationID".into(), PyRep::Integer(station_id));
    skw.insert("contractType".into(), PyRep::String("Auction".into()));
    assert!(matches!(handle(&ctx, &mut ceo_sess, "contractProxy", "SearchContracts", &[], &skw).unwrap().unwrap(), PyRep::List(_)));

    assert!(matches!(handle(&ctx, &mut ceo_sess, "contractProxy", "GetMyCurrentContractList", &[PyRep::Bool(true)], &kwargs).unwrap().unwrap(), PyRep::List(_)));
    assert_eq!(handle(&ctx, &mut ceo_sess, "contractProxy", "GetMyCurrentContractList", &[PyRep::Integer(99_999_888)], &kwargs).unwrap().unwrap_err().code, "ContractAccessDenied");
    assert_eq!(handle(&ctx, &mut corp_sess, "contractProxy", "GetMyBids", &[PyRep::Bool(false), PyRep::Integer(corp_id)], &kwargs).unwrap().unwrap_err().code, "CorpRoleDenied");
    assert!(matches!(handle(&ctx, &mut ceo_sess, "contractProxy", "GetMyBids", &[PyRep::Bool(false), PyRep::Integer(corp_id)], &kwargs).unwrap().unwrap(), PyRep::List(_)));
    assert!(matches!(handle(&ctx, &mut ceo_sess, "contractProxy", "GetMyBids", &[PyRep::Bool(true), PyRep::Integer(corp_id)], &kwargs).unwrap().unwrap(), PyRep::List(_)));
}

#[test]
fn issue_935_create_contract_accepts_object_ex_contract_struct_from_wizard() {
    let (ctx, mut sess, issuer_id, _acc_sess, assignee_id, station_id, dest_station_id) =
        setup_contracts_ctx();
    let courier_item_id = 935_001_i64;
    let exchange_item_id = 935_002_i64;
    let kwargs = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        for iid in [courier_item_id, exchange_item_id] {
            snap.items.insert(
                iid,
                make_fighter_item(iid, 34, issuer_id, station_id, InventoryFlag::Hangar, 50),
            );
        }
    });

    // Build V24 new-style `ContractStruct(object)` serialized as Op_PyObjectEx1 (`header = (cls, (), state_dict)`)
    let courier_struct = PyRep::ObjectEx {
        header: Box::new(PyRep::Tuple(vec![
            PyRep::String("eve.client.script.ui.shared.neocom.contracts.createContractWnd.ContractStruct".into()),
            PyRep::Tuple(vec![]),
            PyRep::Dict(vec![
                (PyRep::String("type".into()), PyRep::Integer(3)), // Courier
                (PyRep::String("availability".into()), PyRep::None),
                (PyRep::String("isPrivate".into()), PyRep::Bool(true)),
                (PyRep::String("assigneeID".into()), PyRep::None),
                (PyRep::String("assignedToID".into()), PyRep::Integer(assignee_id)),
                (PyRep::String("expireTime".into()), PyRep::None),
                (PyRep::String("minutesExpire".into()), PyRep::Integer(2880)), // 2 days
                (PyRep::String("numDays".into()), PyRep::Integer(3)),
                (PyRep::String("startStationID".into()), PyRep::Integer(station_id)),
                (PyRep::String("endStationID".into()), PyRep::None),
                (PyRep::String("destinationID".into()), PyRep::Integer(dest_station_id)),
                (PyRep::String("reward".into()), PyRep::Real(250_000.0)),
                (PyRep::String("collateral".into()), PyRep::Real(500_000.0)),
                (PyRep::String("title".into()), PyRep::None),
                (PyRep::String("description".into()), PyRep::String("Wizard Courier".into())),
                (PyRep::String("forCorp".into()), PyRep::Integer(0)),
                (
                    PyRep::String("itemList".into()),
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(courier_item_id),
                        PyRep::Integer(50),
                    ])]),
                ),
            ]),
        ])),
        list: vec![],
        dict: vec![],
        is_type_2: false,
    };

    let created = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "CreateContract",
        &[courier_struct],
        &kwargs,
    )
    .expect("dispatch CreateContract")
    .expect("CreateContract with ObjectEx ContractStruct succeeds");

    let contract_id = dict_i64(&created, "contractID").expect("contractID returned");
    let rec = ctx.db.with_snapshot(|s| s.contracts.get(&contract_id).cloned()).expect("contract persisted");
    assert_eq!(rec.contract_type, "Courier");
    assert_eq!(rec.availability, "Personal");
    assert_eq!(rec.assignee_id, assignee_id);
    assert_eq!(rec.start_station_id, station_id);
    assert_eq!(rec.end_station_id, dest_station_id);
    assert_eq!(rec.title, "Wizard Courier");
    assert_eq!(rec.num_days, 3);
    assert!(!rec.for_corp);

    // Also test ItemExchange with `requestItemTypeList` inside `ObjectEx`
    let exchange_struct = PyRep::ObjectEx {
        header: Box::new(PyRep::Tuple(vec![
            PyRep::String("ContractStruct".into()),
            PyRep::Tuple(vec![]),
            PyRep::Dict(vec![
                (PyRep::String("type".into()), PyRep::Integer(1)), // ItemExchange
                (PyRep::String("isPrivate".into()), PyRep::Integer(0)),
                (PyRep::String("minutesExpire".into()), PyRep::Integer(1440)),
                (PyRep::String("startStationID".into()), PyRep::Integer(station_id)),
                (PyRep::String("price".into()), PyRep::Real(10_000.0)),
                (
                    PyRep::String("itemList".into()),
                    PyRep::List(vec![PyRep::keyval([("itemID", PyRep::Integer(exchange_item_id))])]),
                ),
                (
                    PyRep::String("requestItemTypeList".into()),
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(35),
                        PyRep::Integer(100),
                    ])]),
                ),
            ]),
        ])),
        list: vec![],
        dict: vec![],
        is_type_2: false,
    };
    let ex_created = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "CreateContract",
        &[exchange_struct],
        &kwargs,
    )
    .unwrap()
    .expect("ItemExchange ObjectEx succeeds");
    let ex_cid = dict_i64(&ex_created, "contractID").unwrap();
    let ex_rec = ctx.db.with_snapshot(|s| s.contracts[&ex_cid].clone());
    assert_eq!(ex_rec.contract_type, "ItemExchange");
    assert_eq!(ex_rec.requested_items, vec![(35, 100)]);
}

