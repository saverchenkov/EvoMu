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
use crate::router::{NotificationTarget, RpcArgs, ServerContext};
use eve_db::corp_roles;
use eve_db::ledger::{ref_types, JournalContext, JournalEntryInput, JournalMeta, ACCOUNTING_KEY_CASH};
use eve_db::{InventoryFlag, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, DbColumnType, PyRep};
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
                Some(handle_give_cash_from_corp(ctx, session, args, kwargs))
            }
        }
        ("account", "GetTransactions") => Some(handle_get_journal(ctx, session, args)),
        ("account", "GetEntryTypes") => Some(handle_get_entry_types()),
        ("marketProxy", "CharGetTransactions") => {
            Some(handle_market_transactions(ctx, session, args, false))
        }
        ("marketProxy", "CorpGetTransactions") => {
            Some(handle_market_transactions(ctx, session, args, true))
        }
        ("invbroker", "DepositPlexToVault") => Some(handle_deposit_plex(ctx, session, args, kwargs)),
        ("invbroker", "WithdrawPlexFromVault") => {
            Some(handle_withdraw_plex(ctx, session, args, kwargs))
        }
        _ => None,
    }
}

fn handle_give_cash_from_corp(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    give_cash_from_corp(ctx, session, args, kwargs)
}

fn handle_get_journal(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    get_journal(ctx, session, args)
}

fn handle_get_entry_types() -> Result<PyRep, BridgeRpcError> {
    use eve_db::ledger::ref_types;
    use eve_proto::{DBRowDescriptor, DbColumnType};

    const ENTRY_TYPES: &[(i32, &str, &str)] = &[
        (0, "Undefined", "Undefined"),
        (1, "Player Trading", "Direct trade between players"),
        (ref_types::MARKET_TRANSACTION, "Market Transaction", "Market buy or sell transaction"),
        (ref_types::PLAYER_DONATION, "Player Donation", "Direct ISK transfer between players"),
        (17, "Bounty Prize", "NPC bounty payout"),
        (ref_types::INSURANCE, "Insurance", "Insurance payout"),
        (33, "Agent Mission Reward", "Agent mission reward"),
        (34, "Agent Mission Time Bonus Reward", "Agent mission time bonus reward"),
        (ref_types::CORPORATION_ACCOUNT_WITHDRAWAL, "Corporation Account Withdrawal", "Corporation wallet transfer"),
        (ref_types::MARKET_ESCROW, "Market Escrow", "Market order buy escrow"),
        (ref_types::BROKER_FEE, "Brokers Fee", "Market broker fee"),
        (ref_types::TRANSACTION_TAX, "Transaction Tax", "Market sales tax"),
        (ref_types::CONTRACT_COLLATERAL, "Contract Collateral", "Contract courier collateral"),
        (ref_types::CONTRACT_REWARD, "Contract Reward", "Contract courier reward"),
        (ref_types::CONTRACT_PRICE, "Contract Price", "Contract item exchange price"),
        (ref_types::CONTRACT_BROKERS_FEE, "Contract Brokers Fee", "Contract broker fee"),
        (85, "Bounty Prizes", "Solar system bounty prize payout"),
    ];

    let descriptor = DBRowDescriptor::new([
        ("entryTypeID", DbColumnType::I4),
        ("entryTypeName", DbColumnType::WStr),
        ("entryTypeNameID", DbColumnType::I4),
        ("description", DbColumnType::WStr),
    ]);
    let rows = ENTRY_TYPES
        .iter()
        .map(|&(id, name, desc)| {
            vec![
                PyRep::Integer(i64::from(id)),
                PyRep::String(name.to_string()),
                PyRep::None,
                PyRep::String(desc.to_string()),
            ]
        })
        .collect();
    Ok(descriptor.build_crowset(rows))
}

fn handle_market_transactions(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    is_corp: bool,
) -> Result<PyRep, BridgeRpcError> {
    market_transactions(ctx, session, args, is_corp)
}

fn handle_deposit_plex(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    deposit_plex(ctx, session, args, kwargs)
}

fn handle_withdraw_plex(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    withdraw_plex(ctx, session, args, kwargs)
}

