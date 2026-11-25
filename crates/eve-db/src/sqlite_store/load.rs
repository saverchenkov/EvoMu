//! Relational SQLite world state loader.

use crate::missions::{
    decide_mission_kind, lookup_sde_mission_and_agent, open_sde_sqlite_ro, CourierObjective,
    DeliveryObjective, EncounterObjective, MissionHistoryEntry, MissionInstance, MissionKind,
    MissionObjective, MissionState,
};
use crate::snapshot_ext::{decode_owner_note_entity_id, LpRewardLogEntry};
use crate::sqlite_store::domain_rows::{
    table_exists, DOMAIN_CHAR_LP_LOGS, DOMAIN_MARKET_TRANSACTIONS, DOMAIN_OWNER_NOTES,
    DOMAIN_SOCIAL_BOOKMARKS, DOMAIN_SOCIAL_CHAT_CALENDAR, DOMAIN_SOCIAL_MAIL,
    DOMAIN_SP6_LIFECYCLE, DOMAIN_WALLET_JOURNAL_META,
};
use crate::sqlite_store::quarantine::{
    default_quarantine_unreadable, row_col_string, row_composite_key_string, row_payload_summary,
    LoadContext,
};
use crate::types::*;
use eve_sde::{SolarSystemId, TypeId};
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};

/// Loads the complete `WorldStateSnapshot` from the SQLite database,
/// failing closed on any corrupted row unless `--quarantine-unreadable` is active.
pub fn load_snapshot_from_sqlite(
    conn: &Connection,
) -> Result<WorldStateSnapshot, String> {
    load_snapshot_from_sqlite_with_options(conn, default_quarantine_unreadable())
}

const ENTITY_ID_SQLITE_TABLES: &[(&str, &str)] = &[
    ("characters", "character_id"),
    ("items", "item_id"),
    ("structures", "structure_id"),
    ("saved_fittings", "fitting_id"),
    ("market_orders", "order_id"),
    ("wallet_journal", "entry_id"),
    ("contracts", "contract_id"),
    ("industry_jobs", "job_id"),
    ("mission_instances", "instance_id"),
    ("mission_history", "instance_id"),
    ("bookmarks", "bookmark_id"),
    ("mail", "message_id"),
    ("mail_timestamps", "mail_id"),
    ("fleets", "fleet_id"),
    ("corps", "corporation_id"),
    ("alliances", "alliance_id"),
    ("wars", "war_id"),
    ("corp_projects", "project_id"),
    ("killmails", "kill_id"),
    ("kill_rights", "kill_right_id"),
    ("asteroids", "asteroid_id"),
    ("bookmark_meta", "bookmark_id"),
    ("bookmark_folders", "folder_id"),
    ("mailing_lists", "list_id"),
    ("calendar_events", "event_id"),
    ("access_groups", "group_id"),
    ("item_custom_names", "item_id"),
    ("item_positions", "item_id"),
    ("applied_ship_skins", "ship_id"),
    ("entity_kv", "entity_id"),
    ("structure_vulnerability_windows", "structure_id"),
];

