//! `wallet_ledger` — corporation wallet transfers, the wallet journal (`account.GetTransactions`),
//! market transaction history (`marketProxy.CharGetTransactions` / `CorpGetTransactions`) and PLEX
//! Vault deposits (`invbroker.DepositPlexToVault`).
//!
//! Call shapes follow the V24 client bytecode:
//! - `account.GiveCashFromCorpAccount(toID, amount, fromAccountKey, toAccountKey=..., reason=...)`
//!   (`eve/client/script/ui/shared/neocom/wallet/transferMoneyWnd.pyj`)
//! - `account.GetTransactions(accountKey, year, month, isCorp)` (`ui/services/accountsvc.pyj`)
//! - `marketProxy.CharGetTransactions(fromDate)`, `CorpGetTransactions(fromDate, accountKey)`
//!   (`ui/shared/marketSvc.pyj`)
//! - `invbroker.DepositPlexToVault(locationID, itemID, quantity, invoiceType)`
//!   (`ui/shared/inventory/plexVault.pyj`)

#![forbid(unsafe_code)]

use super::market_contracts::PLEX_TYPE_ID;
use crate::router::ServerContext;
use eve_db::corp_roles;
use eve_db::ledger::{ref_types, JournalContext, JournalEntryInput, JournalMeta, ACCOUNTING_KEY_CASH};
use eve_db::{InventoryFlag, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}

/// Windows FILETIME (100 ns ticks since 1601-01-01) for a unix timestamp.
fn filetime(unix: u64) -> i64 {
    (unix as i64 + 11_644_473_600) * 10_000_000
}

fn unix_from_filetime(ft: i64) -> u64 {
    (ft / 10_000_000 - 11_644_473_600).max(0) as u64
}

/// (year, month) of a unix timestamp in UTC.
fn year_month(unix: u64) -> (i64, i64) {
    // Days since 1970-01-01 -> civil date (Howard Hinnant's algorithm).
    let z = (unix / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m)
}

/// Divisions 1..=7 map to account keys 1000..=1006 (`appConst.accountingKeyCash..Cash7`).
fn division_of(account_key: i64) -> Option<u8> {
    (1000..=1006).contains(&account_key).then(|| (account_key - 999) as u8)
}

fn kw_or_arg<'a>(args: &'a [PyRep], idx: usize, kwargs: &'a HashMap<String, PyRep>, key: &str) -> Option<&'a PyRep> {
    kwargs.get(key).or_else(|| args.get(idx)).filter(|v| !matches!(v, PyRep::None))
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("account", "GiveCashFromCorpAccount") => {
            if args.len() == 3 && matches!(args.get(2), Some(PyRep::Real(_))) && matches!(args.get(0), Some(PyRep::Integer(1..=7))) {
                None
            } else {
                Some(give_cash_from_corp(ctx, session, args, kwargs))
            }
        }
        ("account", "GetTransactions") => Some(get_journal(ctx, session, args)),
        ("marketProxy", "CharGetTransactions") => Some(market_transactions(ctx, session, args, false)),
        ("marketProxy", "CorpGetTransactions") => Some(market_transactions(ctx, session, args, true)),
        ("invbroker", "DepositPlexToVault") => Some(deposit_plex(ctx, session, args)),
        _ => None,
    }
}

