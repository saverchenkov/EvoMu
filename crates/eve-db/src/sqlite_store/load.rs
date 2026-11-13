//! Relational SQLite world state loader.

use crate::sqlite_store::fingerprints::{compute_fingerprints, TableFingerprints};
use crate::sqlite_store::quarantine::{
    default_quarantine_unreadable, row_col_string, row_composite_key_string, row_payload_summary,
    LoadContext,
};
use crate::types::*;
use eve_sde::{SolarSystemId, TypeId};
use rusqlite::Connection;

/// Loads the complete `WorldStateSnapshot` and its `TableFingerprints` from the SQLite database,
/// failing closed on any corrupted row unless `--quarantine-unreadable` is active.
pub fn load_snapshot_from_sqlite(
    conn: &Connection,
) -> Result<(WorldStateSnapshot, TableFingerprints), String> {
    load_snapshot_from_sqlite_with_options(conn, default_quarantine_unreadable())
}

/// Loads the persisted `next_entity_block` high-water mark from `world_meta` (falling back to
/// `next_entity_id` and the minimum `200_000_000` floor).
pub fn load_entity_block_from_sqlite(conn: &Connection) -> Result<i64, String> {
    let mut block = 200_000_000_i64;
    let mut stmt = conn
        .prepare("SELECT meta_key, meta_value FROM world_meta WHERE meta_key IN ('next_entity_block', 'next_entity_id')")
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        let val: i64 = r.get(1).map_err(|e| e.to_string())?;
        if val > block {
            block = val;
        }
    }
    Ok(block)
}