/// Queries the cross-table entity high-water mark (`< 900_000_000`) directly across SQLite
/// ID-bearing tables (`#1087`).
pub fn query_cross_table_entity_high_water(conn: &Connection) -> Result<i64, String> {
    let mut hwm = 200_000_000_i64;
    for &(table, col) in ENTITY_ID_SQLITE_TABLES {
        let sql = format!(
            "SELECT COALESCE(MAX({col}), 0) FROM {table} WHERE {col} > 0 AND {col} < 900000000"
        );
        if let Ok(mut stmt) = conn.prepare_cached(&sql) {
            if let Ok(val) = stmt.query_row([], |r| r.get::<_, i64>(0)) {
                if val > hwm {
                    hwm = val;
                }
            }
        }
    }
    if let Ok(mut stmt) = conn.prepare_cached(
        "SELECT key FROM domain_rows WHERE domain IN (?1, ?2)",
    ) {
        if let Ok(mut rows) = stmt.query(rusqlite::params![
            DOMAIN_MARKET_TRANSACTIONS,
            DOMAIN_WALLET_JOURNAL_META
        ]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let key_str: String = r.get(0).map_err(|e| e.to_string())?;
                if let Ok(val) = key_str.parse::<i64>() {
                    if val > hwm && val < 900_000_000 {
                        hwm = val;
                    }
                }
            }
        }
    }
    if let Ok(mut stmt) = conn.prepare_cached(
        "SELECT key, value FROM domain_rows WHERE domain = ?1",
    ) {
        if let Ok(mut rows) = stmt.query(rusqlite::params![DOMAIN_OWNER_NOTES]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let key_str: String = r.get(0).map_err(|e| e.to_string())?;
                let val_str: String = r.get(1).map_err(|e| e.to_string())?;
                let entity_id = serde_json::from_str::<OwnerNoteRecord>(&val_str)
                    .ok()
                    .as_ref()
                    .and_then(decode_owner_note_entity_id)
                    .or_else(|| key_str.parse::<i64>().ok().filter(|&id| id > 0));
                if let Some(val) = entity_id {
                    if val > hwm && val < 900_000_000 {
                        hwm = val;
                    }
                }
            }
        }
    }
    if let Ok(mut stmt) = conn.prepare_cached(
        "SELECT value FROM domain_rows WHERE domain = ?1",
    ) {
        if let Ok(mut rows) = stmt.query(rusqlite::params![DOMAIN_CHAR_LP_LOGS]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let val_str: String = r.get(0).map_err(|e| e.to_string())?;
                if let Ok(entries) = serde_json::from_str::<Vec<LpRewardLogEntry>>(&val_str) {
                    for entry in entries {
                        if entry.log_id > hwm && entry.log_id < 900_000_000 {
                            hwm = entry.log_id;
                        }
                    }
                }
            }
        }
    }
    if let Ok(mut stmt) = conn.prepare_cached(
        "SELECT domain, key FROM domain_rows WHERE domain IN (?1, ?2, ?3)",
    ) {
        if let Ok(mut rows) = stmt.query(rusqlite::params![
            DOMAIN_SOCIAL_MAIL,
            DOMAIN_SOCIAL_BOOKMARKS,
            DOMAIN_SOCIAL_CHAT_CALENDAR,
        ]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let domain_str: String = r.get(0).map_err(|e| e.to_string())?;
                let key_str: String = r.get(1).map_err(|e| e.to_string())?;
                let Some((field, suffix)) = key_str.split_once('/') else {
                    continue;
                };
                let is_id_field = match domain_str.as_str() {
                    DOMAIN_SOCIAL_MAIL => field == "mailing_lists",
                    DOMAIN_SOCIAL_CHAT_CALENDAR => field == "calendar_events",
                    DOMAIN_SOCIAL_BOOKMARKS => matches!(
                        field,
                        "bookmark_folders"
                            | "bookmark_subfolders"
                            | "bookmark_expiries"
                            | "access_groups"
                    ),
                    _ => false,
                };
                if is_id_field {
                    if let Ok(val) = suffix.parse::<i64>() {
                        if val > hwm && val < 900_000_000 {
                            hwm = val;
                        }
                    }
                }
            }
        }
    }
    Ok(hwm)
}

