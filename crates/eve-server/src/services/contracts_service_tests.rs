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