/// Loads the complete `WorldStateSnapshot` and its `TableFingerprints` from the SQLite database.
/// When `quarantine_unreadable` is `false`, any unreadable row returns `Err` naming the table and key.
/// When `quarantine_unreadable` is `true`, offending rows are moved into `quarantine_rows` and logged.
pub fn load_snapshot_from_sqlite_with_options(
    conn: &Connection,
    quarantine_unreadable: bool,
) -> Result<(WorldStateSnapshot, TableFingerprints), String> {
    let mut ctx = LoadContext::new(quarantine_unreadable);
    let mut snap = WorldStateSnapshot {
        next_entity_id: 200_000_000,
        ..Default::default()
    };

    {
        let mut stmt = conn
            .prepare("SELECT rowid, meta_key, meta_value FROM world_meta")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_payload_summary(r, 1, 3);
            match (|| -> Result<(String, i64), String> {
                Ok((
                    r.get(1).map_err(|e| e.to_string())?,
                    r.get(2).map_err(|e| e.to_string())?,
                ))
            })() {
                Ok((k, v)) => match k.as_str() {
                    "next_entity_id" => snap.next_entity_id = v,
                    "wal_seq" => snap.wal_seq = v as u64,
                    "markets_seeded" => snap.markets_seeded = v != 0,
                    _ => {}
                },
                Err(err) => {
                    ctx.record_bad_row("world_meta", "world_meta", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, username, user_id, password_token, role, banned_until_unix, ban_reason FROM accounts")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_payload_summary(r, 1, 7);
            match (|| -> Result<AccountRecord, String> {
                Ok(AccountRecord {
                    username: r.get(1).map_err(|e| e.to_string())?,
                    user_id: r.get::<_, i64>(2).map_err(|e| e.to_string())? as u64,
                    password_token: r.get(3).map_err(|e| e.to_string())?,
                    role: r.get::<_, i64>(4).map_err(|e| e.to_string())? as u64,
                    banned_until_unix: r
                        .get::<_, Option<i64>>(5)
                        .map_err(|e| e.to_string())?
                        .map(|v| v as u64),
                    ban_reason: r.get(6).map_err(|e| e.to_string())?,
                })
            })() {
                Ok(a) => {
                    snap.accounts.insert(a.username.clone(), a);
                }
                Err(err) => {
                    ctx.record_bad_row("accounts", "accounts", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, character_id, record_json FROM characters")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 2);
            match r
                .get::<_, String>(2)
                .map_err(|e| e.to_string())
                .and_then(|js| serde_json::from_str::<CharacterRecord>(&js).map_err(|e| e.to_string()))
            {
                Ok(ch) => {
                    snap.characters.insert(ch.character_id, ch);
                }
                Err(err) => {
                    ctx.record_bad_row("characters", "characters", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, character_id, type_id, level, skill_points FROM skills")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_composite_key_string(r, 1, 2);
            let payload = row_payload_summary(r, 1, 5);
            match (|| -> Result<(i64, TrainedSkillRecord), String> {
                Ok((
                    r.get::<_, i64>(1).map_err(|e| e.to_string())?,
                    TrainedSkillRecord {
                        type_id: r.get::<_, i64>(2).map_err(|e| e.to_string())? as TypeId,
                        level: r.get::<_, i64>(3).map_err(|e| e.to_string())? as u8,
                        skill_points: r.get(4).map_err(|e| e.to_string())?,
                    },
                ))
            })() {
                Ok((cid, sk)) => {
                    snap.skills.entry(cid).or_default().insert(sk.type_id, sk);
                }
                Err(err) => {
                    ctx.record_bad_row("skills", "skills", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, character_id, type_id, target_level, start_sp, end_sp, duration_seconds, started_at_unix FROM skill_queues ORDER BY character_id ASC, queue_index ASC")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_composite_key_string(r, 1, 2);
            let payload = row_payload_summary(r, 1, 8);
            match (|| -> Result<(i64, SkillQueueEntry), String> {
                Ok((
                    r.get::<_, i64>(1).map_err(|e| e.to_string())?,
                    SkillQueueEntry {
                        type_id: r.get::<_, i64>(2).map_err(|e| e.to_string())? as TypeId,
                        target_level: r.get::<_, i64>(3).map_err(|e| e.to_string())? as u8,
                        start_sp: r.get(4).map_err(|e| e.to_string())?,
                        end_sp: r.get(5).map_err(|e| e.to_string())?,
                        duration_seconds: r.get(6).map_err(|e| e.to_string())?,
                        started_at_unix: r
                            .get::<_, Option<i64>>(7)
                            .map_err(|e| e.to_string())?
                            .map(|v| v as u64),
                    },
                ))
            })() {
                Ok((cid, q)) => {
                    snap.skill_queues.entry(cid).or_default().push(q);
                }
                Err(err) => {
                    ctx.record_bad_row("skill_queues", "skill_queues", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, character_id, clones_json FROM jump_clones")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 2);
            match (|| -> Result<(i64, Vec<JumpCloneRecord>), String> {
                let cid: i64 = r.get(1).map_err(|e| e.to_string())?;
                let js: String = r.get(2).map_err(|e| e.to_string())?;
                let clones: Vec<JumpCloneRecord> =
                    serde_json::from_str(&js).map_err(|e| e.to_string())?;
                Ok((cid, clones))
            })() {
                Ok((cid, clones)) => {
                    snap.jump_clones.insert(cid, clones);
                }
                Err(err) => {
                    ctx.record_bad_row("jump_clones", "jump_clones", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, character_id, implants_json FROM active_implants")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 2);
            match (|| -> Result<(i64, Vec<TypeId>), String> {
                let cid: i64 = r.get(1).map_err(|e| e.to_string())?;
                let js: String = r.get(2).map_err(|e| e.to_string())?;
                let imps: Vec<TypeId> = serde_json::from_str(&js).map_err(|e| e.to_string())?;
                Ok((cid, imps))
            })() {
                Ok((cid, imps)) => {
                    snap.active_implants.insert(cid, imps);
                }
                Err(err) => {
                    ctx.record_bad_row(
                        "active_implants",
                        "active_implants",
                        key,
                        payload,
                        err,
                        rowid,
                    )?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, item_id, record_json FROM items")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 2);
            match r
                .get::<_, String>(2)
                .map_err(|e| e.to_string())
                .and_then(|js| {
                    serde_json::from_str::<InventoryItemRecord>(&js).map_err(|e| e.to_string())
                }) {
                Ok(item) => {
                    snap.items.insert(item.item_id, item);
                }
                Err(err) => {
                    ctx.record_bad_row("items", "items", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, fitting_id, record_json FROM saved_fittings")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 2);
            match r
                .get::<_, String>(2)
                .map_err(|e| e.to_string())
                .and_then(|js| {
                    serde_json::from_str::<SavedFittingRecord>(&js).map_err(|e| e.to_string())
                }) {
                Ok(fit) => {
                    snap.saved_fittings.insert(fit.fitting_id, fit);
                }
                Err(err) => {
                    ctx.record_bad_row(
                        "saved_fittings",
                        "saved_fittings",
                        key,
                        payload,
                        err,
                        rowid,
                    )?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, order_id, owner_id, station_id, region_id, type_id, is_bid, price_cents, vol_entered, vol_remaining, min_volume, is_npc, escrow_cents FROM market_orders")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_payload_summary(r, 1, 13);
            match (|| -> Result<MarketOrderRecord, String> {
                Ok(MarketOrderRecord {
                    order_id: r.get(1).map_err(|e| e.to_string())?,
                    owner_id: r.get(2).map_err(|e| e.to_string())?,
                    station_id: r.get(3).map_err(|e| e.to_string())?,
                    region_id: r.get::<_, i64>(4).map_err(|e| e.to_string())? as u32,
                    type_id: r.get::<_, i64>(5).map_err(|e| e.to_string())? as TypeId,
                    is_bid: r.get::<_, i64>(6).map_err(|e| e.to_string())? != 0,
                    price: IskCents(r.get(7).map_err(|e| e.to_string())?),
                    vol_entered: r.get::<_, i64>(8).map_err(|e| e.to_string())? as i32,
                    vol_remaining: r.get::<_, i64>(9).map_err(|e| e.to_string())? as i32,
                    min_volume: r.get::<_, i64>(10).map_err(|e| e.to_string())? as i32,
                    is_npc: r.get::<_, i64>(11).map_err(|e| e.to_string())? != 0,
                    escrow: IskCents(r.get(12).map_err(|e| e.to_string())?),
                })
            })() {
                Ok(ord) => {
                    snap.market_orders.insert(ord.order_id, ord);
                }
                Err(err) => {
                    ctx.record_bad_row("market_orders", "market_orders", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, entry_id, character_id, entry_type, amount_cents, balance_after_cents, reason FROM wallet_journal ORDER BY seq_idx ASC")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_payload_summary(r, 1, 7);
            match (|| -> Result<WalletJournalEntry, String> {
                Ok(WalletJournalEntry {
                    entry_id: r.get(1).map_err(|e| e.to_string())?,
                    character_id: r.get(2).map_err(|e| e.to_string())?,
                    entry_type: r.get(3).map_err(|e| e.to_string())?,
                    amount: IskCents(r.get(4).map_err(|e| e.to_string())?),
                    balance_after: IskCents(r.get(5).map_err(|e| e.to_string())?),
                    reason: r.get(6).map_err(|e| e.to_string())?,
                })
            })() {
                Ok(entry) => snap.wallet_journal.push(entry),
                Err(err) => {
                    ctx.record_bad_row(
                        "wallet_journal",
                        "wallet_journal",
                        key,
                        payload,
                        err,
                        rowid,
                    )?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, contract_id, record_json FROM contracts")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 2);
            match r
                .get::<_, String>(2)
                .map_err(|e| e.to_string())
                .and_then(|js| serde_json::from_str::<ContractRecord>(&js).map_err(|e| e.to_string()))
            {
                Ok(c) => {
                    snap.contracts.insert(c.contract_id, c);
                }
                Err(err) => {
                    ctx.record_bad_row("contracts", "contracts", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, job_id, installer_id, facility_id, blueprint_item_id, blueprint_type_id, activity_id, runs, product_type_id, status, duration_seconds, start_unix, blueprint_location_id, output_location_id, cost, licensed_runs, probability, completed_unix, completed_character_id FROM industry_jobs")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_payload_summary(r, 1, 19);
            match (|| -> Result<IndustryJobRecord, String> {
                Ok(IndustryJobRecord {
                    job_id: r.get(1).map_err(|e| e.to_string())?,
                    installer_id: r.get(2).map_err(|e| e.to_string())?,
                    facility_id: r.get(3).map_err(|e| e.to_string())?,
                    blueprint_item_id: r.get(4).map_err(|e| e.to_string())?,
                    blueprint_type_id: r.get::<_, i64>(5).map_err(|e| e.to_string())? as TypeId,
                    activity_id: r.get::<_, i64>(6).map_err(|e| e.to_string())? as u8,
                    runs: r.get::<_, i64>(7).map_err(|e| e.to_string())? as u32,
                    product_type_id: r.get::<_, i64>(8).map_err(|e| e.to_string())? as TypeId,
                    status: r.get(9).map_err(|e| e.to_string())?,
                    duration_seconds: r.get::<_, i64>(10).map_err(|e| e.to_string())? as u64,
                    start_unix: r.get::<_, i64>(11).map_err(|e| e.to_string())? as u64,
                    blueprint_location_id: r.get(12).map_err(|e| e.to_string())?,
                    output_location_id: r.get(13).map_err(|e| e.to_string())?,
                    cost: r.get(14).map_err(|e| e.to_string())?,
                    licensed_runs: r
                        .get::<_, Option<i64>>(15)
                        .map_err(|e| e.to_string())?
                        .map(|v| v as u32),
                    probability: r
                        .get::<_, Option<f64>>(16)
                        .map_err(|e| e.to_string())?
                        .map(|v| v as f32),
                    completed_unix: r
                        .get::<_, Option<i64>>(17)
                        .map_err(|e| e.to_string())?
                        .map(|v| v as u64),
                    completed_character_id: r.get(18).map_err(|e| e.to_string())?,
                })
            })() {
                Ok(j) => {
                    snap.industry_jobs.insert(j.job_id, j);
                }
                Err(err) => {
                    ctx.record_bad_row("industry_jobs", "industry_jobs", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, owner_id, planet_id, record_json FROM planetary_colonies")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_composite_key_string(r, 1, 2);
            let payload = row_col_string(r, 3);
            match r
                .get::<_, String>(3)
                .map_err(|e| e.to_string())
                .and_then(|js| {
                    serde_json::from_str::<PlanetaryColonyRecord>(&js).map_err(|e| e.to_string())
                }) {
                Ok(col) => {
                    snap.planetary_colonies
                        .insert((col.owner_id, col.planet_id), col);
                }
                Err(err) => {
                    ctx.record_bad_row(
                        "planetary_colonies",
                        "planetary_colonies",
                        key,
                        payload,
                        err,
                        rowid,
                    )?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, character_id, mission_id, record_json FROM missions")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_composite_key_string(r, 1, 2);
            let payload = row_col_string(r, 3);
            match r
                .get::<_, String>(3)
                .map_err(|e| e.to_string())
                .and_then(|js| {
                    serde_json::from_str::<ActiveMissionRecord>(&js).map_err(|e| e.to_string())
                }) {
                Ok(m) => {
                    snap.active_missions
                        .insert((m.character_id, m.mission_id), m);
                }
                Err(err) => {
                    ctx.record_bad_row("missions", "missions", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, character_id, tally FROM mission_tally")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_payload_summary(r, 1, 3);
            match (|| -> Result<(i64, u32), String> {
                Ok((
                    r.get::<_, i64>(1).map_err(|e| e.to_string())?,
                    r.get::<_, i64>(2).map_err(|e| e.to_string())? as u32,
                ))
            })() {
                Ok((cid, t)) => {
                    snap.mission_tally.insert(cid, t);
                }
                Err(err) => {
                    ctx.record_bad_row("mission_tally", "mission_tally", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, character_id, tx_json FROM standings")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 2);
            match (|| -> Result<(i64, Vec<StandingTransactionRecord>), String> {
                let cid: i64 = r.get(1).map_err(|e| e.to_string())?;
                let js: String = r.get(2).map_err(|e| e.to_string())?;
                let list: Vec<StandingTransactionRecord> =
                    serde_json::from_str(&js).map_err(|e| e.to_string())?;
                Ok((cid, list))
            })() {
                Ok((cid, list)) => {
                    snap.standing_transactions.insert(cid, list);
                }
                Err(err) => {
                    ctx.record_bad_row("standings", "standings", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, character_id, record_json FROM npe_progress")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 2);
            match r
                .get::<_, String>(2)
                .map_err(|e| e.to_string())
                .and_then(|js| {
                    serde_json::from_str::<NpeProgressRecord>(&js).map_err(|e| e.to_string())
                }) {
                Ok(npe) => {
                    snap.npe_progress.insert(npe.character_id, npe);
                }
                Err(err) => {
                    ctx.record_bad_row("npe_progress", "npe_progress", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, bookmark_id, owner_id, label, solar_system_id, x, y, z, item_id, memo, folder_id, is_shared FROM bookmarks")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_payload_summary(r, 1, 12);
            match (|| -> Result<BookmarkRecord, String> {
                Ok(BookmarkRecord {
                    bookmark_id: r.get(1).map_err(|e| e.to_string())?,
                    owner_id: r.get(2).map_err(|e| e.to_string())?,
                    label: r.get(3).map_err(|e| e.to_string())?,
                    solar_system_id: r.get::<_, i64>(4).map_err(|e| e.to_string())?
                        as SolarSystemId,
                    x: r.get(5).map_err(|e| e.to_string())?,
                    y: r.get(6).map_err(|e| e.to_string())?,
                    z: r.get(7).map_err(|e| e.to_string())?,
                    item_id: r.get(8).map_err(|e| e.to_string())?,
                    memo: r
                        .get::<_, Option<String>>(9)
                        .map_err(|e| e.to_string())?
                        .unwrap_or_default(),
                    folder_id: r.get(10).map_err(|e| e.to_string())?,
                    is_shared: r.get::<_, i64>(11).map_err(|e| e.to_string())? != 0,
                })
            })() {
                Ok(bm) => {
                    snap.bookmarks.insert(bm.bookmark_id, bm);
                }
                Err(err) => {
                    ctx.record_bad_row("bookmarks", "bookmarks", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, message_id, sender_id, recipient_id, subject, body FROM mail ORDER BY seq_idx ASC")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_payload_summary(r, 1, 6);
            match (|| -> Result<MailRecord, String> {
                Ok(MailRecord {
                    message_id: r.get(1).map_err(|e| e.to_string())?,
                    sender_id: r.get(2).map_err(|e| e.to_string())?,
                    recipient_id: r.get(3).map_err(|e| e.to_string())?,
                    subject: r.get(4).map_err(|e| e.to_string())?,
                    body: r.get(5).map_err(|e| e.to_string())?,
                })
            })() {
                Ok(m) => snap.mails.push(m),
                Err(err) => {
                    ctx.record_bad_row("mail", "mail", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, fleet_id, record_json FROM fleets")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 2);
            match r
                .get::<_, String>(2)
                .map_err(|e| e.to_string())
                .and_then(|js| serde_json::from_str::<FleetRecord>(&js).map_err(|e| e.to_string()))
            {
                Ok(fl) => {
                    snap.fleets.insert(fl.fleet_id, fl);
                }
                Err(err) => {
                    ctx.record_bad_row("fleets", "fleets", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, corporation_id, record_json FROM corps")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 2);
            match r
                .get::<_, String>(2)
                .map_err(|e| e.to_string())
                .and_then(|js| {
                    serde_json::from_str::<CorporationRecord>(&js).map_err(|e| e.to_string())
                }) {
                Ok(mut corp) => {
                    corp.ensure_wallet_divisions();
                    snap.corporations.insert(corp.corporation_id, corp);
                }
                Err(err) => {
                    ctx.record_bad_row("corps", "corps", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, alliance_id, record_json FROM alliances")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 2);
            match r
                .get::<_, String>(2)
                .map_err(|e| e.to_string())
                .and_then(|js| {
                    serde_json::from_str::<AllianceRecord>(&js).map_err(|e| e.to_string())
                }) {
                Ok(al) => {
                    snap.alliances.insert(al.alliance_id, al);
                }
                Err(err) => {
                    ctx.record_bad_row("alliances", "alliances", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, war_id, aggressor_corp_id, defender_corp_id, mutual, started_unix, active FROM wars")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_payload_summary(r, 1, 7);
            match (|| -> Result<WarRecord, String> {
                Ok(WarRecord {
                    war_id: r.get(1).map_err(|e| e.to_string())?,
                    aggressor_corp_id: r.get(2).map_err(|e| e.to_string())?,
                    defender_corp_id: r.get(3).map_err(|e| e.to_string())?,
                    mutual: r.get::<_, i64>(4).map_err(|e| e.to_string())? != 0,
                    started_unix: r.get::<_, i64>(5).map_err(|e| e.to_string())? as u64,
                    active: r.get::<_, i64>(6).map_err(|e| e.to_string())? != 0,
                })
            })() {
                Ok(w) => {
                    snap.wars.insert(w.war_id, w);
                }
                Err(err) => {
                    ctx.record_bad_row("wars", "wars", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, project_id, corporation_id, name, target_quantity, current_quantity, isk_per_unit_cents, escrow_remaining_cents FROM corp_projects")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_payload_summary(r, 1, 8);
            match (|| -> Result<CorpProjectRecord, String> {
                Ok(CorpProjectRecord {
                    project_id: r.get(1).map_err(|e| e.to_string())?,
                    corporation_id: r.get(2).map_err(|e| e.to_string())?,
                    name: r.get(3).map_err(|e| e.to_string())?,
                    target_quantity: r.get::<_, i64>(4).map_err(|e| e.to_string())? as u64,
                    current_quantity: r.get::<_, i64>(5).map_err(|e| e.to_string())? as u64,
                    isk_per_unit: IskCents(r.get(6).map_err(|e| e.to_string())?),
                    escrow_remaining: IskCents(r.get(7).map_err(|e| e.to_string())?),
                })
            })() {
                Ok(p) => {
                    snap.corp_projects.insert(p.project_id, p);
                }
                Err(err) => {
                    ctx.record_bad_row("corp_projects", "corp_projects", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, kill_id, victim_character_id, attacker_character_id, solar_system_id, ship_type_id, total_value_isk, killed_unix, victim_corp_id, victim_alliance_id, attacker_corp_id, attacker_alliance_id FROM killmails")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_payload_summary(r, 1, 12);
            match (|| -> Result<KillmailRecord, String> {
                Ok(KillmailRecord {
                    kill_id: r.get(1).map_err(|e| e.to_string())?,
                    victim_character_id: r.get(2).map_err(|e| e.to_string())?,
                    attacker_character_id: r.get(3).map_err(|e| e.to_string())?,
                    solar_system_id: r.get::<_, i64>(4).map_err(|e| e.to_string())?
                        as SolarSystemId,
                    ship_type_id: r.get::<_, i64>(5).map_err(|e| e.to_string())? as TypeId,
                    total_value_isk: r.get(6).map_err(|e| e.to_string())?,
                    killed_unix: r.get::<_, i64>(7).map_err(|e| e.to_string())? as u64,
                    victim_corp_id: r.get(8).map_err(|e| e.to_string())?,
                    victim_alliance_id: r.get(9).map_err(|e| e.to_string())?,
                    attacker_corp_id: r.get(10).map_err(|e| e.to_string())?,
                    attacker_alliance_id: r.get(11).map_err(|e| e.to_string())?,
                })
            })() {
                Ok(km) => {
                    snap.killmails.insert(km.kill_id, km);
                }
                Err(err) => {
                    ctx.record_bad_row("killmails", "killmails", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, kill_right_id, owner_id, target_id, activation_fee_cents, is_active_suspect FROM kill_rights")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_payload_summary(r, 1, 6);
            match (|| -> Result<KillRightRecord, String> {
                Ok(KillRightRecord {
                    kill_right_id: r.get(1).map_err(|e| e.to_string())?,
                    owner_id: r.get(2).map_err(|e| e.to_string())?,
                    target_id: r.get(3).map_err(|e| e.to_string())?,
                    activation_fee: IskCents(r.get(4).map_err(|e| e.to_string())?),
                    is_active_suspect: r.get::<_, i64>(5).map_err(|e| e.to_string())? != 0,
                })
            })() {
                Ok(kr) => {
                    snap.kill_rights.insert(kr.kill_right_id, kr);
                }
                Err(err) => {
                    ctx.record_bad_row("kill_rights", "kill_rights", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, structure_id, record_json FROM structures")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 2);
            match r
                .get::<_, String>(2)
                .map_err(|e| e.to_string())
                .and_then(|js| {
                    serde_json::from_str::<UpwellStructureRecord>(&js).map_err(|e| e.to_string())
                }) {
                Ok(st) => {
                    snap.structures.insert(st.structure_id, st);
                }
                Err(err) => {
                    ctx.record_bad_row("structures", "structures", key, payload, err, rowid)?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, solar_system_id, record_json FROM system_states")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 2);
            match (|| -> Result<(SolarSystemId, SystemDynamicState), String> {
                let sid: i64 = r.get(1).map_err(|e| e.to_string())?;
                let js: String = r.get(2).map_err(|e| e.to_string())?;
                let mut st: SystemDynamicState =
                    serde_json::from_str(&js).map_err(|e| e.to_string())?;
                let sys_id = sid as SolarSystemId;
                st.solar_system_id = sys_id;
                Ok((sys_id, st))
            })() {
                Ok((sys_id, st)) => {
                    snap.system_states.insert(sys_id, st);
                }
                Err(err) => {
                    ctx.record_bad_row(
                        "system_states",
                        "system_states",
                        key,
                        payload,
                        err,
                        rowid,
                    )?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, character_id, idem_key FROM idempotency_keys")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_composite_key_string(r, 1, 2);
            let payload = row_payload_summary(r, 1, 3);
            match (|| -> Result<(i64, String), String> {
                Ok((
                    r.get(1).map_err(|e| e.to_string())?,
                    r.get(2).map_err(|e| e.to_string())?,
                ))
            })() {
                Ok(pair) => {
                    snap.committed_idempotency_keys.insert(pair);
                }
                Err(err) => {
                    ctx.record_bad_row(
                        "idempotency_keys",
                        "idempotency_keys",
                        key,
                        payload,
                        err,
                        rowid,
                    )?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, character_id, skins_json FROM licensed_skins")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 2);
            match (|| -> Result<(i64, Vec<u32>), String> {
                let cid: i64 = r.get(1).map_err(|e| e.to_string())?;
                let js: String = r.get(2).map_err(|e| e.to_string())?;
                let skins: Vec<u32> = serde_json::from_str(&js).map_err(|e| e.to_string())?;
                Ok((cid, skins))
            })() {
                Ok((cid, skins)) => {
                    snap.licensed_skins.insert(cid, skins);
                }
                Err(err) => {
                    ctx.record_bad_row(
                        "licensed_skins",
                        "licensed_skins",
                        key,
                        payload,
                        err,
                        rowid,
                    )?;
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT rowid, ship_id, skin_id FROM applied_ship_skins")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_payload_summary(r, 1, 3);
            match (|| -> Result<(i64, u32), String> {
                Ok((
                    r.get(1).map_err(|e| e.to_string())?,
                    r.get::<_, i64>(2).map_err(|e| e.to_string())? as u32,
                ))
            })() {
                Ok((sid, skin_id)) => {
                    snap.applied_ship_skins.insert(sid, skin_id);
                }
                Err(err) => {
                    ctx.record_bad_row(
                        "applied_ship_skins",
                        "applied_ship_skins",
                        key,
                        payload,
                        err,
                        rowid,
                    )?;
                }
            }
        }
    }

    if let Ok(mut stmt) = conn.prepare(
        "SELECT rowid, character_id, solar_system_id, type_id, quantity, date, timestamp_unix FROM char_mining_ledger",
    ) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = row_col_string(r, 1);
                let payload = row_payload_summary(r, 1, 7);
                match (|| -> Result<MiningLedgerRecord, String> {
                    Ok(MiningLedgerRecord {
                        character_id: r.get(1).map_err(|e| e.to_string())?,
                        solar_system_id: r.get::<_, i64>(2).map_err(|e| e.to_string())? as u32,
                        type_id: r.get::<_, i64>(3).map_err(|e| e.to_string())? as u32,
                        quantity: r.get(4).map_err(|e| e.to_string())?,
                        date: r.get(5).map_err(|e| e.to_string())?,
                        timestamp_unix: r.get::<_, i64>(6).map_err(|e| e.to_string())? as u64,
                    })
                })() {
                    Ok(rec) => snap.mining_ledger.push(rec),
                    Err(err) => {
                        ctx.record_bad_row(
                            "char_mining_ledger",
                            "char_mining_ledger",
                            key,
                            payload,
                            err,
                            rowid,
                        )?;
                    }
                }
            }
        }
    }

    if let Ok(mut stmt) = conn.prepare(
        "SELECT rowid, asteroid_id, solar_system_id, belt_id, type_id, quantity, initial_quantity, x, y, z, radius FROM asteroids",
    ) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = row_col_string(r, 1);
                let payload = row_payload_summary(r, 1, 11);
                match (|| -> Result<AsteroidRecord, String> {
                    Ok(AsteroidRecord {
                        asteroid_id: r.get(1).map_err(|e| e.to_string())?,
                        solar_system_id: r.get(2).map_err(|e| e.to_string())?,
                        belt_id: r.get(3).map_err(|e| e.to_string())?,
                        type_id: r.get(4).map_err(|e| e.to_string())?,
                        quantity: r.get(5).map_err(|e| e.to_string())?,
                        initial_quantity: r.get(6).map_err(|e| e.to_string())?,
                        position: (
                            r.get(7).map_err(|e| e.to_string())?,
                            r.get(8).map_err(|e| e.to_string())?,
                            r.get(9).map_err(|e| e.to_string())?,
                        ),
                        radius: r.get(10).map_err(|e| e.to_string())?,
                    })
                })() {
                    Ok(rec) => {
                        snap.asteroids.insert(rec.asteroid_id, rec);
                    }
                    Err(err) => {
                        ctx.record_bad_row("asteroids", "asteroids", key, payload, err, rowid)?;
                    }
                }
            }
        }
    }

    if let Ok(mut stmt) = conn.prepare(
        "SELECT rowid, character_id, last_jump_unix FROM character_last_jump_unix",
    ) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = row_col_string(r, 1);
                let payload = row_payload_summary(r, 1, 3);
                match (|| -> Result<(i64, u64), String> {
                    Ok((
                        r.get(1).map_err(|e| e.to_string())?,
                        r.get::<_, i64>(2).map_err(|e| e.to_string())? as u64,
                    ))
                })() {
                    Ok((cid, ts)) => {
                        snap.character_last_jump_unix.insert(cid, ts);
                    }
                    Err(err) => {
                        ctx.record_bad_row(
                            "character_last_jump_unix",
                            "character_last_jump_unix",
                            key,
                            payload,
                            err,
                            rowid,
                        )?;
                    }
                }
            }
        }
    }

    if let Ok(mut stmt) = conn.prepare(
        "SELECT rowid, character_id, cspa_charge_cents FROM character_cspa_charges",
    ) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = row_col_string(r, 1);
                let payload = row_payload_summary(r, 1, 3);
                match (|| -> Result<(i64, IskCents), String> {
                    Ok((
                        r.get(1).map_err(|e| e.to_string())?,
                        IskCents(r.get(2).map_err(|e| e.to_string())?),
                    ))
                })() {
                    Ok((cid, charge)) => {
                        snap.character_cspa_charges.insert(cid, charge);
                    }
                    Err(err) => {
                        ctx.record_bad_row(
                            "character_cspa_charges",
                            "character_cspa_charges",
                            key,
                            payload,
                            err,
                            rowid,
                        )?;
                    }
                }
            }
        }
    }

    if let Ok(mut stmt) =
        conn.prepare("SELECT rowid, mail_id, timestamp_unix FROM mail_timestamps")
    {
        if let Ok(mut rows) = stmt.query([]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = row_col_string(r, 1);
                let payload = row_payload_summary(r, 1, 3);
                match (|| -> Result<(i64, u64), String> {
                    Ok((
                        r.get(1).map_err(|e| e.to_string())?,
                        r.get::<_, i64>(2).map_err(|e| e.to_string())? as u64,
                    ))
                })() {
                    Ok((mid, ts)) => {
                        snap.mail_timestamps.insert(mid, ts);
                    }
                    Err(err) => {
                        ctx.record_bad_row(
                            "mail_timestamps",
                            "mail_timestamps",
                            key,
                            payload,
                            err,
                            rowid,
                        )?;
                    }
                }
            }
        }
    }

    if let Ok(mut stmt) = conn.prepare(
        "SELECT rowid, character_id, last_login_unix, last_logout_unix, logins_count FROM character_login_stats",
    ) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = row_col_string(r, 1);
                let payload = row_payload_summary(r, 1, 5);
                match (|| -> Result<(i64, CharacterLoginStats), String> {
                    Ok((
                        r.get(1).map_err(|e| e.to_string())?,
                        CharacterLoginStats {
                            last_login_unix: r.get::<_, i64>(2).map_err(|e| e.to_string())? as u64,
                            last_logout_unix: r.get::<_, i64>(3).map_err(|e| e.to_string())? as u64,
                            logins_count: r.get::<_, i64>(4).map_err(|e| e.to_string())? as u64,
                        },
                    ))
                })() {
                    Ok((cid, stats)) => {
                        snap.character_login_stats.insert(cid, stats);
                    }
                    Err(err) => {
                        ctx.record_bad_row(
                            "character_login_stats",
                            "character_login_stats",
                            key,
                            payload,
                            err,
                            rowid,
                        )?;
                    }
                }
            }
        }
    }

    if let Ok(mut stmt) =
        conn.prepare("SELECT rowid, item_id, custom_name FROM item_custom_names")
    {
        if let Ok(mut rows) = stmt.query([]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = row_col_string(r, 1);
                let payload = row_payload_summary(r, 1, 3);
                match (|| -> Result<(i64, String), String> {
                    Ok((
                        r.get(1).map_err(|e| e.to_string())?,
                        r.get(2).map_err(|e| e.to_string())?,
                    ))
                })() {
                    Ok((iid, name)) => {
                        snap.item_custom_names.insert(iid, name);
                    }
                    Err(err) => {
                        ctx.record_bad_row(
                            "item_custom_names",
                            "item_custom_names",
                            key,
                            payload,
                            err,
                            rowid,
                        )?;
                    }
                }
            }
        }
    }

    if let Ok(mut stmt) = conn.prepare("SELECT rowid, item_id, x, y, z FROM item_positions") {
        if let Ok(mut rows) = stmt.query([]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = row_col_string(r, 1);
                let payload = row_payload_summary(r, 1, 5);
                match (|| -> Result<(i64, [f64; 3]), String> {
                    Ok((
                        r.get(1).map_err(|e| e.to_string())?,
                        [
                            r.get(2).map_err(|e| e.to_string())?,
                            r.get(3).map_err(|e| e.to_string())?,
                            r.get(4).map_err(|e| e.to_string())?,
                        ],
                    ))
                })() {
                    Ok((iid, pos)) => {
                        snap.item_positions.insert(iid, pos);
                    }
                    Err(err) => {
                        ctx.record_bad_row(
                            "item_positions",
                            "item_positions",
                            key,
                            payload,
                            err,
                            rowid,
                        )?;
                    }
                }
            }
        }
    }

    if let Ok(mut stmt) = conn.prepare(
        "SELECT rowid, structure_id, window_start_unix, window_end_unix FROM structure_vulnerability_windows",
    ) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = row_col_string(r, 1);
                let payload = row_payload_summary(r, 1, 4);
                match (|| -> Result<(i64, (u64, u64)), String> {
                    Ok((
                        r.get(1).map_err(|e| e.to_string())?,
                        (
                            r.get::<_, i64>(2).map_err(|e| e.to_string())? as u64,
                            r.get::<_, i64>(3).map_err(|e| e.to_string())? as u64,
                        ),
                    ))
                })() {
                    Ok((sid, win)) => {
                        snap.structure_vulnerability_windows.insert(sid, win);
                    }
                    Err(err) => {
                        ctx.record_bad_row(
                            "structure_vulnerability_windows",
                            "structure_vulnerability_windows",
                            key,
                            payload,
                            err,
                            rowid,
                        )?;
                    }
                }
            }
        }
    }

    if let Ok(mut stmt) =
        conn.prepare("SELECT rowid, character_id, history_json FROM skill_history")
    {
        if let Ok(mut rows) = stmt.query([]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = row_col_string(r, 1);
                let payload = row_col_string(r, 2);
                match (|| -> Result<(i64, Vec<SkillHistoryEntry>), String> {
                    let cid: i64 = r.get(1).map_err(|e| e.to_string())?;
                    let js: String = r.get(2).map_err(|e| e.to_string())?;
                    let hist: Vec<SkillHistoryEntry> =
                        serde_json::from_str(&js).map_err(|e| e.to_string())?;
                    Ok((cid, hist))
                })() {
                    Ok((cid, hist)) => {
                        snap.skill_history.insert(cid, hist);
                    }
                    Err(err) => {
                        ctx.record_bad_row(
                            "skill_history",
                            "skill_history",
                            key,
                            payload,
                            err,
                            rowid,
                        )?;
                    }
                }
            }
        }
    }

    if let Ok(mut stmt) = conn.prepare(
        "SELECT rowid, character_id, doll_dna, portrait_meta FROM character_doll_info",
    ) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = row_col_string(r, 1);
                let payload = row_payload_summary(r, 1, 4);
                match (|| -> Result<(i64, (String, String)), String> {
                    Ok((
                        r.get(1).map_err(|e| e.to_string())?,
                        (
                            r.get(2).map_err(|e| e.to_string())?,
                            r.get(3).map_err(|e| e.to_string())?,
                        ),
                    ))
                })() {
                    Ok((cid, info)) => {
                        snap.character_doll_info.insert(cid, info);
                    }
                    Err(err) => {
                        ctx.record_bad_row(
                            "character_doll_info",
                            "character_doll_info",
                            key,
                            payload,
                            err,
                            rowid,
                        )?;
                    }
                }
            }
        }
    }

    let had_legacy_ledger =
        super::domain_rows::load_ledger_domain_rows(conn, &mut snap, &mut ctx)?;
    super::notes_lp_sequence_store::load_d6_checked(conn, &mut snap, &mut ctx)?;
    super::social_store::load_d6_social_into_snapshot_with_ctx(conn, &mut snap, &mut ctx)?;
    super::corp_crimewatch_store::load_d6_corp_crimewatch_checked(conn, &mut snap, &mut ctx)?;
    super::industry_market_pve_store::load_d6_batch4_checked(conn, &mut snap, &mut ctx)?;
    super::service_domains_store::load_service_domains_checked(conn, &mut snap, &mut ctx)?;
    ctx.flush_quarantined_rows(conn)?;

    let mut pre_migration_fp = compute_fingerprints(&snap);
    // Run startup snapshot migrations BEFORE computing baseline fingerprints and flush any
    // migrated rows immediately into SQLite so neither clean reads nor first-transaction rollbacks
    // revert `next_entity_id`, `account_plex_vault`, `service_domains`, or `fw_system_occupations`.
    snap.fold_legacy_plex_balances();
    snap.migrate_legacy_service_state();
    snap.reconcile_id_counters();
    snap.backfill_fw_system_occupations();
    for (&sid, st) in &mut snap.system_states {
        st.solar_system_id = sid;
    }
    if conn.is_autocommit() && !conn.is_readonly(rusqlite::DatabaseName::Main).unwrap_or(false) {
        if had_legacy_ledger {
            super::domain_rows::apply_domain_rows_migration(conn)?;
        }
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let delta = super::sync::sync_dirty_to_sqlite(&tx, &snap, &mut pre_migration_fp, false)?;
        if had_legacy_ledger {
            super::domain_rows::drop_legacy_ledger_tables(&tx)?;
        }
        if delta.dirty || had_legacy_ledger {
            tx.commit().map_err(|e| e.to_string())?;
        }
    }
    let fp = compute_fingerprints(&snap);
    Ok((snap, fp))
}
#[cfg(test)] #[path = "load_unit_tests.rs"] mod load_unit_tests;
