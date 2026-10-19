//! `market_contracts` — Domain 7: Regional Market Order Book, Matching Engine, Wallet/PLEX, & Contracts (`§3.7`).

#![forbid(unsafe_code)]

use crate::formulas::compute_sales_tax_rate;
use crate::router::ServerContext;
use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// Computes exact integer sales tax in ISK cents (`i128` parts-per-million rounded to nearest cent)
/// from the seller's own Accounting (`16622`) skill level (`R21-P1-4`).
pub fn compute_sales_tax_cents(fill_gross: IskCents, seller_acct_lvl: u8) -> IskCents {
    let tax_rate = compute_sales_tax_rate(seller_acct_lvl);
    let tax_ppm = (tax_rate * 1_000_000.0).round() as i128;
    IskCents((((fill_gross.0 as i128) * tax_ppm + 500_000) / 1_000_000) as i64)
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service == "marketProxy" {
        return super::market_trading::handle(ctx, session, service, method, args, _kwargs);
    }
    if service == "contractProxy" || service == "contractMgr" {
        return super::contracts_service::handle(ctx, session, service, method, args, _kwargs);
    }

    match (service, method) {
        ("account", "GetCashBalance") | ("vaultManager", "get_account_balance") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let bal = ctx
                .db
                .get_character(char_id)
                .map(|c| c.balance.to_wire_f64())
                .unwrap_or(0.0);
            Some(Ok(PyRep::Real(bal)))
        }

        ("account", "GetJournal") | ("account", "GetTransactions") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.wallet_journal
                    .iter()
                    .filter(|j| j.character_id == char_id)
                    .map(|j| {
                        vec![
                            PyRep::Integer(j.entry_id),
                            PyRep::String(j.entry_type.clone()),
                            PyRep::Real(j.amount.to_wire_f64()),
                            PyRep::Real(j.balance_after.to_wire_f64()),
                            PyRep::String(j.reason.clone()),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["entryID", "entryType", "amount", "balance", "reason"],
                rows,
            )))
        }

        _ => None,
    }
}