fn give_cash_from_corp(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let rpc = RpcArgs::new(args, kwargs);
    let to_id = rpc.req_positive_i64(0, "toID")?;
    let amount_isk = rpc.req_positive_f64(1, "amount")?;
    let from_key = rpc.opt_i64(2, "fromAccountKey")?.unwrap_or(0);
    let to_key = rpc.opt_i64(3, "toAccountKey")?.filter(|k| *k != 0);
    let reason = rpc.opt_str(4, "reason")?.unwrap_or("").to_string();
    let from_div = division_of(from_key)
        .ok_or_else(|| rpc_err("InvalidAccountKey", format!("fromAccountKey {from_key} is not a corp wallet division")))?;
    let amount = IskCents::from_f64_isk(amount_isk);
    let now = ctx.now_unix();
    let caller_corp_id = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0));
    if caller_corp_id > 0 {
        crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(
            ctx,
            caller_corp_id,
        );
    }
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .ok_or_else(|| rpc_err("CharacterNotFound", "character not found"))?;
        if !snap.corporations.contains_key(&corp_id) {
            return Err(rpc_err("CorporationNotFound", format!("Corporation {corp_id} not found")));
        }
        let (is_ceo, eff_roles) = snap.effective_corp_roles_at(corp_id, char_id, None, 0, now);
        let take_role = corp_roles::ROLE_ACCOUNT_CAN_TAKE[(from_div - 1) as usize];
        if !is_ceo
            && !corp_roles::has_role(eff_roles, corp_roles::ROLE_DIRECTOR)
            && !corp_roles::has_role(eff_roles, take_role)
        {
            return Err(rpc_err(
                "CrpAccessDenied",
                format!("Withdrawing from wallet division {from_div} requires Director or Account Take {from_div}"),
            ));
        }
        let corp = snap
            .corporations
            .get_mut(&corp_id)
            .ok_or_else(|| rpc_err("CorporationNotFound", format!("Corporation {corp_id} not found")))?;
        let new_from = corp
            .debit_division(from_div, amount)
            .ok_or_else(|| rpc_err("NotEnoughMoney", "Insufficient funds in the source wallet division"))?;
        let internal = to_id == corp_id;
        let mut to_bal: Option<(i64, i32, IskCents)> = None;
        let dest_info = if internal {
            let to_div = to_key
                .and_then(division_of)
                .ok_or_else(|| rpc_err("InvalidAccountKey", "toAccountKey required for a division transfer"))?;
            let nb = corp.credit_division(to_div, amount);
            to_bal = Some((corp_id, to_div as i32 + 999, nb));
            Some((true, corp_id, to_div, nb))
        } else {
            None
        };
        let mut ext_dest = dest_info;
        if !internal {
            if let Some(ch) = snap.characters.get_mut(&to_id) {
                ch.balance = ch.balance.checked_add(amount).unwrap_or(ch.balance);
                to_bal = Some((to_id, ACCOUNTING_KEY_CASH, ch.balance));
                ext_dest = Some((false, to_id, 1, ch.balance));
            } else if let Some(other) = snap.corporations.get_mut(&to_id) {
                let mw = other.credit_division(1, amount);
                to_bal = Some((to_id, ACCOUNTING_KEY_CASH, mw));
                ext_dest = Some((true, to_id, 1, mw));
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
        Ok((corp_id, from_div, new_from, ext_dest))
    })?;
    let (src_corp_id, src_div, src_bal, dest_update) = res;
    let _ = ctx.notify_wallet_balance(true, src_corp_id, src_div, src_bal);
    if let Some((d_is_corp, d_owner, d_div, d_bal)) = dest_update {
        let _ = ctx.notify_wallet_balance(d_is_corp, d_owner, d_div, d_bal);
    }
    Ok(PyRep::None)
}

