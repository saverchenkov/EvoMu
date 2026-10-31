//! `notify_emitters_b5` — Server-side V24 notification emitters for Batch 5
//! (Bookmarks, Implants & Boosters, Structure Docking & Undocking, Corporation Office Rentals,
//! NPC Standings, and Skill / Skill Queue Updates).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_proto::PyRep;

// ---------------------------------------------------------------------------
// 1. Bookmarks
// ---------------------------------------------------------------------------

/// Emits `OnBookmarkCreated(bookmarkID, comment, typeID)` to `char_id`.
pub fn emit_bookmark_created(
    ctx: &ServerContext,
    char_id: i64,
    bookmark_id: i64,
    comment: &str,
    type_id: i64,
) {
    if char_id <= 0 || bookmark_id <= 0 {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnBookmarkCreated",
        "charid",
        vec![
            PyRep::Integer(bookmark_id),
            PyRep::String(comment.to_string()),
            PyRep::Integer(type_id),
        ],
    );
}

/// Emits `OnRefreshBookmarks()` to `char_id`.
pub fn emit_refresh_bookmarks(ctx: &ServerContext, char_id: i64) {
    if char_id <= 0 {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnRefreshBookmarks",
        "charid",
        vec![],
    );
}

// ---------------------------------------------------------------------------
// 2. Implants & Boosters
// ---------------------------------------------------------------------------

/// Emits `OnImplantsChanged()` to `char_id`.
pub fn emit_implants_changed(ctx: &ServerContext, char_id: i64) {
    if char_id <= 0 {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnImplantsChanged",
        "charid",
        vec![],
    );
}

/// Emits `OnBoosterUpdated()` to `char_id`.
pub fn emit_booster_updated(ctx: &ServerContext, char_id: i64) {
    if char_id <= 0 {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnBoosterUpdated",
        "charid",
        vec![],
    );
}

// ---------------------------------------------------------------------------
// 3. Docking & Undocking
// ---------------------------------------------------------------------------

/// Emits `OnDockingRequestWasDenied(structureID)` to `char_id`.
pub fn emit_docking_request_was_denied(ctx: &ServerContext, char_id: i64, structure_id: i64) {
    if char_id <= 0 || structure_id <= 0 {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDockingRequestWasDenied",
        "charid",
        vec![PyRep::Integer(structure_id)],
    );
}

/// Emits `OnUndockingStarted(location_id)` to `char_id`.
pub fn emit_undocking_started(ctx: &ServerContext, char_id: i64, location_id: i64) {
    if char_id <= 0 || location_id <= 0 {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnUndockingStarted",
        "charid",
        vec![PyRep::Integer(location_id)],
    );
}

/// Emits `OnUndockingCompleted(itemID)` to `char_id`.
pub fn emit_undocking_completed(ctx: &ServerContext, char_id: i64, item_id: i64) {
    if char_id <= 0 || item_id <= 0 {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnUndockingCompleted",
        "charid",
        vec![PyRep::Integer(item_id)],
    );
}

// ---------------------------------------------------------------------------
// 4. Corporation Office Rentals
// ---------------------------------------------------------------------------

/// Emits `OnOfficeRentalChanged(corporationID, officeID)` to `corp_id`.
pub fn emit_office_rental_changed(ctx: &ServerContext, corp_id: i64, office_id: i64) {
    if corp_id <= 0 || office_id <= 0 {
        return;
    }
    ctx.notify(
        NotificationTarget::Corporation(corp_id),
        "OnOfficeRentalChanged",
        "corpid",
        vec![PyRep::Integer(corp_id), PyRep::Integer(office_id)],
    );
}

// ---------------------------------------------------------------------------
// 5. NPC Standings
// ---------------------------------------------------------------------------

/// Emits `OnNPCStandingChange(fromID, newStanding, oldStanding)` to `char_id`.
pub fn emit_npc_standing_change(
    ctx: &ServerContext,
    char_id: i64,
    from_id: i64,
    new_standing: f64,
    old_standing: f64,
) {
    if char_id <= 0 || from_id <= 0 || (new_standing - old_standing).abs() < f64::EPSILON {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnNPCStandingChange",
        "charid",
        vec![
            PyRep::Integer(from_id),
            PyRep::Real(new_standing),
            PyRep::Real(old_standing),
        ],
    );
}

// ---------------------------------------------------------------------------
// 6. Skills & Skill Queue
// ---------------------------------------------------------------------------

/// Emits `OnSkillsChanged(skills)` to `char_id`.
pub fn emit_skills_changed(ctx: &ServerContext, char_id: i64, entries: Vec<(PyRep, PyRep)>) {
    if char_id <= 0 || entries.is_empty() {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSkillsChanged",
        "charid",
        vec![PyRep::Dict(entries)],
    );
}

/// Emits `OnSkillQueueChanged()` to `char_id`.
pub fn emit_skill_queue_changed(ctx: &ServerContext, char_id: i64) {
    if char_id <= 0 {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSkillQueueChanged",
        "charid",
        vec![],
    );
}

/// Emits `OnSkillQueuePaused()` to `char_id`.
pub fn emit_skill_queue_paused(ctx: &ServerContext, char_id: i64) {
    if char_id <= 0 {
        return;
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSkillQueuePaused",
        "charid",
        vec![],
    );
}
