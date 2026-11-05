use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item,
};
use eve_db::{CorporationRecord, InventoryFlag, IskCents, KillmailRecord, MarketOrderRecord};
use std::collections::HashMap;

fn dict_i64(r: &PyRep, k: &str) -> Option<i64> {
    r.dict_get(k).and_then(|v| v.as_i64())
}

fn setup_corp_registry_ctx() -> (ServerContext, SessionState, i64, i64, i64) {
    let (ctx, mut sess, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let corp_id = 98_000_010_i64;
    let station_id = 60_003_760_i64;
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.corp_role = u64::MAX;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
        }
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "Apex Frontier Syndicate".into(),
            ticker: "APEX".into(),
            ceo_id: char_id,
            alliance_id: None,
            tax_rate: 0.05,
            master_wallet: IskCents::from_isk(100_000_000),
            members: vec![char_id].into_iter().collect(),
            member_roles: [(char_id, u64::MAX)].into_iter().collect(),
            hq_station_id: Some(station_id),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);
    });
    let _ = sys_id;
    (ctx, sess, char_id, corp_id, station_id)
}

#[tokio::test]
async fn test_handle_corp_hangar_invbroker_list_and_add() {
    let (ctx, mut sess, char_id, corp_id, station_id) = setup_corp_registry_ctx();
    let kwargs = HashMap::new();
    let corp_item_id = 910_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            corp_item_id,
            make_fighter_item(
                corp_item_id,
                34,
                corp_id,
                station_id,
                InventoryFlag::CorpSAG1,
                500,
            ),
        );
    });

    // CEO can query division 1 (flag 116)
    let listed = handle_corp_hangar_invbroker(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "List",
        &[PyRep::Integer(116)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(listed, PyRep::ObjectEx { .. }));

    // Non-CEO without role is denied on List and Add
    let member_id = 90_000_077_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.members.insert(member_id);
            c.member_roles.insert(member_id, 0);
        }
    });
    let mut member_sess = sess.clone();
    member_sess.character_id = Some(member_id);
    member_sess.corp_role = 0;

    let denied_list = handle_corp_hangar_invbroker(
        &ctx,
        &mut member_sess,
        None,
        "invbroker",
        "List",
        &[PyRep::Integer(116)],
        &kwargs,
    )
    .await
    .unwrap();
    assert!(denied_list.is_err());

    let denied_add = handle_corp_hangar_invbroker(
        &ctx,
        &mut member_sess,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(corp_item_id)],
        &kwargs,
    )
    .await
    .unwrap();
    assert!(denied_add.is_err());
    let _ = char_id;
}

#[test]
fn test_corp_registry_handle_methods_and_helpers() {
    let (ctx, mut sess, char_id, corp_id, station_id) = setup_corp_registry_ctx();
    let kwargs = HashMap::new();

    // Seed a killmail and a corporate market order
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            501,
            KillmailRecord {
                kill_id: 501,
                solar_system_id: 30_000_142,
                killed_unix: 1_700_000_000,
                victim_character_id: 90_000_099,
                victim_corp_id: corp_id,
                victim_alliance_id: None,
                ship_type_id: 600,
                attacker_character_id: char_id,
                attacker_corp_id: corp_id,
                attacker_alliance_id: None,
                total_value_isk: 10_000_000.0,
            },
        );
        snap.market_orders.insert(
            701,
            MarketOrderRecord {
                order_id: 701,
                type_id: 34,
                owner_id: corp_id,
                station_id,
                region_id: 10_000_002,
                price: IskCents::from_isk(5),
                vol_entered: 1000,
                vol_remaining: 1000,
                min_volume: 1,
                is_bid: false,
                is_npc: false,
                escrow: IskCents::ZERO,
            },
        );
    });

    // GetEveOwners
    let owners = handle(&ctx, &mut sess, "corpRegistry", "GetEveOwners", &[], &kwargs)
        .unwrap()
        .unwrap();
    assert!(!matches!(owners, PyRep::None));

    // GetRecentKills & GetRecentLosses
    let kills = handle(&ctx, &mut sess, "corpRegistry", "GetRecentKills", &[], &kwargs)
        .unwrap()
        .unwrap();
    assert!(!matches!(kills, PyRep::None));
    let losses = handle(&ctx, &mut sess, "corpRegistry", "GetRecentLosses", &[], &kwargs)
        .unwrap()
        .unwrap();
    assert!(!matches!(losses, PyRep::None));

    // GetSuggestedAllianceShortNames & GetSuggestedTickerNames
    let short_names = handle(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetSuggestedAllianceShortNames",
        &[PyRep::String("Northern Coalition Dot".into())],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(short_names, PyRep::List(ref l) if !l.is_empty()));

    let tickers = handle(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetSuggestedTickerNames",
        &[PyRep::String("Stellar Industrial Logistics".into())],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(tickers, PyRep::List(ref l) if !l.is_empty()));

    // SetAccountKey
    let set_key = handle(
        &ctx,
        &mut sess,
        "corpRegistry",
        "SetAccountKey",
        &[PyRep::Integer(1002)],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    assert_eq!(set_key, PyRep::None);
    assert_eq!(sess.corp_account_key, 1002);

    // ExecuteActions (batch role update & legacy tuple format)
    let batch_exec = handle(
        &ctx,
        &mut sess,
        "corpRegistry",
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(char_id)]),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("add".into()),
                PyRep::String("roles".into()),
                PyRep::Integer(128),
            ])]),
        ],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    assert_eq!(batch_exec, PyRep::Bool(true));

    // Bind & UnBind
    let bound = handle(&ctx, &mut sess, "corpRegistry", "Bind", &[PyRep::Integer(corp_id)], &kwargs)
        .unwrap()
        .unwrap();
    let bid = bound.as_i64().unwrap();
    let unbound = handle(&ctx, &mut sess, "corpRegistry", "UnBind", &[PyRep::Integer(bid)], &kwargs)
        .unwrap()
        .unwrap();
    assert_eq!(unbound, PyRep::None);

    // Supplemental helpers
    let mkt = handle_get_corp_market_info(&ctx, &mut sess, &[PyRep::Integer(corp_id)])
        .unwrap()
        .unwrap();
    assert!(!matches!(mkt, PyRep::None));

    let info_win = handle_get_info_window_data_for_char(&ctx, &mut sess, &[PyRep::Integer(char_id)])
        .unwrap()
        .unwrap();
    assert_eq!(dict_i64(&info_win, "corporationID"), Some(corp_id));

    let member = handle_get_member(&ctx, &mut sess, &[PyRep::Integer(char_id)])
        .unwrap()
        .unwrap();
    assert_eq!(dict_i64(&member, "characterID"), Some(char_id));

    let tracking =
        handle_get_member_tracking_info(&ctx, &mut sess, "GetMemberTrackingInfo", &[])
            .unwrap()
            .unwrap();
    assert!(!matches!(tracking, PyRep::None));
}