fn give_cash_from_corp(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let to_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| rpc_err("InvalidArgument", "toID required"))?;
    let amount_isk = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
    let from_key = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
    let to_key = kw_or_arg(args, 3, kwargs, "toAccountKey").and_then(|v| v.as_i64()).filter(|k| *k != 0);
    let reason = kw_or_arg(args, 4, kwargs, "reason").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !amount_isk.is_finite() || amount_isk <= 0.0 {
        return Err(rpc_err("InvalidArgument", "Transfer amount must be positive and finite"));
    }
    let from_div = division_of(from_key)
        .ok_or_else(|| rpc_err("InvalidAccountKey", format!("fromAccountKey {from_key} is not a corp wallet division")))?;
    let amount = IskCents::from_f64_isk(amount_isk);
    let now = ctx.now_unix();
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .ok_or_else(|| rpc_err("CharacterNotFound", "character not found"))?;
        let corp = snap
            .corporations
            .get_mut(&corp_id)
            .ok_or_else(|| rpc_err("CorporationNotFound", format!("Corporation {corp_id} not found")))?;
        let role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
        let take_role = corp_roles::ROLE_ACCOUNT_CAN_TAKE[(from_div - 1) as usize];
        if char_id != corp.ceo_id
            && !corp_roles::has_role(role, corp_roles::ROLE_ACCOUNTANT)
            && !corp_roles::has_role(role, take_role)
        {
            return Err(rpc_err(
                "CrpAccessDenied",
                format!("Withdrawing from wallet division {from_div} requires Accountant or Account Take {from_div}"),
            ));
        }
        corp.ensure_wallet_divisions();
        let from_pos = corp.wallet_divisions.iter().position(|d| d.division_id == from_div).expect("7 divisions");
        let new_from = corp.wallet_divisions[from_pos]
            .balance
            .checked_sub(amount)
            .ok_or_else(|| rpc_err("NotEnoughMoney", "Insufficient funds in the source wallet division"))?;
        corp.wallet_divisions[from_pos].balance = new_from;
        if from_div == 1 {
            corp.master_wallet = new_from;
        }
        let internal = to_id == corp_id;
        let mut to_bal = None;
        if internal {
            let to_div = to_key
                .and_then(division_of)
                .ok_or_else(|| rpc_err("InvalidAccountKey", "toAccountKey required for a division transfer"))?;
            let to_pos = corp.wallet_divisions.iter().position(|d| d.division_id == to_div).expect("7 divisions");
            let nb = corp.wallet_divisions[to_pos].balance.checked_add(amount).unwrap_or(corp.wallet_divisions[to_pos].balance);
            corp.wallet_divisions[to_pos].balance = nb;
            if to_div == 1 {
                corp.master_wallet = nb;
            }
            to_bal = Some((corp_id, to_div as i32 + 999, nb));
        }
        if !internal {
            if let Some(ch) = snap.characters.get_mut(&to_id) {
                ch.balance = ch.balance.checked_add(amount).unwrap_or(ch.balance);
                to_bal = Some((to_id, ACCOUNTING_KEY_CASH, ch.balance));
            } else if let Some(other) = snap.corporations.get_mut(&to_id) {
                other.ensure_wallet_divisions();
                other.master_wallet = other.master_wallet.checked_add(amount).unwrap_or(other.master_wallet);
                let mw = other.master_wallet;
                if let Some(d) = other.wallet_divisions.iter_mut().find(|d| d.division_id == 1) {
                    d.balance = mw;
                }
                to_bal = Some((to_id, ACCOUNTING_KEY_CASH, mw));
            } else {
                return Err(rpc_err("InvalidRecipient", format!("Recipient {to_id} not found")));
            }
        }
        let desc = if reason.is_empty() { format!("Corporation transfer by {char_id}") } else { reason.clone() };
        let meta = JournalMeta::new(ref_types::CORPORATION_ACCOUNT_WITHDRAWAL, corp_id, to_id, now)
            .with_context(JournalContext::Character, char_id);
        snap.append_journal_entry(JournalEntryInput {
            owner_id: corp_id,
            entry_type: "CorporationAccountWithdrawal",
            amount: IskCents(-amount.0),
            balance_after: new_from,
            reason: desc.clone(),
            meta: meta.clone().with_account_key(from_key as i32),
        });
        let (rid, rkey, rbal) = to_bal.expect("recipient credited");
        snap.append_journal_entry(JournalEntryInput {
            owner_id: rid,
            entry_type: "CorporationAccountWithdrawal",
            amount,
            balance_after: rbal,
            reason: desc,
            meta: meta.with_account_key(rkey),
        });
        Ok((corp_id, rid, rbal, internal))
    })?;
    let (_, rid, rbal, internal) = res;
    if !internal && ctx.db.with_snapshot(|s| s.characters.contains_key(&rid)) {
        let _ = ctx.notify_account_change(rid, "cash", rid, rbal.to_wire_f64());
    }
    Ok(PyRep::None)
}

fn get_journal(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let account_key = args.first().and_then(|a| a.as_i64()).unwrap_or(ACCOUNTING_KEY_CASH as i64) as i32;
    let year = args.get(1).and_then(|a| a.as_i64());
    let month = args.get(2).and_then(|a| a.as_i64());
    let is_corp = args.get(3).and_then(|a| a.as_bool()).unwrap_or(false);
    let rows = ctx.db.with_snapshot(|snap| -> Result<Vec<Vec<PyRep>>, BridgeRpcError> {
        let owner = if is_corp {
            let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
            let corp = snap.corporations.get(&corp_id).ok_or_else(|| rpc_err("CorporationNotFound", "no corporation"))?;
            let role = corp.member_roles.get(&char_id).copied().unwrap_or(0);
            if char_id != corp.ceo_id
                && !corp_roles::has_role(role, corp_roles::ROLE_ACCOUNTANT)
                && !corp_roles::has_role(role, corp_roles::ROLE_JUNIOR_ACCOUNTANT)
            {
                return Err(rpc_err("CrpAccessDenied", "Corporation journal requires Accountant or Junior Accountant"));
            }
            corp_id
        } else {
            char_id
        };
        Ok(snap
            .wallet_journal
            .iter()
            .rev()
            .filter(|j| j.character_id == owner)
            .filter_map(|j| snap.wallet_journal_meta.get(&j.entry_id).map(|m| (j, m)))
            .filter(|(_, m)| m.account_key == account_key)
            .filter(|(_, m)| {
                let (y, mo) = year_month(m.date_unix);
                year.map_or(true, |v| v == y) && month.map_or(true, |v| v == mo)
            })
            .map(|(j, m)| {
                vec![
                    PyRep::Integer(j.entry_id),
                    PyRep::Integer(filetime(m.date_unix)),
                    PyRep::Integer(m.context_id),
                    PyRep::Integer(m.ref_type_id as i64),
                    PyRep::Integer(m.first_party_id),
                    PyRep::Integer(m.second_party_id),
                    PyRep::Integer(m.account_key as i64),
                    PyRep::Real(j.amount.to_wire_f64()),
                    PyRep::Real(j.balance_after.to_wire_f64()),
                    PyRep::String(j.reason.clone()),
                ]
            })
            .collect())
    })?;
    Ok(PyRep::rowset(
        vec![
            "transactionID", "transactionDate", "referenceID", "entryTypeID", "ownerID1", "ownerID2",
            "accountKey", "amount", "balance", "description",
        ],
        rows,
    ))
}