fn get_journal(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let rpc = RpcArgs::positional(args);
    let account_key = rpc.opt_i32(0, "accountKey")?.unwrap_or(ACCOUNTING_KEY_CASH);
    let year = rpc.opt_i64(1, "year")?;
    let month = rpc.opt_i64(2, "month")?;
    let is_corp = rpc.opt_bool(3, "isCorp")?.unwrap_or(false);
    let now = ctx.now_unix();
    let rows = ctx.db.with_snapshot(|snap| -> Result<Vec<Vec<PyRep>>, BridgeRpcError> {
        let owner = if is_corp {
            let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
            if !snap.corporations.contains_key(&corp_id) {
                return Err(rpc_err("CorporationNotFound", "no corporation"));
            }
            let (is_ceo, eff_roles) = snap.effective_corp_roles_at(corp_id, char_id, None, 0, now);
            let div_take = division_of(i64::from(account_key))
                .map(|div| corp_roles::ROLE_ACCOUNT_CAN_TAKE[(div - 1) as usize])
                .unwrap_or(0);
            let allowed_mask = corp_roles::ROLE_DIRECTOR
                | corp_roles::ROLE_ACCOUNTANT
                | corp_roles::ROLE_JUNIOR_ACCOUNTANT
                | div_take;
            if !is_ceo && (eff_roles & allowed_mask) == 0 {
                return Err(rpc_err(
                    "CrpAccessDenied",
                    "Corporation journal requires Director, Accountant, Junior Accountant, or Account Take for the division",
                ));
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
                let reference_id = if m.ref_type_id == ref_types::INSURANCE && m.context != JournalContext::Type {
                    670
                } else {
                    m.context_id
                };
                vec![
                    PyRep::Integer(j.entry_id),
                    PyRep::Integer(filetime(m.date_unix)),
                    PyRep::Integer(reference_id),
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
    let descriptor = DBRowDescriptor::new([
        ("transactionID", DbColumnType::I8),
        ("transactionDate", DbColumnType::FileTime),
        ("referenceID", DbColumnType::I8),
        ("entryTypeID", DbColumnType::I4),
        ("ownerID1", DbColumnType::I8),
        ("ownerID2", DbColumnType::I8),
        ("accountKey", DbColumnType::I4),
        ("amount", DbColumnType::R8),
        ("balance", DbColumnType::R8),
        ("description", DbColumnType::WStr),
    ]);
    Ok(descriptor.build_crowset(rows))
}

fn market_transactions(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    corp: bool,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let rpc = RpcArgs::positional(args);
    let from_unix = rpc.opt_i64(0, "fromDate")?.map(unix_from_filetime).unwrap_or(0);
    let corp_key = if corp { rpc.opt_i32(1, "accountKey")?.unwrap_or(1000) } else { ACCOUNTING_KEY_CASH };
    let now = ctx.now_unix();
    let rows = ctx.db.with_snapshot(|snap| -> Result<Vec<Vec<PyRep>>, BridgeRpcError> {
        let owner = if corp {
            let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
            if !snap.corporations.contains_key(&corp_id) {
                return Err(rpc_err("CorporationNotFound", "no corporation"));
            }
            let (is_ceo, eff_roles) = snap.effective_corp_roles_at(corp_id, char_id, None, 0, now);
            let div_take = division_of(i64::from(corp_key))
                .map(|div| corp_roles::ROLE_ACCOUNT_CAN_TAKE[(div - 1) as usize])
                .unwrap_or(0);
            let allowed_mask = corp_roles::ROLE_DIRECTOR
                | corp_roles::ROLE_ACCOUNTANT
                | corp_roles::ROLE_JUNIOR_ACCOUNTANT
                | corp_roles::ROLE_TRADER
                | div_take;
            if !is_ceo && (eff_roles & allowed_mask) == 0 {
                return Err(rpc_err(
                    "CrpAccessDenied",
                    "Corporation transactions require Director, Accountant, Junior Accountant, Trader, or Account Take for the division",
                ));
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
    let descriptor = DBRowDescriptor::new([
        ("transactionID", DbColumnType::I8),
        ("transactionDate", DbColumnType::FileTime),
        ("typeID", DbColumnType::I4),
        ("quantity", DbColumnType::I4),
        ("price", DbColumnType::R8),
        ("stationID", DbColumnType::I8),
        ("buyerID", DbColumnType::I8),
        ("sellerID", DbColumnType::I8),
        ("accountID", DbColumnType::I8),
        ("buyerAccountID", DbColumnType::I4),
        ("sellerAccountID", DbColumnType::I4),
        ("journalRefID", DbColumnType::I8),
    ]);
    Ok(descriptor.build_crowset(rows))
}

#[derive(Default)]
struct DepositPlexOutcome {
    entity_id: i64,
    user_id: u64,
    tx_id: u64,
    rem_plex: i64,
    qty: i64,
    pre_item: Option<eve_db::InventoryItemRecord>,
    post_item: Option<eve_db::InventoryItemRecord>,
}

fn deposit_plex_inner(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    args: &[PyRep],
) -> Result<DepositPlexOutcome, BridgeRpcError> {
    let rpc = RpcArgs::positional(args);
    let location_id = rpc.req_positive_i64(0, "locationID")?;
    let item_id = rpc.req_positive_i64(1, "itemID")?;
    let qty = rpc.req_positive_i64(2, "quantity")?;
    let Ok(qty_i32) = i32::try_from(qty) else {
        return Err(rpc_err("InvalidArgument", "quantity exceeds stack limit"));
    };
    let docked = session.station_id.or(session.structure_id);
    if docked != Some(location_id) {
        return Err(rpc_err("NotDockedAtLocation", "PLEX can only be deposited from the hangar you are docked in"));
    }
    let cs_state = crate::services::inventory_fitting::fitting_containers_ships::read_container_ship_state(ctx);
    if cs_state.alsc_locked_items.contains(&item_id) {
        return Err(rpc_err("ItemLocked", "PLEX item is locked in an Audit Log Secure Container"));
    }
    if let Some(corp_id) = ctx.db.with_snapshot(|snap| {
        let mut cur = snap.items.get(&item_id);
        for _ in 0..8 {
            let Some(it) = cur else { break };
            if snap.corporations.contains_key(&it.owner_id) {
                return Some(it.owner_id);
            }
            cur = snap.items.get(&it.location_id);
        }
        None
    }) {
        crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(
            ctx, corp_id,
        );
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(persisted_dock) = snap
            .characters
            .get(&char_id)
            .and_then(|c| c.station_id.or(c.structure_id))
        {
            if persisted_dock != location_id {
                return Err(rpc_err(
                    "NotDockedAtLocation",
                    "Character is not physically docked at this location",
                ));
            }
        }
        let item = snap.items.get(&item_id).cloned().ok_or_else(|| rpc_err("ItemNotFound", "item not found"))?;
        if snap.is_item_locked_in_contract_or_job(item_id) {
            return Err(rpc_err(
                "ItemLocked",
                "PLEX item is escrowed in an active contract, job, direct trade, or HyperNet raffle",
            ));
        }
        let ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        if ext.asset_safety_wraps.values().any(|w| {
            !w.retrieval_fee_paid
                && (w.wrap_id == item_id
                    || w.wrap_id == item.location_id
                    || w.item_ids.contains(&item_id))
        }) {
            return Err(rpc_err("ItemLocked", "PLEX item is sealed inside an Asset Safety wrap"));
        }
        if item.location_id != location_id {
            return Err(rpc_err("ItemNotFound", "PLEX item is not in your hangar at this location"));
        }
        let corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .or(session.corporation_id)
            .unwrap_or(0);
        if item.owner_id == char_id {
            if item.flag != InventoryFlag::Hangar {
                return Err(rpc_err("ItemNotFound", "PLEX item is not in your hangar at this location"));
            }
        } else if corp_id > 0 && item.owner_id == corp_id && (116..=122).contains(&(item.flag as i64)) {
            crate::services::inventory_bound::validate_corp_source_item_take_access_in_snap(
                ctx, snap, char_id, corp_id, &item,
            )?;
        } else {
            return Err(rpc_err("ItemNotFound", "PLEX item is not in your hangar at this location"));
        }
        if item.type_id != PLEX_TYPE_ID {
            return Err(rpc_err("NotPlex", "only PLEX can be deposited into the PLEX Vault"));
        }
        if item.quantity < qty_i32 {
            return Err(rpc_err("NotEnoughQuantity", "stack has fewer units than requested"));
        }
        let pre_item = Some(item.clone());
        let mut opts = eve_db::ConsumeExactOptions::for_character_stack(char_id);
        opts.expected_owner_id = item.owner_id;
        opts.required_location_id = Some(location_id);
        opts.required_flag = Some(item.flag);
        snap.consume_exact_item_quantity(item_id, qty_i32, opts)
            .map_err(|e| rpc_err("ItemLocked", e.to_string()))?;
        let post_item = snap.items.get(&item_id).cloned();
        let rem_plex = snap.adjust_plex_vault(char_id, qty).ok_or_else(|| rpc_err("VaultError", "vault update failed"))?;
        snap.next_entity_id += 1;
        let tx_id = snap.next_entity_id as u64;
        let user_id = snap.characters.get(&char_id).map(|c| c.user_id).unwrap_or(char_id as u64);
        crate::services::quasar_payment_store::record_plex_transaction(
            snap,
            tx_id,
            user_id,
            qty,
            rem_plex,
            "PLEX Vault Deposit from hangar",
            ctx.now_unix(),
        );
        Ok(DepositPlexOutcome {
            entity_id: snap.next_entity_id,
            user_id,
            tx_id,
            rem_plex,
            qty,
            pre_item,
            post_item,
        })
    })
}

fn deposit_plex(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let DepositPlexOutcome {
        entity_id,
        user_id,
        tx_id,
        rem_plex,
        qty,
        pre_item,
        post_item,
    } = match deposit_plex_inner(ctx, session, char_id, args) {
            Ok(v) => v,
            Err(e) => {
                if let Some(reference) = kwargs.get("reference").or_else(|| args.get(3)).cloned() {
                    super::notify_emitters_missing::emit_plex_deposit_failed(ctx, char_id, &e.code, reference);
                }
                return Err(e);
            }
        };
    ctx.notify_item_mutation(char_id, pre_item.as_ref(), post_item.as_ref());
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPlexDeposited",
        "charid",
        vec![PyRep::Integer(entity_id)],
    );
    crate::services::quasar_payment_store::emit_balance_changed_notice(
        &ctx.quasar_gateway.pending_notices,
        user_id,
        tx_id,
        qty,
        rem_plex,
        "PLEX Vault Deposit from hangar",
        ctx.now_unix(),
    );
    Ok(PyRep::Integer(entity_id))
}

struct WithdrawPlexOutcome {
    rep: PyRep,
    final_item_id: i64,
    user_id: u64,
    tx_id: u64,
    rem_plex: i64,
    qty: i64,
    pre_item: Option<eve_db::InventoryItemRecord>,
    post_item: Option<eve_db::InventoryItemRecord>,
}

impl Default for WithdrawPlexOutcome {
    fn default() -> Self {
        Self {
            rep: PyRep::None,
            final_item_id: 0,
            user_id: 0,
            tx_id: 0,
            rem_plex: 0,
            qty: 0,
            pre_item: None,
            post_item: None,
        }
    }
}

fn withdraw_plex_inner(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<WithdrawPlexOutcome, BridgeRpcError> {

    let mut qty = kwargs
        .get("quantity")
        .or_else(|| kwargs.get("qty"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let mut location_id_opt = kwargs
        .get("locationID")
        .or_else(|| kwargs.get("location_id"))
        .and_then(|v| v.as_i64());
    let mut flag_opt = kwargs
        .get("flag")
        .or_else(|| kwargs.get("flagID"))
        .and_then(|v| v.as_i64());

    if qty <= 0 {
        if let Some(first_num) = args.first().and_then(|a| a.as_i64()) {
            if let Some(sec_num) = args.get(1).and_then(|a| a.as_i64()) {
                let sec_is_loc = session.station_id == Some(sec_num)
                    || session.structure_id == Some(sec_num)
                    || session.ship_id == Some(sec_num);
                if sec_is_loc || (sec_num > 10_000_000 && first_num <= 10_000_000) {
                    qty = first_num;
                    location_id_opt = Some(sec_num);
                } else {
                    location_id_opt = Some(first_num);
                    qty = sec_num;
                }
                if let Some(f) = args.get(2).and_then(|a| a.as_i64()) {
                    flag_opt = Some(f);
                }
            } else {
                qty = first_num;
            }
        }
    }

    if qty <= 0 {
        return Err(rpc_err("InvalidArgument", "Withdraw quantity must be positive"));
    }
    let qty_i32 = i32::try_from(qty).map_err(|_| {
        rpc_err("InvalidArgument", "Withdraw quantity exceeds maximum stack size (i32::MAX)")
    })?;

    let (target_location, target_flag) = match location_id_opt {
        Some(loc) => {
            let is_docked = session.station_id == Some(loc) || session.structure_id == Some(loc);
            let is_active_ship = session.ship_id == Some(loc);
            if !is_docked && !is_active_ship {
                return Err(rpc_err(
                    "NotDockedAtLocation",
                    "PLEX can only be withdrawn into your current station hangar, structure hangar, or active ship cargo",
                ));
            }
            let fl = flag_opt.map(|f| InventoryFlag::from_u16(f as u16)).unwrap_or_else(|| {
                if is_active_ship {
                    InventoryFlag::Cargo
                } else {
                    InventoryFlag::Hangar
                }
            });
            let valid_flag = (is_docked && fl == InventoryFlag::Hangar)
                || (is_active_ship && fl == InventoryFlag::Cargo);
            if !valid_flag {
                return Err(rpc_err(
                    "InvalidFlag",
                    "PLEX can only be withdrawn into Hangar (4) or Cargo (5)",
                ));
            }
            (loc, fl)
        }
        None => {
            if let Some(st_id) = session.station_id.or(session.structure_id) {
                (st_id, InventoryFlag::Hangar)
            } else if let Some(ship_id) = session.ship_id {
                (ship_id, InventoryFlag::Cargo)
            } else {
                return Err(rpc_err(
                    "NotDockedAtLocation",
                    "PLEX can only be withdrawn into your current station hangar, structure hangar, or active ship cargo",
                ));
            }
        }
    };

    ctx.db.try_with_mut_snapshot(|snap| {
        let vault_bal = snap.plex_vault_balance(char_id);
        if vault_bal < qty {
            return Err(rpc_err(
                "NotEnoughPlex",
                format!("Insufficient PLEX in account vault ({vault_bal} available, {qty} requested)"),
            ));
        }
        let existing_item = snap
            .items
            .values()
            .find(|i| {
                i.owner_id == char_id
                    && i.location_id == target_location
                    && i.flag == target_flag
                    && i.type_id == PLEX_TYPE_ID
                    && !i.singleton
            })
            .map(|i| (i.item_id, i.quantity));
        if let Some((_, cur_q)) = existing_item {
            if cur_q.checked_add(qty_i32).is_none() {
                return Err(rpc_err(
                    "InvalidArgument",
                    "Merged PLEX stack would overflow i32::MAX",
                ));
            }
        }
        let rem_plex = snap.adjust_plex_vault(char_id, -qty)
            .ok_or_else(|| rpc_err("VaultError", "Failed to deduct PLEX from vault"))?;
        snap.next_entity_id += 1;
        let tx_id = snap.next_entity_id as u64;
        let user_id = snap.characters.get(&char_id).map(|c| c.user_id).unwrap_or(char_id as u64);
        crate::services::quasar_payment_store::record_plex_transaction(
            snap,
            tx_id,
            user_id,
            -qty,
            rem_plex,
            "PLEX Vault Withdrawal to hangar",
            ctx.now_unix(),
        );

        let (pre_item, post_item, final_item_id) = if let Some((eid, _)) = existing_item {
            let it = snap.items.get_mut(&eid).unwrap();
            let pre = it.clone();
            it.quantity += qty_i32;
            it.version += 1;
            (Some(pre), Some(it.clone()), eid)
        } else {
            snap.next_entity_id += 1;
            let nid = snap.next_entity_id;
            let new_rec = eve_db::InventoryItemRecord {
                item_id: nid,
                type_id: PLEX_TYPE_ID,
                owner_id: char_id,
                location_id: target_location,
                flag: target_flag,
                quantity: qty_i32,
                singleton: false,
                version: 1,
                ..Default::default()
            };
            snap.items.insert(nid, new_rec.clone());
            (None, Some(new_rec), nid)
        };

        Ok(WithdrawPlexOutcome {
            rep: PyRep::keyval([
                ("itemID", PyRep::Integer(final_item_id)),
                ("quantityWithdrawn", PyRep::Integer(qty)),
                ("remainingVaultBalance", PyRep::Integer(rem_plex)),
                ("locationID", PyRep::Integer(target_location)),
                ("flag", PyRep::Integer(target_flag as u16 as i64)),
                ("reference", PyRep::Integer(tx_id as i64)),
            ]),
            final_item_id,
            user_id,
            tx_id,
            rem_plex,
            qty,
            pre_item,
            post_item,
        })
    })
}

fn withdraw_plex(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let WithdrawPlexOutcome {
        rep,
        final_item_id,
        user_id,
        tx_id,
        rem_plex,
        qty,
        pre_item,
        post_item,
    } = match withdraw_plex_inner(ctx, session, char_id, args, kwargs) {
            Ok(v) => v,
            Err(e) => {
                if let Some(reference) = kwargs.get("reference").or_else(|| args.get(3)).cloned() {
                    super::notify_emitters_missing::emit_plex_withdraw_failed(ctx, char_id, &e.code, reference);
                }
                return Err(e);
            }
        };
    ctx.notify_item_mutation(char_id, pre_item.as_ref(), post_item.as_ref());
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnPlexItemAddedToInventory",
        "charid",
        vec![PyRep::Integer(final_item_id), PyRep::Integer(tx_id as i64)],
    );
    crate::services::quasar_payment_store::emit_balance_changed_notice(
        &ctx.quasar_gateway.pending_notices,
        user_id,
        tx_id,
        -qty,
        rem_plex,
        "PLEX Vault Withdrawal to hangar",
        ctx.now_unix(),
    );
    Ok(rep)
}

#[cfg(test)]
#[path = "wallet_ledger_tests.rs"]
mod wallet_ledger_tests;

#[path = "wallet_ledger_cov_unit_tests.rs"]
#[cfg(test)]
mod wallet_ledger_cov_unit_tests;

