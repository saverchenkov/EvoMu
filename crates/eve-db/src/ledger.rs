//! Wallet ledger extensions: per-entry journal metadata (parties, context, ref type, timestamp),
//! market transaction records written at fill time, and the account-wide PLEX Vault.
//!
//! Journal metadata is keyed by `WalletJournalEntry::entry_id` so existing journal rows keep their
//! layout; `ref_type_id` values are the V24 client `appConst.ref*` constants.

use crate::types::{IskCents, WalletJournalEntry, WorldStateSnapshot};
use eve_sde::TypeId;
use serde::{Deserialize, Serialize};

/// V24 client `appConst.ref*` journal reference type IDs used by the server.
pub mod ref_types {
    pub const MARKET_TRANSACTION: i32 = 2;
    pub const PLAYER_DONATION: i32 = 10;
    pub const CORPORATION_ACCOUNT_WITHDRAWAL: i32 = 37;
    pub const MARKET_ESCROW: i32 = 42;
    pub const BROKER_FEE: i32 = 46;
    pub const TRANSACTION_TAX: i32 = 54;
    pub const CONTRACT_PRICE: i32 = 71;
    pub const CONTRACT_BROKERS_FEE: i32 = 72;
    pub const CONTRACT_REWARD: i32 = 68;
    pub const CONTRACT_COLLATERAL: i32 = 65;
}

/// V24 client `appConst.accountingKeyCash` (personal wallet / corp master wallet).
pub const ACCOUNTING_KEY_CASH: i32 = 1000;

/// Context of a journal entry (what the `context_id` refers to).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum JournalContext {
    #[default]
    None,
    MarketOrder,
    MarketTransaction,
    Contract,
    Character,
    Corporation,
}

/// Parties/context metadata for one wallet journal entry (`ownerID1`, `ownerID2`, `referenceID`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct JournalMeta {
    pub ref_type_id: i32,
    /// Party the ISK came from (`ownerID1`).
    pub first_party_id: i64,
    /// Party the ISK went to (`ownerID2`).
    pub second_party_id: i64,
    pub context_id: i64,
    pub context: JournalContext,
    pub account_key: i32,
    pub date_unix: u64,
}

/// One market fill, recorded at fill time (`marketProxy.CharGetTransactions` rows).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketTransactionRecord {
    pub transaction_id: i64,
    pub date_unix: u64,
    pub type_id: TypeId,
    pub quantity: i64,
    pub price: IskCents,
    pub location_id: i64,
    pub buyer_id: i64,
    pub seller_id: i64,
    pub buyer_account_key: i32,
    pub seller_account_key: i32,
    /// Journal entry of the buyer's ISK movement for this fill.
    pub journal_ref_id: i64,
}

/// Input for [`WorldStateSnapshot::append_journal_entry`].
#[derive(Debug, Clone)]
pub struct JournalEntryInput<'a> {
    pub owner_id: i64,
    pub entry_type: &'a str,
    pub amount: IskCents,
    pub balance_after: IskCents,
    pub reason: String,
    pub meta: JournalMeta,
}

impl JournalMeta {
    /// Metadata for an ISK movement from `first_party_id` to `second_party_id`.
    pub fn new(ref_type_id: i32, first_party_id: i64, second_party_id: i64, date_unix: u64) -> Self {
        Self {
            ref_type_id,
            first_party_id,
            second_party_id,
            account_key: ACCOUNTING_KEY_CASH,
            date_unix,
            ..Default::default()
        }
    }

    pub fn with_context(mut self, context: JournalContext, context_id: i64) -> Self {
        self.context = context;
        self.context_id = context_id;
        self
    }

    pub fn with_account_key(mut self, account_key: i32) -> Self {
        self.account_key = account_key;
        self
    }
}

/// One market fill to journal: wallet balances must already be updated by the caller.
#[derive(Debug, Clone)]
pub struct MarketFill {
    pub buyer_id: i64,
    pub seller_id: i64,
    pub type_id: TypeId,
    pub quantity: i64,
    pub price: IskCents,
    pub location_id: i64,
    pub date_unix: u64,
    pub gross: IskCents,
    /// Buyer's wallet balance after paying, when the buyer is a player wallet (debit entry written).
    pub buyer_balance_after: Option<IskCents>,
    /// Seller's net proceeds and balance after, when the seller is a player wallet.
    pub seller_net: IskCents,
    pub seller_balance_after: Option<IskCents>,
    pub sales_tax: IskCents,
    /// Recipient of the sales tax (`ownerID2` of the tax entry).
    pub tax_collector_id: i64,
}