/// Returns `true` if `candidate` exists in any relational SQLite ID-bearing table (`#1087`).
pub fn sqlite_entity_id_exists(conn: &Connection, candidate: i64) -> Result<bool, String> {
    if candidate <= 0 {
        return Ok(false);
    }
    for &(table, col) in ENTITY_ID_SQLITE_TABLES {
        let sql = format!("SELECT 1 FROM {table} WHERE {col} = ?1 LIMIT 1");
        if let Ok(mut stmt) = conn.prepare_cached(&sql) {
            let mut rows = stmt
                .query(rusqlite::params![candidate])
                .map_err(|e| e.to_string())?;
            if rows.next().map_err(|e| e.to_string())?.is_some() {
                return Ok(true);
            }
        }
    }
    let cand_key = candidate.to_string();
    if let Ok(mut stmt) = conn.prepare_cached(
        "SELECT 1 FROM domain_rows WHERE domain IN (?1, ?2, ?3) AND key = ?4 LIMIT 1",
    ) {
        let mut rows = stmt
            .query(rusqlite::params![
                DOMAIN_MARKET_TRANSACTIONS,
                DOMAIN_WALLET_JOURNAL_META,
                DOMAIN_OWNER_NOTES,
                cand_key
            ])
            .map_err(|e| e.to_string())?;
        if rows.next().map_err(|e| e.to_string())?.is_some() {
            return Ok(true);
        }
    }
    if let Ok(mut stmt) = conn.prepare_cached(
        "SELECT 1 FROM domain_rows WHERE domain = ?1 AND key = ?2 LIMIT 1",
    ) {
        if let Some(notif_key) = (-7_900_000_000_000_i64).checked_sub(candidate) {
            let mut rows = stmt
                .query(rusqlite::params![DOMAIN_OWNER_NOTES, notif_key.to_string()])
                .map_err(|e| e.to_string())?;
            if rows.next().map_err(|e| e.to_string())?.is_some() {
                return Ok(true);
            }
        }
        if let Some(report_key) = 780_000_000_i64.checked_add(candidate) {
            let mut rows = stmt
                .query(rusqlite::params![DOMAIN_OWNER_NOTES, report_key.to_string()])
                .map_err(|e| e.to_string())?;
            if rows.next().map_err(|e| e.to_string())?.is_some() {
                return Ok(true);
            }
        }
    }
    if let Ok(mut stmt) = conn.prepare_cached(
        "SELECT 1 FROM domain_rows
         WHERE (domain = ?1 AND key = ?2)
            OR (domain = ?3 AND key = ?4)
            OR (domain = ?5 AND key IN (?6, ?7, ?8, ?9))
         LIMIT 1",
    ) {
        let mut rows = stmt
            .query(rusqlite::params![
                DOMAIN_SOCIAL_MAIL,
                format!("mailing_lists/{candidate}"),
                DOMAIN_SOCIAL_CHAT_CALENDAR,
                format!("calendar_events/{candidate}"),
                DOMAIN_SOCIAL_BOOKMARKS,
                format!("bookmark_folders/{candidate}"),
                format!("bookmark_subfolders/{candidate}"),
                format!("bookmark_expiries/{candidate}"),
                format!("access_groups/{candidate}"),
            ])
            .map_err(|e| e.to_string())?;
        if rows.next().map_err(|e| e.to_string())?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Loads the persisted `next_entity_block` high-water mark from `world_meta` (falling back to
/// `next_entity_id`, the cross-table SQLite high-water mark, and the minimum `200_000_000` floor).
pub fn load_entity_block_from_sqlite(conn: &Connection) -> Result<i64, String> {
    let mut block = query_cross_table_entity_high_water(conn)?;
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

/// Loads the complete `WorldStateSnapshot` from the SQLite database.
/// When `quarantine_unreadable` is `false`, any unreadable row returns `Err` naming the table and key.
/// When `quarantine_unreadable` is `true`, offending rows are moved into `quarantine_rows` and logged.
pub fn load_snapshot_from_sqlite_with_options(
    conn: &Connection,
    quarantine_unreadable: bool,
) -> Result<WorldStateSnapshot, String> {
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
                Ok(mut item) => {
                    // Multi-unit stacks of plain items must never be marked singleton;
                    // repair legacy persisted starter stacks (e.g., #1551 item 100_000_004) on load.
                    if !item.is_blueprint_copy
                        && item.quantity > 1
                        && item.singleton
                        && item.blueprint_runs_remaining.is_none()
                        && item.dynamic_attributes.is_empty()
                        && item.source_mutaplasmid_id.is_none()
                    {
                        item.singleton = false;
                    }
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

    let mut upgraded_mission_rows = false;
    if table_exists(conn, "mission_instances")? || !table_exists(conn, "missions")? {
        let mut stmt = conn
            .prepare(
                "SELECT rowid, instance_id, character_id, state, record_json FROM mission_instances ORDER BY instance_id ASC",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 4);
            match (|| -> Result<MissionInstance, String> {
                let instance_id = r.get::<_, i64>(1).map_err(|e| e.to_string())?;
                let col_char_id = r.get::<_, i64>(2).map_err(|e| e.to_string())?;
                let col_state = r.get::<_, String>(3).map_err(|e| e.to_string())?;
                let parsed_col_state = MissionState::parse(&col_state)
                    .ok_or_else(|| format!("unknown mission state '{col_state}'"))?;
                let js = r.get::<_, String>(4).map_err(|e| e.to_string())?;
                let mut inst: MissionInstance =
                    serde_json::from_str(&js).map_err(|e| e.to_string())?;
                if inst.instance_id != instance_id
                    || inst.character_id != col_char_id
                    || inst.state != parsed_col_state
                {
                    return Err(format!(
                        "mission_instances row column/json mismatch for instance_id {instance_id}"
                    ));
                }
                if inst.kind == MissionKind::Heraldry
                    && matches!(inst.objective, MissionObjective::None)
                {
                    inst.objective = MissionObjective::Delivery(DeliveryObjective::default());
                    upgraded_mission_rows = true;
                }
                Ok(inst)
            })() {
                Ok(inst) => {
                    if !inst.state.is_active() {
                        let ended_at = inst.accepted_at_unix.unwrap_or(inst.offered_at_unix);
                        let entry = inst.to_history_entry(inst.state, ended_at);
                        upgraded_mission_rows = true;
                        if let Err(err) = snap.missions.record_history(entry) {
                            ctx.record_bad_row(
                                "mission_instances",
                                "mission_instances",
                                key,
                                payload,
                                err,
                                rowid,
                            )?;
                        }
                    } else if let Err(err) = snap.missions.try_insert(inst) {
                        ctx.record_bad_row(
                            "mission_instances",
                            "mission_instances",
                            key,
                            payload,
                            err,
                            rowid,
                        )?;
                    }
                }
                Err(err) => {
                    ctx.record_bad_row(
                        "mission_instances",
                        "mission_instances",
                        key,
                        payload,
                        err,
                        rowid,
                    )?;
                }
            }
        }
    }

    let has_mission_history_table = table_exists(conn, "mission_history")?;
    if has_mission_history_table {
        let mut stmt = conn
            .prepare(
                "SELECT rowid, instance_id, character_id, final_state, record_json FROM mission_history ORDER BY instance_id ASC",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = row_col_string(r, 1);
            let payload = row_col_string(r, 4);
            match (|| -> Result<MissionHistoryEntry, String> {
                let instance_id = r.get::<_, i64>(1).map_err(|e| e.to_string())?;
                let col_char_id = r.get::<_, i64>(2).map_err(|e| e.to_string())?;
                let col_state = r.get::<_, String>(3).map_err(|e| e.to_string())?;
                let parsed_col_state = MissionState::parse(&col_state)
                    .ok_or_else(|| format!("unknown mission history state '{col_state}'"))?;
                let js = r.get::<_, String>(4).map_err(|e| e.to_string())?;
                let entry: MissionHistoryEntry =
                    serde_json::from_str(&js).map_err(|e| e.to_string())?;
                if entry.instance_id != instance_id
                    || entry.character_id != col_char_id
                    || entry.final_state != parsed_col_state
                {
                    return Err(format!(
                        "mission_history row column/json mismatch for instance_id {instance_id}"
                    ));
                }
                Ok(entry)
            })() {
                Ok(entry) => {
                    if let Err(err) = snap.missions.record_history(entry) {
                        ctx.record_bad_row(
                            "mission_history",
                            "mission_history",
                            key,
                            payload,
                            err,
                            rowid,
                        )?;
                    }
                }
                Err(err) => {
                    ctx.record_bad_row(
                        "mission_history",
                        "mission_history",
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
    let had_legacy_notes_lp =
        super::domain_rows::load_notes_lp_domain_rows(conn, &mut snap, &mut ctx)?;
    let (legacy_sp6_waves, had_legacy_sp6_mission_rows) =
        read_legacy_sp6_mission_waves(conn, &mut ctx)?;
    let had_legacy_sp6_lifecycle =
        super::domain_rows::load_sp6_lifecycle_domain_rows(conn, &mut snap, &mut ctx)?;
    super::notes_lp_sequence_store::load_d6_checked(conn, &mut snap, &mut ctx)?;
    let had_legacy_social =
        super::social_store::load_social_domain_rows(conn, &mut snap, &mut ctx)?;
    let had_legacy_corp_crimewatch =
        super::corp_crimewatch_store::load_corp_crimewatch_domain_rows(conn, &mut snap, &mut ctx)?;
    let had_legacy_industry_market_pve =
        super::industry_market_pve_store::load_industry_market_pve_domain_rows(
            conn, &mut snap, &mut ctx,
        )?;
    let had_legacy_service_domains =
        super::service_domains_store::load_service_domains_checked(conn, &mut snap, &mut ctx)?;

    let can_sync_migrations =
        conn.is_autocommit() && !conn.is_readonly(rusqlite::DatabaseName::Main).unwrap_or(false);
    let pre_migration = can_sync_migrations.then(|| snap.clone());
    // Run startup snapshot migrations and flush any migrated rows immediately into SQLite so
    // neither clean reads nor first-transaction rollbacks revert `next_entity_id`,
    // `account_plex_vault`, `service_domains`, `mission_instances`, or `fw_system_occupations`.
    snap.fold_legacy_plex_balances();
    snap.migrate_legacy_service_state();
    snap.reconcile_id_counters();
    let sql_hwm = query_cross_table_entity_high_water(conn)?;
    if sql_hwm > snap.next_entity_id {
        snap.next_entity_id = sql_hwm;
    }
    snap.backfill_fw_system_occupations();
    for (&sid, st) in &mut snap.system_states {
        st.solar_system_id = sid;
    }
    let had_legacy_missions_table = table_exists(conn, "missions")?;
    if had_legacy_missions_table {
        migrate_legacy_missions_table(conn, &mut snap, &legacy_sp6_waves, &mut ctx)?;
    }
    if (had_legacy_missions_table || !has_mission_history_table || upgraded_mission_rows)
        && can_sync_migrations
    {
        super::schema::apply_mission_instances_ddl(conn)?;
    }
    ctx.flush_quarantined_rows(conn)?;
    if let Some(pre_mig) = pre_migration {
        let had_any_legacy = had_legacy_ledger
            || had_legacy_notes_lp
            || had_legacy_sp6_lifecycle
            || had_legacy_social
            || had_legacy_corp_crimewatch
            || had_legacy_industry_market_pve
            || had_legacy_service_domains
            || had_legacy_missions_table
            || had_legacy_sp6_mission_rows;
        if had_any_legacy {
            super::domain_rows::apply_domain_rows_migration(conn)?;
        }
        let changes = crate::tx_engine::ChangeSet::diff(snap.wal_seq, &pre_mig, &snap);
        let did_change_snap = !changes.is_empty();
        if did_change_snap || had_any_legacy || upgraded_mission_rows {
            let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
            if did_change_snap {
                let mut exec =
                    crate::tx_engine::TxSqlExecutor::new(&tx, crate::tx_engine::TxFaultPlan::None);
                let _ = crate::tx_engine::apply_changeset_to_sqlite(
                    &mut exec,
                    &pre_mig,
                    &snap,
                    &changes,
                    #[cfg(any(test, feature = "legacy-json-snapshot"))]
                    false,
                )
                .map_err(|e| e.to_string())?;
            }
            if upgraded_mission_rows {
                super::sync::rewrite_mission_tables(&tx, &snap)?;
            }
            if had_legacy_ledger {
                super::domain_rows::sync_ledger_domain_rows(&tx, &snap)?;
                super::domain_rows::drop_legacy_ledger_tables(&tx)?;
            }
            if had_legacy_notes_lp {
                super::domain_rows::sync_notes_lp_domain_rows(&tx, &snap)?;
                super::domain_rows::drop_legacy_notes_lp_tables(&tx)?;
            }
            if had_legacy_sp6_lifecycle {
                super::domain_rows::sync_sp6_lifecycle_domain_rows(&tx, &snap)?;
                super::domain_rows::drop_legacy_sp6_lifecycle_table(&tx)?;
            }
            if had_legacy_social {
                super::social_store::sync_social_domain_rows(&tx, &snap)?;
                super::domain_rows::drop_legacy_social_tables(&tx)?;
            }
            if had_legacy_corp_crimewatch {
                super::corp_crimewatch_store::sync_corp_crimewatch_domain_rows(&tx, &snap)?;
                super::domain_rows::drop_legacy_corp_crimewatch_tables(&tx)?;
            }
            if had_legacy_industry_market_pve {
                super::industry_market_pve_store::sync_industry_market_pve_domain_rows(&tx, &snap)?;
                super::domain_rows::drop_legacy_industry_market_pve_tables(&tx)?;
            }
            if had_legacy_service_domains {
                super::service_domains_store::sync_service_domains_rows(&tx, &snap)?;
                super::domain_rows::drop_legacy_service_domain_tables(&tx)?;
            }
            if had_legacy_sp6_mission_rows || had_legacy_missions_table {
                tx.execute(
                    "DELETE FROM domain_rows WHERE domain = ?1 AND (key = 'missions_awaiting_downtime_respawn' OR key LIKE 'mission_initial_npc_counts/%' OR key LIKE 'mission_initial_npc_types/%')",
                    rusqlite::params![DOMAIN_SP6_LIFECYCLE],
                )
                .map_err(|e| e.to_string())?;
            }
            if had_legacy_missions_table {
                tx.execute("DROP TABLE IF EXISTS missions", [])
                    .map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
        }
    }
    Ok(snap)
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct LegacySp6MissionWaves {
    missions_awaiting_downtime_respawn: Vec<(i64, u32)>,
    mission_initial_npc_counts: BTreeMap<String, u32>,
    mission_initial_npc_types: BTreeMap<String, Vec<u32>>,
}

#[derive(serde::Deserialize)]
#[serde(default)]
#[derive(Default)]
struct LegacyMissionRowJson {
    mission_id: u32,
    character_id: i64,
    agent_id: i64,
    solar_system_id: SolarSystemId,
    state: String,
    current_room_id: u32,
    npcs_remaining: u32,
    spawned_npc_ball_ids: Vec<i64>,
    bookmark_id: Option<i64>,
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct LegacyMissionMetaJson {
    offered_at_unix: u64,
    offer_expiry_unix: u64,
    accepted_at_unix: Option<u64>,
    accept_expiry_unix: Option<u64>,
    bonus_expiry_unix: Option<u64>,
    required_delivery_type_id: Option<TypeId>,
    required_delivery_qty: i32,
    granted_courier_item_id: Option<i64>,
    granted_courier_qty: i32,
    is_anomic_burner: bool,
    destination_station_id: Option<i64>,
    origin_station_id: Option<i64>,
    offered_level: Option<u8>,
    completed_at_unix: Option<u64>,
}

fn legacy_mission_meta_note_id(char_id: i64, mission_id: u32) -> i64 {
    let mixed = ((char_id.unsigned_abs() & 0xFFFF_FFFF) << 20) ^ (u64::from(mission_id) & 0xFFFFF);
    -((mixed as i64).max(1) + 5_000_000_000_000)
}

fn read_legacy_sp6_mission_waves(
    conn: &Connection,
    ctx: &mut LoadContext,
) -> Result<(LegacySp6MissionWaves, bool), String> {
    let mut waves = LegacySp6MissionWaves::default();
    let mut had_rows = false;
    if table_exists(conn, "sp6_lifecycle_state")? {
        let mut stmt = conn
            .prepare("SELECT state_json FROM sp6_lifecycle_state ORDER BY singleton_id ASC")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            if let Ok(js) = r.get::<_, String>(0) {
                if let Ok(parsed) = serde_json::from_str::<LegacySp6MissionWaves>(&js) {
                    if !parsed.missions_awaiting_downtime_respawn.is_empty() {
                        waves.missions_awaiting_downtime_respawn =
                            parsed.missions_awaiting_downtime_respawn;
                        had_rows = true;
                    }
                    if !parsed.mission_initial_npc_counts.is_empty() {
                        waves.mission_initial_npc_counts = parsed.mission_initial_npc_counts;
                        had_rows = true;
                    }
                    if !parsed.mission_initial_npc_types.is_empty() {
                        waves.mission_initial_npc_types = parsed.mission_initial_npc_types;
                        had_rows = true;
                    }
                }
            }
        }
    }
    if table_exists(conn, "domain_rows")? {
        let mut stmt = conn
            .prepare(
                "SELECT rowid, key, value FROM domain_rows WHERE domain = ?1 AND (key = 'missions_awaiting_downtime_respawn' OR key LIKE 'mission_initial_npc_counts/%' OR key LIKE 'mission_initial_npc_types/%') ORDER BY rowid ASC",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query(rusqlite::params![DOMAIN_SP6_LIFECYCLE]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let raw_key: String = r.get(1).map_err(|e| e.to_string())?;
            let raw_val: String = r.get(2).map_err(|e| e.to_string())?;
            had_rows = true;
            if raw_key == "missions_awaiting_downtime_respawn" {
                match serde_json::from_str::<Vec<(i64, u32)>>(&raw_val) {
                    Ok(v) => waves.missions_awaiting_downtime_respawn = v,
                    Err(e) => ctx.record_bad_row(
                        "domain_rows",
                        DOMAIN_SP6_LIFECYCLE,
                        raw_key,
                        raw_val,
                        e.to_string(),
                        rowid,
                    )?,
                }
            } else if let Some(sub) = raw_key.strip_prefix("mission_initial_npc_counts/") {
                match super::domain_rows::decode_map_entry::<String, u32>(sub, &raw_val) {
                    Ok((k, v)) => {
                        waves.mission_initial_npc_counts.insert(k, v);
                    }
                    Err(err) => ctx.record_bad_row(
                        "domain_rows",
                        DOMAIN_SP6_LIFECYCLE,
                        raw_key,
                        raw_val,
                        err,
                        rowid,
                    )?,
                }
            } else if let Some(sub) = raw_key.strip_prefix("mission_initial_npc_types/") {
                match super::domain_rows::decode_map_entry::<String, Vec<u32>>(sub, &raw_val) {
                    Ok((k, v)) => {
                        waves.mission_initial_npc_types.insert(k, v);
                    }
                    Err(err) => ctx.record_bad_row(
                        "domain_rows",
                        DOMAIN_SP6_LIFECYCLE,
                        raw_key,
                        raw_val,
                        err,
                        rowid,
                    )?,
                }
            }
        }
    }
    Ok((waves, had_rows))
}

fn migrate_legacy_missions_table(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    waves: &LegacySp6MissionWaves,
    ctx: &mut LoadContext,
) -> Result<(), String> {
    struct PendingLegacyMission {
        rowid: Option<i64>,
        key: String,
        payload: String,
        rec: LegacyMissionRowJson,
        state: MissionState,
        meta: LegacyMissionMetaJson,
    }

    let mut pending = Vec::new();
    let mut pending_ball_ids = BTreeSet::new();
    let mut stmt = conn
        .prepare(
            "SELECT rowid, character_id, mission_id, state, record_json FROM missions ORDER BY character_id ASC, mission_id ASC",
        )
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        let rowid = r.get::<_, i64>(0).ok();
        let key = format!("{}:{}", row_col_string(r, 1), row_col_string(r, 2));
        let payload = row_col_string(r, 4);
        let parsed = (|| -> Result<(LegacyMissionRowJson, MissionState), String> {
            let col_char_id = r.get::<_, i64>(1).map_err(|e| e.to_string())?;
            let col_mission_i64 = r.get::<_, i64>(2).map_err(|e| e.to_string())?;
            let col_mission_id = u32::try_from(col_mission_i64)
                .map_err(|_| format!("invalid legacy mission_id column {col_mission_i64}"))?;
            let col_state = r.get::<_, String>(3).map_err(|e| e.to_string())?;
            let col_parsed = MissionState::parse(&col_state)
                .ok_or_else(|| format!("unknown legacy mission state '{col_state}'"))?;
            let js = r.get::<_, String>(4).map_err(|e| e.to_string())?;
            let rec: LegacyMissionRowJson =
                serde_json::from_str(&js).map_err(|e| e.to_string())?;
            if rec.character_id != col_char_id || rec.mission_id != col_mission_id {
                return Err(format!(
                    "legacy mission key ({col_char_id}, {col_mission_id}) != JSON ({}, {})",
                    rec.character_id, rec.mission_id
                ));
            }
            let json_parsed = MissionState::parse(&rec.state)
                .ok_or_else(|| format!("unknown legacy mission JSON state '{}'", rec.state))?;
            if col_parsed != json_parsed {
                return Err(format!(
                    "legacy mission state column '{col_state}' != JSON '{}'",
                    rec.state
                ));
            }
            Ok((rec, json_parsed))
        })();
        let (rec, state) = match parsed {
            Ok(v) => v,
            Err(err) => {
                ctx.record_bad_row("missions", "missions", key, payload, err, rowid)?;
                continue;
            }
        };

        let meta_label = format!("mission_meta:{}:{}", rec.character_id, rec.mission_id);
        let expected_nid = legacy_mission_meta_note_id(rec.character_id, rec.mission_id);
        let matched_nid = snap
            .owner_notes
            .get(&expected_nid)
            .filter(|n| n.label == meta_label || n.character_id == -5)
            .map(|n| n.note_id)
            .or_else(|| {
                snap.owner_notes
                    .iter()
                    .find(|(_, n)| n.label == meta_label)
                    .map(|(&nid, _)| nid)
            });
        let meta = matched_nid
            .and_then(|nid| snap.owner_notes.remove(&nid))
            .and_then(|note| serde_json::from_str::<LegacyMissionMetaJson>(&note.note).ok())
            .unwrap_or_default();

        if state.is_active()
            && snap
                .missions
                .contains_character_template(rec.character_id, rec.mission_id)
        {
            continue;
        }

        for &bid in &rec.spawned_npc_ball_ids {
            pending_ball_ids.insert(bid);
            if bid > 0 && bid < 900_000_000 && bid > snap.next_entity_id {
                snap.next_entity_id = bid;
            }
        }
        pending.push(PendingLegacyMission {
            rowid,
            key,
            payload,
            rec,
            state,
            meta,
        });
    }

    let sde_conn = if pending.is_empty() {
        None
    } else {
        open_sde_sqlite_ro()
    };

    for entry in pending {
        let PendingLegacyMission {
            rowid,
            key,
            payload,
            rec,
            state,
            meta,
        } = entry;

        let wave_key = format!("{}:{}", rec.character_id, rec.mission_id);
        let wave_count = waves.mission_initial_npc_counts.get(&wave_key).copied();
        let wave_types = waves
            .mission_initial_npc_types
            .get(&wave_key)
            .cloned()
            .unwrap_or_default();
        let awaiting_downtime = waves
            .missions_awaiting_downtime_respawn
            .contains(&(rec.character_id, rec.mission_id));

        let has_courier_clue = meta.destination_station_id.is_some()
            || meta.granted_courier_item_id.is_some()
            || meta.granted_courier_qty > 0;
        let has_encounter_clue = rec.npcs_remaining > 0
            || !rec.spawned_npc_ball_ids.is_empty()
            || wave_count.is_some()
            || !wave_types.is_empty()
            || awaiting_downtime;
        let has_delivery_clue =
            meta.required_delivery_type_id.is_some() || meta.required_delivery_qty > 0;

        let kind = if has_courier_clue {
            MissionKind::Courier
        } else if let Some((tmpl, ag)) = sde_conn
            .as_ref()
            .map(|conn| lookup_sde_mission_and_agent(conn, rec.mission_id, rec.agent_id))
            .filter(|(tmpl, ag)| tmpl.is_some() || ag.is_some())
        {
            if has_encounter_clue && !has_delivery_clue && tmpl.is_none() {
                MissionKind::Encounter
            } else {
                let base = decide_mission_kind(tmpl.as_ref(), ag.as_ref());
                match base {
                    MissionKind::Encounter
                        if has_delivery_clue && !has_encounter_clue =>
                    {
                        if meta.required_delivery_type_id == Some(17202) {
                            MissionKind::DataCenterTurnIn
                        } else {
                            MissionKind::Trade
                        }
                    }
                    _ => base,
                }
            }
        } else if has_encounter_clue && !has_delivery_clue {
            MissionKind::Encounter
        } else if meta.required_delivery_type_id == Some(17202) {
            MissionKind::DataCenterTurnIn
        } else if has_delivery_clue {
            MissionKind::Trade
        } else {
            MissionKind::Encounter
        };

        let objective = match kind {
            MissionKind::Encounter => MissionObjective::Encounter(EncounterObjective {
                npcs_remaining: rec.npcs_remaining,
                spawned_npc_ball_ids: rec.spawned_npc_ball_ids,
                initial_wave_count: wave_count,
                initial_wave_types: wave_types,
                awaiting_downtime_respawn: awaiting_downtime,
            }),
            MissionKind::Courier => MissionObjective::Courier(CourierObjective {
                cargo_type_id: meta.required_delivery_type_id,
                quantity: meta.required_delivery_qty,
                destination_station_id: meta.destination_station_id,
                granted_item_id: meta.granted_courier_item_id,
                granted_qty: meta.granted_courier_qty,
            }),
            MissionKind::Trade
            | MissionKind::Mining
            | MissionKind::Heraldry
            | MissionKind::DataCenterTurnIn
            | MissionKind::ResearchTurnIn => MissionObjective::Delivery(DeliveryObjective {
                item_type_id: meta.required_delivery_type_id,
                quantity: meta.required_delivery_qty,
            }),
        };

        let mut instance_id = snap.alloc_entity_id();
        while pending_ball_ids.contains(&instance_id) {
            instance_id = snap.alloc_entity_id();
        }
        let res = if !state.is_active() {
            let ended_at_unix = meta
                .completed_at_unix
                .or(meta.accepted_at_unix)
                .unwrap_or(meta.offered_at_unix);
            snap.missions.record_history(MissionHistoryEntry {
                instance_id,
                character_id: rec.character_id,
                agent_id: rec.agent_id,
                template_id: rec.mission_id,
                kind,
                final_state: state,
                offered_at_unix: meta.offered_at_unix,
                ended_at_unix,
            })
        } else {
            let instance = MissionInstance {
                instance_id,
                character_id: rec.character_id,
                agent_id: rec.agent_id,
                template_id: rec.mission_id,
                state,
                kind,
                solar_system_id: rec.solar_system_id,
                current_room_id: rec.current_room_id,
                bookmark_id: rec.bookmark_id,
                origin_station_id: meta.origin_station_id,
                offered_level: meta.offered_level,
                is_anomic_burner: meta.is_anomic_burner,
                offered_at_unix: meta.offered_at_unix,
                offer_expiry_unix: meta.offer_expiry_unix,
                accepted_at_unix: meta.accepted_at_unix,
                accept_expiry_unix: meta.accept_expiry_unix,
                bonus_expiry_unix: meta.bonus_expiry_unix,
                objective,
            };
            snap.missions.try_insert(instance)
        };
        if let Err(err) = res {
            ctx.record_bad_row("missions", "missions", key, payload, err, rowid)?;
        }
    }
    Ok(())
}
#[cfg(test)] #[path = "load_unit_tests.rs"] mod load_unit_tests;
