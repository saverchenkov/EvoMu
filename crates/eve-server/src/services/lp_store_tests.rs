use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item,
};
use eve_db::{corp_roles, CorporationRecord, InventoryFlag, IskCents};
use eve_sde::NpcCorporationInfo;
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn test_lp_store_reward_log_and_balances_and_transfers() {
    let (mut ctx, mut session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let station_id = 60_003_760_i64;
    let player_corp = 98_000_001_i64;
    let dest_corp = 98_000_002_i64;
    let npc_corp = 1_000_035_i64;
    session.station_id = Some(station_id);
    session.corporation_id = Some(player_corp);
    session.corp_role =
        corp_roles::ROLE_BRAND_MANAGER | corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_DIRECTOR;

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut.npc_corporations.insert(
        npc_corp,
        NpcCorporationInfo {
            corporation_id: npc_corp,
            name: "Caldari Navy".into(),
            ticker_name: "CN".into(),
            faction_id: Some(500_001),
            solar_system_id: Some(sys_id),
            station_id: Some(station_id),
            tax_rate: 0.0,
        },
    );

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = player_corp;
            ch.station_id = Some(station_id);
            ch.loyalty_points.insert(CONCORD_CORP_ID, 10_000);
            ch.loyalty_points.insert(npc_corp, 5_000);
        }
        snap.corporations.insert(
            player_corp,
            CorporationRecord {
                corporation_id: player_corp,
                name: "Player Corp".into(),
                ticker: "PLRC".into(),
                ceo_id: char_id,
                ..Default::default()
            },
        );
        snap.corporations.insert(
            dest_corp,
            CorporationRecord {
                corporation_id: dest_corp,
                name: "Dest Corp".into(),
                ticker: "DSTC".into(),
                ceo_id: char_id,
                ..Default::default()
            },
        );
        snap.corp_lp_balances.insert((player_corp, npc_corp), 8_000);
        record_lp_reward_log(snap, char_id, npc_corp, 5_000, "Mission completion".into(), now);
    });
    let _ = sys_id;

    // GetAllMyCharacterWalletLPBalances
    let char_bals = handle(
        &ctx,
        &mut session,
        "LPSvc",
        "GetAllMyCharacterWalletLPBalances",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(char_bals, PyRep::List(ref items) if items.len() == 2));

    // GetAllMyCorporationWalletLPBalances
    let corp_bals = handle(
        &ctx,
        &mut session,
        "LPSvc",
        "GetAllMyCorporationWalletLPBalances",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(corp_bals, PyRep::List(ref items) if !items.is_empty()));

    // ExchangeConcordLP
    let ex_err = handle(
        &ctx,
        &mut session,
        "LPSvc",
        "ExchangeConcordLP",
        &[PyRep::Integer(npc_corp), PyRep::Integer(0)],
        &kw,
    )
    .unwrap();
    assert!(ex_err.is_err());

    let ex_ok = handle(
        &ctx,
        &mut session,
        "LPSvc",
        "ExchangeConcordLP",
        &[PyRep::Integer(npc_corp), PyRep::Integer(1_000)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(ex_ok.dict_get("convertedLP").and_then(|v| v.as_i64()), Some(800));

    // TransferLPFromMyWalletToOtherCorp
    let xfer_char = handle(
        &ctx,
        &mut session,
        "LPSvc",
        "TransferLPFromMyWalletToOtherCorp",
        &[
            PyRep::Integer(dest_corp),
            PyRep::Integer(npc_corp),
            PyRep::Integer(1_500),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(xfer_char.dict_get("transferredLP").and_then(|v| v.as_i64()), Some(1_500));

    // TransferLPFromMyCorpWalletToOtherCorp
    let xfer_corp = handle(
        &ctx,
        &mut session,
        "LPSvc",
        "TransferLPFromMyCorpWalletToOtherCorp",
        &[
            PyRep::Integer(dest_corp),
            PyRep::Integer(npc_corp),
            PyRep::Integer(2_000),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(xfer_corp.dict_get("transferredLP").and_then(|v| v.as_i64()), Some(2_000));

    // GetRewardLPLogs
    let logs = handle(&ctx, &mut session, "rewardMgr", "GetRewardLPLogs", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(logs, PyRep::List(ref items) if !items.is_empty()));
}

#[test]
fn test_lp_store_offers_and_take_offer_char_and_corp() {
    let (ctx, mut session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let station_id = 60_003_760_i64;
    let player_corp = 98_000_001_i64;
    let npc_corp = 1_000_035_i64;
    session.station_id = Some(station_id);
    session.corporation_id = Some(player_corp);
    session.corp_role =
        corp_roles::ROLE_BRAND_MANAGER | corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_DIRECTOR;
    let _ = sys_id;

    ctx.db.with_mut_snapshot(|snap| {
        register_lp_store_offer(
            snap,
            LPOffer {
                offer_id: 777_001,
                corp_id: npc_corp,
                type_id: 34,
                quantity: 100,
                lp_cost: 500,
                isk_cost: IskCents::from_isk(1_000),
                ak_cost: 0,
                req_items: vec![(35, 10)],
                min_standing: Some(1.0),
            },
        );
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = player_corp;
            c.station_id = Some(station_id);
            c.balance = IskCents::from_isk(1_000_000);
            c.loyalty_points.insert(npc_corp, 5_000);
            c.standings.insert(npc_corp, 5.0);
        }
        let mut corp = CorporationRecord {
            corporation_id: player_corp,
            name: "Player Corp".into(),
            ticker: "PLRC".into(),
            ceo_id: char_id,
            master_wallet: IskCents::from_isk(1_000_000),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(player_corp, corp);
        snap.corp_lp_balances.insert((player_corp, npc_corp), 5_000);
        snap.items.insert(
            900_001,
            make_fighter_item(900_001, 35, char_id, station_id, InventoryFlag::Hangar, 50),
        );
        snap.items.insert(
            900_002,
            make_fighter_item(900_002, 35, player_corp, station_id, InventoryFlag::CorpDeliveries, 50),
        );
    });

    // GetAvailableOffersFromCorp
    let offers = handle(
        &ctx,
        &mut session,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(npc_corp)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(offers, PyRep::List(ref list) if !list.is_empty()));

    // TakeOfferForCharacter
    let take_char = handle(
        &ctx,
        &mut session,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(npc_corp),
            PyRep::Integer(777_001),
            PyRep::Integer(2),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(take_char.dict_get("deliveredItemID").is_some());

    // TakeOfferForCorporation
    let take_corp = handle(
        &ctx,
        &mut session,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[
            PyRep::Integer(npc_corp),
            PyRep::Integer(777_001),
            PyRep::Integer(1),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(take_corp.dict_get("deliveredItemID").is_some());

    // GetDelayedRewardsByGroupIDs
    let delayed = handle(
        &ctx,
        &mut session,
        "rewardMgr",
        "GetDelayedRewardsByGroupIDs",
        &[PyRep::List(vec![PyRep::Integer(npc_corp), PyRep::Integer(20)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(delayed, PyRep::Dict(_)));
}
