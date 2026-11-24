//! Snapshot-backed Limited Engagement helper functions for `combat_crimewatch` (`#1541` Track A3).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use std::collections::HashMap;

/// Returns `true` if `char_id` holds an active Limited Engagement (`now_unix < exp`)
/// against `target_id` inside `snap`.
#[inline]
pub fn has_limited_engagement_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    target_id: i64,
    now_unix: u64,
) -> bool {
    snap.combat_crimewatch_state
        .limited_engagements
        .get(&char_id)
        .and_then(|m| m.get(&target_id))
        .is_some_and(|&exp| now_unix < exp)
}

/// Returns `true` if `char_id` holds an active Limited Engagement (`now_unix < exp`)
/// against `target_id` in persisted DB snapshot state.
#[inline]
pub fn has_limited_engagement_with(
    ctx: &ServerContext,
    char_id: i64,
    target_id: i64,
    now_unix: u64,
) -> bool {
    ctx.db
        .with_snapshot(|snap| has_limited_engagement_in_snap(snap, char_id, target_id, now_unix))
}

/// Returns `true` if `char_id` holds any active Limited Engagement (`now_unix < exp`)
/// inside `snap`.
#[inline]
pub fn is_in_any_limited_engagement_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    now_unix: u64,
) -> bool {
    snap.combat_crimewatch_state
        .limited_engagements
        .get(&char_id)
        .is_some_and(|m| m.values().any(|&exp| now_unix < exp))
}

/// Returns `true` if `char_id` holds any active Limited Engagement (`now_unix < exp`)
/// in persisted DB snapshot state.
#[inline]
pub fn is_in_any_limited_engagement(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
) -> bool {
    ctx.db
        .with_snapshot(|snap| is_in_any_limited_engagement_in_snap(snap, char_id, now_unix))
}

/// Grants or refreshes a 300s directional Limited Engagement (`char_id -> target_id`)
/// directly inside `snap`.
#[inline]
pub fn add_limited_engagement_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    target_id: i64,
    now_unix: u64,
) {
    let exp = now_unix.saturating_add(300);
    snap.combat_crimewatch_state
        .as_inner_mut()
        .limited_engagements
        .entry(char_id)
        .or_default()
        .insert(target_id, exp);
}

/// Grants or refreshes a 300s directional Limited Engagement (`char_id -> target_id`)
/// directly in SQLite snapshot storage.
#[inline]
pub fn add_limited_engagement(
    ctx: &ServerContext,
    char_id: i64,
    target_id: i64,
    now_unix: u64,
) {
    ctx.db
        .with_mut_snapshot(|snap| add_limited_engagement_in_snap(snap, char_id, target_id, now_unix));
}

/// Grants or refreshes a bidirectional 300s Limited Engagement between `char_a` and `char_b`
/// directly inside `snap`.
#[inline]
pub fn add_mutual_limited_engagement_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    char_a: i64,
    char_b: i64,
    now_unix: u64,
) {
    let exp = now_unix.saturating_add(300);
    let le = &mut snap
        .combat_crimewatch_state
        .as_inner_mut()
        .limited_engagements;
    le.entry(char_a).or_default().insert(char_b, exp);
    le.entry(char_b).or_default().insert(char_a, exp);
}

/// Creates or refreshes a bidirectional 300s Limited Engagement between `char_a` and `char_b`
/// when `create_new` is true or `char_a` already holds an active engagement against `char_b`.
/// Returns `true` if a Limited Engagement was created or refreshed.
pub fn ensure_mutual_limited_engagement_if(
    ctx: &ServerContext,
    char_a: i64,
    char_b: i64,
    create_new: bool,
    now_unix: u64,
) -> bool {
    // Fast path avoids acquiring the single DB writer lock on common non-LE attacks.
    if !create_new && !has_limited_engagement_with(ctx, char_a, char_b, now_unix) {
        return false;
    }
    ctx.db.with_mut_snapshot(|snap| {
        if !create_new && !has_limited_engagement_in_snap(snap, char_a, char_b, now_unix) {
            return false;
        }
        add_mutual_limited_engagement_in_snap(snap, char_a, char_b, now_unix);
        true
    })
}

/// Returns `true` if `target_char_id`'s active engagement (`now_unix <= exp`) against `peer_id`
/// is not also held by `helper_char_id`.
#[inline]
fn is_unshared_active_engagement(
    helper_les: Option<&HashMap<i64, u64>>,
    helper_char_id: i64,
    peer_id: i64,
    exp: u64,
    now_unix: u64,
) -> bool {
    peer_id != helper_char_id
        && now_unix <= exp
        && !helper_les
            .and_then(|hm| hm.get(&peer_id))
            .is_some_and(|&hexp| now_unix <= hexp)
}

/// Returns the peer character IDs with whom `target_char_id` holds an active
/// Limited Engagement (`now_unix <= exp`) that `helper_char_id` does not share.
pub fn unshared_limited_engagement_partners_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    helper_char_id: i64,
    target_char_id: i64,
    now_unix: u64,
) -> Vec<i64> {
    let le = &snap.combat_crimewatch_state.limited_engagements;
    let helper_les = le.get(&helper_char_id);
    le.get(&target_char_id)
        .map(|m| {
            m.iter()
                .filter(|&(&pid, &exp)| {
                    is_unshared_active_engagement(helper_les, helper_char_id, pid, exp, now_unix)
                })
                .map(|(&pid, _)| pid)
                .collect()
        })
        .unwrap_or_default()
}

/// Returns `true` if `target_char_id` holds at least one active Limited Engagement
/// (`now_unix <= exp`) against a peer not shared by `helper_char_id`.
pub fn has_unshared_limited_engagement_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    helper_char_id: i64,
    target_char_id: i64,
    now_unix: u64,
) -> bool {
    let le = &snap.combat_crimewatch_state.limited_engagements;
    let helper_les = le.get(&helper_char_id);
    le.get(&target_char_id).is_some_and(|m| {
        m.iter().any(|(&pid, &exp)| {
            is_unshared_active_engagement(helper_les, helper_char_id, pid, exp, now_unix)
        })
    })
}