fn market_transactions(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    corp: bool,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let from_unix = args.first().and_then(|a| a.as_i64()).map(unix_from_filetime).unwrap_or(0);
    let corp_key = if corp { args.get(1).and_then(|a| a.as_i64()).unwrap_or(1000) as i32 } else { ACCOUNTING_KEY_CASH };
    let rows = ctx.db.with_snapshot(|snap| -> Result<Vec<Vec<PyRep>>, BridgeRpcError> {
        let owner = if corp {
            let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
            let c = snap.corporations.get(&corp_id).ok_or_else(|| rpc_err("CorporationNotFound", "no corporation"))?;
            let role = c.member_roles.get(&char_id).copied().unwrap_or(0);
            if char_id != c.ceo_id
                && !corp_roles::has_role(role, corp_roles::ROLE_ACCOUNTANT)
                && !corp_roles::has_role(role, corp_roles::ROLE_JUNIOR_ACCOUNTANT)
            {
                return Err(rpc_err("CrpAccessDenied", "Corporation transactions require Accountant or Junior Accountant"));
            }
            corp_id
        } else {
            char_id
        };
        Ok(snap
            .market_transactions
            .iter()
            .rev()
            .filter(|t| t.date_unix >= from_unix)
            .filter(|t| {
                (t.buyer_id == owner && t.buyer_account_key == corp_key)
                    || (t.seller_id == owner && t.seller_account_key == corp_key)
            })
            .map(|t| {
                vec![
                    PyRep::Integer(t.transaction_id),
                    PyRep::Integer(filetime(t.date_unix)),
                    PyRep::Integer(t.type_id as i64),
                    PyRep::Integer(t.quantity),
                    PyRep::Real(t.price.to_wire_f64()),
                    PyRep::Integer(t.location_id),
                    PyRep::Integer(t.buyer_id),
                    PyRep::Integer(t.seller_id),
                    PyRep::Integer(corp_key as i64),
                    PyRep::Integer(t.buyer_account_key as i64),
                    PyRep::Integer(t.seller_account_key as i64),
                    PyRep::Integer(t.journal_ref_id),
                ]
            })
            .collect())
    })?;
    Ok(PyRep::rowset(
        vec![
            "transactionID", "transactionDate", "typeID", "quantity", "price", "stationID", "buyerID",
            "sellerID", "accountID", "buyerAccountID", "sellerAccountID", "journalRefID",
        ],
        rows,
    ))
}

fn deposit_plex(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let location_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| rpc_err("InvalidArgument", "locationID required"))?;
    let item_id = args.get(1).and_then(|a| a.as_i64()).ok_or_else(|| rpc_err("InvalidArgument", "itemID required"))?;
    let qty = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
    if qty <= 0 {
        return Err(rpc_err("InvalidArgument", "quantity must be positive"));
    }
    let docked = session.station_id.or(session.structure_id);
    if docked != Some(location_id) {
        return Err(rpc_err("NotDockedAtLocation", "PLEX can only be deposited from the hangar you are docked in"));
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        let item = snap.items.get(&item_id).cloned().ok_or_else(|| rpc_err("ItemNotFound", "item not found"))?;
        if item.owner_id != char_id || item.location_id != location_id || item.flag != InventoryFlag::Hangar {
            return Err(rpc_err("ItemNotFound", "PLEX item is not in your hangar at this location"));
        }
        if item.type_id != PLEX_TYPE_ID {
            return Err(rpc_err("NotPlex", "only PLEX can be deposited into the PLEX Vault"));
        }
        if (item.quantity as i64) < qty {
            return Err(rpc_err("NotEnoughQuantity", "stack has fewer units than requested"));
        }
        if item.quantity as i64 == qty {
            snap.items.remove(&item_id);
        } else if let Some(i) = snap.items.get_mut(&item_id) {
            i.quantity -= qty as i32;
            i.version += 1;
        }
        snap.adjust_plex_vault(char_id, qty).ok_or_else(|| rpc_err("VaultError", "vault update failed"))?;
        snap.next_entity_id += 1;
        Ok(PyRep::Integer(snap.next_entity_id))
    })
}
