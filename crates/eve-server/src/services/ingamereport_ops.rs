//! `ingamereport_ops` — In-Game Pilot Moderation and Incident Reporting Service (`ingamereport`).
//!
//! Handles pilot-submitted incident reports covering bot/macro usage, inappropriate names/language,
//! exploit abuse, gambling, offensive mail, questionable transactions, and general policy violations.
//! Records are persistently logged and stored in memory with authentic character and corporation
//! provenance, returning `PyRep::None`.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

use crate::router::context::ServerContext;

pub use eve_db::InGameReportRecord;

/// Returns all persisted in-game reports from `ctx.db`, sorted by `report_id`.
pub fn get_persisted_reports(ctx: &ServerContext) -> Vec<InGameReportRecord> {
    ctx.db.with_snapshot(|snap| {
        if !snap.fleet_and_social_ops.ingamereport_tickets.is_empty() {
            let mut list = snap.fleet_and_social_ops.ingamereport_tickets.clone();
            list.sort_by_key(|r| r.report_id);
            return list;
        }
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
            Some(handle_submit_report(ctx, session, "exploit_abuse", args, kwargs))
        }
        ("ingamereport", "report_gambling") => {
            Some(handle_submit_report(ctx, session, "gambling", args, kwargs))
        }
        ("ingamereport", "report_inappropriate_character_name") => {
            Some(handle_submit_report(ctx, session, "inappropriate_character_name", args, kwargs))
        }
        ("ingamereport", "report_inappropriate_language") => {
            Some(handle_submit_report(ctx, session, "inappropriate_language", args, kwargs))
        }
        ("ingamereport", "report_macro_use") => {
            Some(handle_submit_report(ctx, session, "macro_use", args, kwargs))
        }
        ("ingamereport", "report_offensive_mail") => {
            Some(handle_submit_report(ctx, session, "offensive_mail", args, kwargs))
        }
        ("ingamereport", "report_other") => {
            Some(handle_submit_report(ctx, session, "other", args, kwargs))
        }
        ("ingamereport", "report_questionable_transaction") => {
            Some(handle_submit_report(ctx, session, "questionable_transaction", args, kwargs))
        }
        _ => None,
    }
}

fn handle_submit_report(
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

    // Extract reason / comments text (V24.01 `reportSvc.pyj` passes `(charID, comments)` or
    // `(charID, comments, chat_time, chat_channel, chat_content)` for `report_inappropriate_language`).
    let reason = args.get(1).and_then(|a| a.as_str())
        .or_else(|| kwargs.get("reason").and_then(|v| v.as_str()))
        .or_else(|| kwargs.get("comments").and_then(|v| v.as_str()))
        .or_else(|| kwargs.get("category").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    // Extract details / comment text, or assemble V24.01 chat evidence for `report_inappropriate_language`
    let mut details = args.get(2).and_then(|a| a.as_str())
        .or_else(|| kwargs.get("details").and_then(|v| v.as_str()))
        .or_else(|| kwargs.get("comment").and_then(|v| v.as_str()))
        .or_else(|| kwargs.get("text").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    if args.len() >= 4 || kwargs.contains_key("chat_channel") || kwargs.contains_key("chat_content") {
        let chat_time = args.get(2)
            .or_else(|| kwargs.get("chat_time"))
            .and_then(|v| v.as_i64().map(|n| n.to_string()).or_else(|| v.as_str().map(str::to_string)))
            .unwrap_or_default();
        let chat_channel = args.get(3)
            .or_else(|| kwargs.get("chat_channel"))
            .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_i64().map(|n| n.to_string())))
            .unwrap_or_default();
        let chat_content = args.get(4)
            .or_else(|| kwargs.get("chat_content"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !chat_channel.is_empty() || !chat_content.is_empty() || !chat_time.is_empty() {
            details = format!(
                "chat_time={chat_time}; chat_channel={chat_channel}; chat_content={chat_content}"
            );
        }
    }

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
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

        snap.fleet_and_social_ops.ingamereport_tickets.push(rec);
    });

    Ok(PyRep::None)
}

#[cfg(test)] #[path = "ingamereport_ops_tests.rs"] mod ingamereport_ops_tests;

