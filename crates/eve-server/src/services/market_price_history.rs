//! `market_price_history` — Regional daily OHLC market price history query builders.
//!
//! Formats daily aggregates from `snap.market_history` into the canonical client
//! rowset (`historyDate, lowPrice, highPrice, avgPrice, volume, orders`) expected
//! by `marketProxy.GetOldPriceHistory`, `GetNewPriceHistory`, and `GetHistoryForManyTypeIDs`.

use eve_db::{TypeId, WorldStateSnapshot};
use eve_proto::PyRep;

/// Converts a "YYYY-MM-DD" date string to Unix epoch seconds (midnight UTC).
pub fn date_str_to_unix_secs(date_str: &str) -> Option<u64> {
    let mut parts = date_str.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe as i64 - 719_468;
    if days < 0 {
        return None;
    }
    Some((days as u64) * 86_400)
}

/// Converts a "YYYY-MM-DD" date string to Windows FileTime (100-nanosecond intervals since 1601-01-01).
pub fn date_str_to_filetime(date_str: &str) -> Option<i64> {
    let unix_secs = date_str_to_unix_secs(date_str)?;
    Some((unix_secs as i64 + 11_644_473_600) * 10_000_000)
}

/// Builds price history rows for a given `(region_id, type_id)` from persisted trade history.
/// Returns an empty vector if no trades exist for this type in the region.
pub fn build_price_history_rows(
    snap: &WorldStateSnapshot,
    type_id: TypeId,
    region_id: u32,
) -> Vec<Vec<PyRep>> {
    let mut rows = Vec::new();
    if let Some(entries) = snap.market_history.get(&(region_id, type_id)) {
        let mut sorted = entries.clone();
        sorted.sort_by(|a, b| a.date.cmp(&b.date));
        for day in sorted {
            let file_time = date_str_to_filetime(&day.date).unwrap_or(0);
            rows.push(vec![
                PyRep::Integer(file_time),
                PyRep::Real(day.lowest),
                PyRep::Real(day.highest),
                PyRep::Real(day.average),
                PyRep::Integer(day.volume),
                PyRep::Integer(day.order_count),
            ]);
        }
    } else {
        // Fallback: check if market_transactions have fills for this type in this region
        let mut daily_map: std::collections::BTreeMap<String, (f64, f64, i64, i64, f64)> = std::collections::BTreeMap::new();
        for tx in &snap.market_transactions {
            if tx.type_id == type_id {
                let tx_region = snap
                    .market_orders
                    .values()
                    .find(|o| o.station_id == tx.location_id)
                    .map(|o| o.region_id)
                    .unwrap_or(10_000_002);
                if tx_region == region_id {
                    let date_str = eve_db::ledger::unix_secs_to_date_str(tx.date_unix);
                    let unit_price = tx.price.to_wire_f64();
                    let qty = tx.quantity.max(1);
                    let entry = daily_map.entry(date_str).or_insert((unit_price, unit_price, 0, 0, 0.0));
                    entry.0 = entry.0.min(unit_price);
                    entry.1 = entry.1.max(unit_price);
                    entry.2 += qty;
                    entry.3 += 1;
                    entry.4 += unit_price * (qty as f64);
                }
            }
        }
        for (date_str, (low, high, vol, orders, total_val)) in daily_map {
            let avg = if vol > 0 { total_val / (vol as f64) } else { (low + high) / 2.0 };
            let file_time = date_str_to_filetime(&date_str).unwrap_or(0);
            rows.push(vec![
                PyRep::Integer(file_time),
                PyRep::Real(low),
                PyRep::Real(high),
                PyRep::Real(avg),
                PyRep::Integer(vol),
                PyRep::Integer(orders),
            ]);
        }
    }
    rows
}
