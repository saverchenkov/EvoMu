//! `ingamereport_ops` — In-Game Pilot Moderation and Incident Reporting Service (`ingamereport`).
//!
//! Handles pilot-submitted incident reports covering bot/macro usage, inappropriate names/language,
//! exploit abuse, gambling, offensive mail, questionable transactions, and general policy violations.
//! Records are persistently logged and stored in memory with authentic character and corporation
//! provenance, returning `PyRep::None`.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::Mutex;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};

use crate::router::context::ServerContext;

/// In-game report incident record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InGameReportRecord {
    pub report_id: i64,
    pub reporter_char_id: i64,
    pub reporter_corp_id: i64,
    pub reported_entity_id: i64,
    pub category: String,
    pub reason: String,
    pub details: String,
    pub created_at: u64,
}

pub static IN_GAME_REPORTS: LazyLock<Mutex<HashMap<i64, InGameReportRecord>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Returns a clone of all recorded in-game reports from the in-memory store.
pub fn get_all_reports() -> Vec<InGameReportRecord> {
    let g = IN_GAME_REPORTS.lock().unwrap();
    let mut list: Vec<InGameReportRecord> = g.values().cloned().collect();
    list.sort_by_key(|r| r.report_id);
    list
}

/// Returns all persisted in-game reports from `ctx.db` (`owner_notes` under character `-78`),
/// sorted by `report_id`.
pub fn get_persisted_reports(ctx: &ServerContext) -> Vec<InGameReportRecord> {
    ctx.db.with_snapshot(|snap| {
        let mut list: Vec<InGameReportRecord> = snap
            .owner_notes
            .values()
            .filter(|n| n.character_id == -78)
            .filter_map(|n| serde_json::from_str::<InGameReportRecord>(&n.note).ok())
            .collect();
        list.sort_by_key(|r| r.report_id);
        list
    })
}

/// Clears all recorded in-game reports (test helper).
pub fn clear_reports() {
    IN_GAME_REPORTS.lock().unwrap().clear();
}

/// Dispatches `ingamereport.*` RPC calls.
pub fn handle(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match ("ingamereport", method) {
        ("ingamereport", "report_exploit_abuse") => {
            Some(submit_report(ctx, session, "exploit_abuse", args, kwargs))
        }
        ("ingamereport", "report_gambling") => {
            Some(submit_report(ctx, session, "gambling", args, kwargs))
        }
        ("ingamereport", "report_inappropriate_character_name") => {
            Some(submit_report(ctx, session, "inappropriate_character_name", args, kwargs))
        }
        ("ingamereport", "report_inappropriate_language") => {
            Some(submit_report(ctx, session, "inappropriate_language", args, kwargs))
        }
        ("ingamereport", "report_macro_use") => {
            Some(submit_report(ctx, session, "macro_use", args, kwargs))
        }
        ("ingamereport", "report_offensive_mail") => {
            Some(submit_report(ctx, session, "offensive_mail", args, kwargs))
        }
        ("ingamereport", "report_other") => {
            Some(submit_report(ctx, session, "other", args, kwargs))
        }
        ("ingamereport", "report_questionable_transaction") => {
            Some(submit_report(ctx, session, "questionable_transaction", args, kwargs))
        }
        _ => None,
    }
}

fn submit_report(
    ctx: &ServerContext,
    session: &SessionState,
    category: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let corp_id = session.corporation_id.unwrap_or(0);

    // Extract reported entity ID from args or kwargs (e.g. targetID, characterID, mailID, transactionID)
    let reported_id = args.first().and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("targetID").and_then(|v| v.as_i64()))
        .or_else(|| kwargs.get("characterID").and_then(|v| v.as_i64()))
        .or_else(|| kwargs.get("mailID").and_then(|v| v.as_i64()))
        .or_else(|| kwargs.get("transactionID").and_then(|v| v.as_i64()))
        .or_else(|| kwargs.get("reportedID").and_then(|v| v.as_i64()))
        .unwrap_or(0);

    // Extract reason text
    let reason = args.get(1).and_then(|a| a.as_str())
        .or_else(|| kwargs.get("reason").and_then(|v| v.as_str()))
        .or_else(|| kwargs.get("category").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    // Extract details / comment text
    let details = args.get(2).and_then(|a| a.as_str())
        .or_else(|| kwargs.get("details").and_then(|v| v.as_str()))
        .or_else(|| kwargs.get("comment").and_then(|v| v.as_str()))
        .or_else(|| kwargs.get("text").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    let now = ctx.now_unix();
    let record = ctx.db.with_mut_snapshot(|snap| {
        let report_id = snap.allocate_report_id();
        let rec = InGameReportRecord {
            report_id,
            reporter_char_id: char_id,
            reporter_corp_id: corp_id,
            reported_entity_id: reported_id,
            category: category.to_string(),
            reason,
            details,
            created_at: now,
        };
        if let Ok(serialized) = serde_json::to_string(&rec) {
            let nid = 780_000_000 + report_id;
            snap.owner_notes.insert(
                nid,
                eve_db::OwnerNoteRecord {
                    note_id: nid,
                    character_id: -78,
                    item_id: reported_id,
                    label: format!("report:{category}:{report_id}"),
                    note: serialized,
                    timestamp_unix: now,
                },
            );
        }
        rec
    });

    if ctx.db.sqlite_path().is_none() {
        IN_GAME_REPORTS
            .lock()
            .unwrap()
            .insert(record.report_id, record);
    }

    Ok(PyRep::None)
}
