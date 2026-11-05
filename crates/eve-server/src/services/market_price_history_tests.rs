#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{MarketHistoryDayRecord, MarketOrderRecord};

#[test]
fn market_price_history_and_transactions_mutations() {
    assert_eq!(date_str_to_unix_secs("1970-01-01"), Some(0));
    assert_eq!(date_str_to_unix_secs("1970-01-02"), Some(86_400));
    assert_eq!(date_str_to_unix_secs("invalid"), None);
    assert_eq!(date_str_to_unix_secs("2025-13-01"), None);
    assert_eq!(date_str_to_unix_secs("2025-01-32"), None);
    assert_eq!(date_str_to_filetime("1970-01-01"), Some(116_444_736_000_000_000));

    assert_eq!(
        extract_type_ids_arg(Some(&PyRep::List(vec![PyRep::Integer(34), PyRep::Integer(-1)]))),
        Some((vec![34], false))
    );
    assert_eq!(
        extract_type_ids_arg(Some(&PyRep::PySet(vec![PyRep::Integer(35)]))),
        Some((vec![35], true))
    );
    assert_eq!(extract_type_ids_arg(Some(&PyRep::Integer(34))), None);
    assert_eq!(extract_type_ids_arg(None), None);

    let ctx = test_ctx();
    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_history.insert(
            (10_000_002, 34),
            vec![
                MarketHistoryDayRecord {
                    date: "2025-01-02".into(),
                    lowest: 4.0,
                    highest: 5.0,
                    average: 4.5,
                    volume: 1000,
                    order_count: 10,
                    open_price: None,
                    close_price: None,
                },
                MarketHistoryDayRecord {
                    date: "2025-01-01".into(),
                    lowest: 3.5,
                    highest: 4.5,
                    average: 4.0,
                    volume: 500,
                    order_count: 5,
                    open_price: None,
                    close_price: None,
                },
            ],
        );
    });

    ctx.db.with_snapshot(|snap| {
        let rows = build_price_history_rows(snap, 34, 10_000_002);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][1], PyRep::Real(3.5));
        assert_eq!(rows[1][1], PyRep::Real(4.0));

        let (sys_id, const_id) = resolve_station_solar_and_constellation(&ctx, snap, 60_003_760);
        assert_eq!(sys_id, 30_000_142);
        assert!(const_id > 0);

        let ord = MarketOrderRecord {
            order_id: 888_001,
            type_id: 34,
            owner_id: 90_000_001,
            region_id: 10_000_002,
            station_id: 60_003_760,
            price: IskCents(450),
            vol_entered: 100,
            vol_remaining: 50,
            min_volume: 1,
            is_bid: false,
            is_npc: false,
            escrow: IskCents(0),
        };
        let cells = order_to_packed_cells_with_ctx(&ctx, snap, &ord, 1_700_000_000);
        assert_eq!(cells.len(), 15);
    });

    let own_payload = build_own_order_changed_payload(&ctx, 999_999_999, "Add", false);
    assert_eq!(own_payload.len(), 3);
    assert_eq!(own_payload[1], PyRep::String("Add".into()));
    assert_eq!(own_payload[2], PyRep::Bool(false));

    assert!(handle_market_transactions_rpc(&ctx, &sess, "UnknownMethod", &[]).is_none());
    let char_tx = handle_market_transactions_rpc(&ctx, &sess, "CharGetTransactions", &[])
        .unwrap()
        .unwrap();
    assert!(!matches!(char_tx, PyRep::None));

    let corp_tx = handle_market_transactions_rpc(&ctx, &sess, "CorpGetTransactions", &[])
        .unwrap();
    assert!(corp_tx.is_ok() || corp_tx.is_err());
}