impl WorldStateSnapshot {
    /// Records the market transaction and journals buyer debit, seller credit and sales tax with
    /// parties and the transaction as context. Returns the transaction ID.
    pub fn journal_market_fill(&mut self, f: MarketFill) -> i64 {
        let tx_id = self.record_market_transaction(
            f.date_unix, f.type_id, f.quantity, f.price, f.location_id, f.buyer_id, f.seller_id, 0,
        );
        let base = JournalMeta::new(ref_types::MARKET_TRANSACTION, f.buyer_id, f.seller_id, f.date_unix)
            .with_context(JournalContext::MarketTransaction, tx_id);
        let mut journal_ref = 0;
        if let Some(bal) = f.buyer_balance_after {
            journal_ref = self.append_journal_entry(JournalEntryInput {
                owner_id: f.buyer_id,
                entry_type: "MarketTransaction",
                amount: IskCents(-f.gross.0),
                balance_after: bal,
                reason: format!("Bought {}x type {}", f.quantity, f.type_id),
                meta: base.clone(),
            });
        }
        if let Some(bal) = f.seller_balance_after {
            let e = self.append_journal_entry(JournalEntryInput {
                owner_id: f.seller_id,
                entry_type: "MarketSell",
                amount: f.seller_net,
                balance_after: bal,
                reason: format!("Sold {}x type {}", f.quantity, f.type_id),
                meta: base.clone(),
            });
            if journal_ref == 0 {
                journal_ref = e;
            }
            if f.sales_tax.0 > 0 {
                let meta = JournalMeta::new(ref_types::TRANSACTION_TAX, f.seller_id, f.tax_collector_id, f.date_unix)
                    .with_context(JournalContext::MarketTransaction, tx_id);
                self.append_journal_entry(JournalEntryInput {
                    owner_id: f.seller_id,
                    entry_type: "TransactionTax",
                    amount: IskCents(-f.sales_tax.0),
                    balance_after: bal,
                    reason: format!("Sales tax for selling {}x type {}", f.quantity, f.type_id),
                    meta,
                });
            }
        }
        if let Some(t) = self.market_transactions.iter_mut().rev().find(|t| t.transaction_id == tx_id) {
            t.journal_ref_id = journal_ref;
        }
        let qty = f.quantity.max(1);
        let unit_price = f.price.to_wire_f64();
        if self.characters.contains_key(&f.buyer_id) {
            self.char_wallet_transactions
                .entry(f.buyer_id)
                .or_default()
                .push(crate::WalletTransactionRecord {
                    transaction_id: tx_id,
                    date_unix: f.date_unix,
                    location_id: f.location_id,
                    type_id: f.type_id,
                    unit_price,
                    quantity: qty,
                    client_id: f.seller_id,
                    is_buy: true,
                    is_personal: true,
                    journal_ref_id: journal_ref,
                });
        } else if self.corporations.contains_key(&f.buyer_id) {
            self.corp_wallet_transactions
                .entry((f.buyer_id, 1))
                .or_default()
                .push(crate::WalletTransactionRecord {
                    transaction_id: tx_id,
                    date_unix: f.date_unix,
                    location_id: f.location_id,
                    type_id: f.type_id,
                    unit_price,
                    quantity: qty,
                    client_id: f.seller_id,
                    is_buy: true,
                    is_personal: false,
                    journal_ref_id: journal_ref,
                });
        }
        if self.characters.contains_key(&f.seller_id) {
            self.char_wallet_transactions
                .entry(f.seller_id)
                .or_default()
                .push(crate::WalletTransactionRecord {
                    transaction_id: tx_id,
                    date_unix: f.date_unix,
                    location_id: f.location_id,
                    type_id: f.type_id,
                    unit_price,
                    quantity: qty,
                    client_id: f.buyer_id,
                    is_buy: false,
                    is_personal: true,
                    journal_ref_id: journal_ref,
                });
        } else if self.corporations.contains_key(&f.seller_id) {
            self.corp_wallet_transactions
                .entry((f.seller_id, 1))
                .or_default()
                .push(crate::WalletTransactionRecord {
                    transaction_id: tx_id,
                    date_unix: f.date_unix,
                    location_id: f.location_id,
                    type_id: f.type_id,
                    unit_price,
                    quantity: qty,
                    client_id: f.buyer_id,
                    is_buy: false,
                    is_personal: false,
                    journal_ref_id: journal_ref,
                });
        }
        let region_id = self
            .market_orders
            .values()
            .find(|o| o.station_id == f.location_id)
            .map(|o| o.region_id)
            .unwrap_or(10_000_002);
        let date_str = unix_secs_to_date_str(f.date_unix);
        let hist = self.market_history.entry((region_id, f.type_id)).or_default();
        if let Some(day) = hist.iter_mut().find(|d| d.date == date_str) {
            let prev_vol = day.volume.max(0);
            let new_vol = prev_vol + qty;
            day.average = ((day.average * (prev_vol as f64)) + (unit_price * (qty as f64))) / (new_vol as f64);
            day.highest = day.highest.max(unit_price);
            day.lowest = day.lowest.min(unit_price);
            day.order_count += 1;
            day.volume = new_vol;
        } else {
            hist.push(crate::MarketHistoryDayRecord {
                date: date_str,
                average: unit_price,
                highest: unit_price,
                lowest: unit_price,
                order_count: 1,
                volume: qty,
            });
        }
        tx_id
    }
}

