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
    sess.corp_account_key = 1000;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
        }
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Apex Frontier Syndicate".into();
            __corp.ticker = "APEX".into();
            __corp.ceo_id = char_id;
            __corp.alliance_id = None;
            __corp.tax_rate = 0.05;
            __corp.members = vec![char_id].into_iter().collect();
            __corp.member_roles = [(char_id, u64::MAX)].into_iter().collect();
            __corp.hq_station_id = Some(station_id);
            __corp.set_division_balance(1, IskCents::from_isk(100_000_000));
            __corp };
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
    let PyRep::Rowset { columns: t_cols, rows: t_rows } = tracking else {
        panic!("expected Rowset from GetMemberTrackingInfo");
    };
    let idx = |name: &str| t_cols.iter().position(|c| c == name).expect(name);
    assert_eq!(t_rows.len(), 1);
    assert_eq!(t_rows[0][idx("lastOnline")].as_f64(), Some(-1.0));
    assert_eq!(t_rows[0][idx("online")].as_bool(), Some(true));
    assert_eq!(t_rows[0][idx("factionID")], PyRep::None);
    assert!(t_rows[0][idx("startDateTime")].as_i64().unwrap_or(0) >= 116_444_736_000_000_000);
    assert!(t_rows[0][idx("logonDateTime")].as_i64().unwrap_or(0) >= 116_444_736_000_000_000);

    // Offline member with prior login/logout and never-logged-in member
    let off_char_id = 90_000_002_i64;
    let never_char_id = 90_000_003_i64;
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(off_char_id);
            corp.members.insert(never_char_id);
        }
        if let Some(ch2) = snap.characters.get_mut(&off_char_id) {
            ch2.corporation_id = corp_id;
        }
        if let Some(ch3) = snap.characters.get_mut(&never_char_id) {
            ch3.corporation_id = corp_id;
        } else if let Some(mut ch3) = snap.characters.get(&char_id).cloned() {
            ch3.character_id = never_char_id;
            ch3.user_id = never_char_id as u64;
            ch3.name = "Never Logged In".into();
            ch3.corporation_id = corp_id;
            snap.characters.insert(never_char_id, ch3);
        }
        snap.character_login_stats.remove(&never_char_id);
        snap.character_login_stats.insert(
            off_char_id,
            eve_db::CharacterLoginStats {
                last_login_unix: now_u.saturating_sub(7200),
                last_logout_unix: now_u.saturating_sub(3600),
                ..Default::default()
            },
        );
    });
    let tracking_all =
        handle_get_member_tracking_info(&ctx, &mut sess, "GetMemberTrackingInfo", &[])
            .unwrap()
            .unwrap();
    let PyRep::Rowset { rows: all_rows, .. } = tracking_all else {
        panic!("expected Rowset");
    };
    let off_row = all_rows.iter().find(|r| r[0].as_i64() == Some(off_char_id)).unwrap();
    let off_hours = off_row[idx("lastOnline")].as_f64().unwrap();
    assert!((off_hours - 1.0).abs() < 0.01, "expected ~1.0 hours offline, got {off_hours}");
    assert!(off_row[idx("logoffDateTime")].as_i64().unwrap_or(0) >= 116_444_736_000_000_000);
    let never_row = all_rows.iter().find(|r| r[0].as_i64() == Some(never_char_id)).unwrap();
    assert_eq!(never_row[idx("lastOnline")], PyRep::None);
}

#[test]
fn set_account_key_noop_skips_session_change_timer() {
    let (ctx, mut sess, _char_id, _corp_id, _station_id) = setup_corp_registry_ctx();
    let kwargs = HashMap::new();

    // No-op SetAccountKey(1000) returns None without starting the 10s timer.
    let res1 = handle(&ctx, &mut sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1000)], &kwargs)
        .unwrap()
        .unwrap();
    assert_eq!(res1, PyRep::None);
    assert_eq!(sess.corp_account_key, 1000);

    // Immediate actual change 1000 -> 1001 succeeds without SessionChangeInProgress.
    let res2 = handle(&ctx, &mut sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1001)], &kwargs)
        .unwrap()
        .unwrap();
    assert_eq!(res2, PyRep::None);
    assert_eq!(sess.corp_account_key, 1001);

    // No-op repeat of current key (1001) during active timer also succeeds without triggering cooldown.
    let res3 = handle(&ctx, &mut sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1001)], &kwargs)
        .unwrap()
        .unwrap();
    assert_eq!(res3, PyRep::None);

    // Changing to 1002 within 10s fails with SessionChangeInProgress.
    let err = handle(&ctx, &mut sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1002)], &kwargs)
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "SessionChangeInProgress");
}