fn unix_secs_to_date_str(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}")
}

impl WorldStateSnapshot {
    pub fn append_journal_entry(&mut self, input: JournalEntryInput<'_>) -> i64 {
        self.next_entity_id += 1;
        let entry_id = self.next_entity_id;
        let date_unix = input.meta.date_unix;
        let first_party_id = input.meta.first_party_id;
        let second_party_id = input.meta.second_party_id;
        let account_key = input.meta.account_key;
        let reason_str = input.reason.clone();
        self.wallet_journal.push(WalletJournalEntry {
            entry_id,
            character_id: input.owner_id,
            entry_type: input.entry_type.to_string(),
            amount: input.amount,
            balance_after: input.balance_after,
            reason: input.reason,
        });
        self.wallet_journal_meta.insert(entry_id, input.meta);
        self.wallet_journal_timestamps.insert(entry_id, date_unix);
        if self.corporations.contains_key(&input.owner_id) {
            let div = ((account_key - 999).clamp(1, 7)) as i64;
            self.corp_wallet_journals
                .entry((input.owner_id, div))
                .or_default()
                .push(crate::CorpWalletJournalRecord {
                    id: entry_id,
                    date_unix,
                    ref_type: input.entry_type.to_string(),
                    description: reason_str.clone(),
                    amount: input.amount.to_wire_f64(),
                    balance: input.balance_after.to_wire_f64(),
                    first_party_id: Some(first_party_id),
                    second_party_id: Some(second_party_id),
                    reason: Some(reason_str),
                });
        }
        entry_id
    }

    /// Records a market fill; returns the transaction ID.
    #[allow(clippy::too_many_arguments)]
    pub fn record_market_transaction(
        &mut self,
        date_unix: u64,
        type_id: TypeId,
        quantity: i64,
        price: IskCents,
        location_id: i64,
        buyer_id: i64,
        seller_id: i64,
        journal_ref_id: i64,
    ) -> i64 {
        self.next_entity_id += 1;
        let transaction_id = self.next_entity_id;
        self.market_transactions.push(MarketTransactionRecord {
            transaction_id,
            date_unix,
            type_id,
            quantity,
            price,
            location_id,
            buyer_id,
            seller_id,
            buyer_account_key: ACCOUNTING_KEY_CASH,
            seller_account_key: ACCOUNTING_KEY_CASH,
            journal_ref_id,
        });
        transaction_id
    }

    /// Account (`user_id`) owning `char_id`, if the character exists.
    pub fn account_of_character(&self, char_id: i64) -> Option<u64> {
        self.characters.get(&char_id).map(|c| c.user_id)
    }

    /// Account-wide PLEX Vault balance visible to `char_id` (shared by all characters on the account).
    pub fn plex_vault_balance(&self, char_id: i64) -> i64 {
        self.account_of_character(char_id)
            .and_then(|uid| self.account_plex_vault.get(&uid).copied())
            .unwrap_or(0)
    }

    /// Adds `delta` PLEX to the account vault of `char_id`. Returns the new balance, or `None` if the
    /// character is unknown or the vault would go negative (no change is made then).
    pub fn adjust_plex_vault(&mut self, char_id: i64, delta: i64) -> Option<i64> {
        let uid = self.account_of_character(char_id)?;
        let cur = self.account_plex_vault.get(&uid).copied().unwrap_or(0);
        let next = cur.checked_add(delta)?;
        if next < 0 {
            return None;
        }
        self.account_plex_vault.insert(uid, next);
        Some(next)
    }

    /// Migration: folds legacy per-character `plex_balance` values into the account-wide vault.
    pub fn fold_legacy_plex_balances(&mut self) {
        let legacy: Vec<(i64, u64, i64)> = self
            .characters
            .values()
            .filter(|c| c.plex_balance != 0)
            .map(|c| (c.character_id, c.user_id, c.plex_balance))
            .collect();
        for (cid, uid, plex) in legacy {
            *self.account_plex_vault.entry(uid).or_insert(0) += plex;
            if let Some(c) = self.characters.get_mut(&cid) {
                c.plex_balance = 0;
            }
        }
    }
}
