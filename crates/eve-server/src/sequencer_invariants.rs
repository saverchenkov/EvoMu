//! Outbound sequencer per-entity materialization gate and Destiny group invariant validator (`GH #1084`).
//!
//! Enforces:
//! 1. Per-entity materialization tracking across emitted `SetState`, `AddBalls2` / `AddBalls` / `AddBall`,
//!    and `RemoveBalls` / `RemoveBall` updates; drops entity-dependent notifications/updates targeting
//!    unmaterialized balls (reason `entity-not-materialized`), while ordering same-dispatch dependent
//!    updates after the `AddBalls2` / `SetState` that materializes their entity.
//! 2. Destiny group invariants on every outgoing `DoDestinyUpdate` frame:
//!    - `single-destiny-stamp-per-notification`
//!    - `group-stamp-matches-update-stamp`
//!    - `monotonic-destiny-notification-stamp`
//!    - `remove-after-addballs`

#![forbid(unsafe_code)]

use crate::flight_recorder::{FlightRecorder, SessionSequencerCounters};
use crate::outbound_sequencer::{SequencerLedgerEvent, SequencerLedgerEventKind};
use eve_destiny::wire::decode_add_balls;
use eve_destiny::BallId;
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};

pub const INV_SINGLE_STAMP_PER_NOTIFICATION: &str = "single-destiny-stamp-per-notification";
pub const INV_GROUP_STAMP_MATCHES_UPDATE_STAMP: &str = "group-stamp-matches-update-stamp";
pub const INV_MONOTONIC_STAMP: &str = "monotonic-destiny-notification-stamp";
pub const INV_REMOVE_AFTER_ADDBALLS: &str = "remove-after-addballs";
pub const REASON_ENTITY_NOT_MATERIALIZED: &str = "entity-not-materialized";

/// Behavior when a Destiny group invariant fails on an outgoing `DoDestinyUpdate` frame (`GH #1084`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvariantValidatorMode {
    /// Record the violation in counters + `FlightRecorder`, then panic loudly (default in tests).
    StrictPanic,
    /// Increment counters, record in `FlightRecorder`, and still emit the frame (default in release).
    RecordAndEmit,
}

impl Default for InvariantValidatorMode {
    fn default() -> Self {
        if cfg!(any(test, feature = "testkit")) {
            Self::StrictPanic
        } else {
            Self::RecordAndEmit
        }
    }
}

/// Per-session materialization gate counters and Destiny group validator state.
#[derive(Debug, Clone, Default)]
pub struct SequencerGuardState {
    pub mode: InvariantValidatorMode,
    pub last_emitted_epoch_and_stamp: Option<(u32, u32)>,
    pub last_addballs_stamp: HashMap<BallId, u32>,
    pub counters: SessionSequencerCounters,
}

impl SequencerGuardState {
    pub fn reset_epoch_state(&mut self) {
        self.last_emitted_epoch_and_stamp = None;
        self.last_addballs_stamp.clear();
    }

    pub fn note_prevented_jolt_counter(&mut self) {
        self.counters.prevented_jolts = self.counters.prevented_jolts.saturating_add(1);
    }

    #[allow(clippy::too_many_arguments)]
    fn record_dependent_drop(
        &mut self,
        flight_recorder: Option<&FlightRecorder>,
        session_id: u64,
        epoch: u32,
        stamp: u32,
        notification_name: &str,
        ball_id: BallId,
        events: &mut Vec<SequencerLedgerEvent>,
    ) -> SequencerLedgerEvent {
        self.counters.dropped_dependent_notifications = self
            .counters
            .dropped_dependent_notifications
            .saturating_add(1);
        *self
            .counters
            .dropped_dependent_by_name
            .entry(notification_name.to_string())
            .or_insert(0) += 1;

        if let Some(fr) = flight_recorder {
            fr.record_dropped_dependent_notification(session_id, notification_name);
        }

        let ev = SequencerLedgerEvent {
            kind: SequencerLedgerEventKind::Dropped,
            epoch,
            stamp,
            reason: format!(
                "{REASON_ENTITY_NOT_MATERIALIZED}:{notification_name}:ball={ball_id}"
            ),
            packet_kind: Some(notification_name.to_string()),
        };
        events.push(ev.clone());
        ev
    }

    fn record_violation(
        &mut self,
        flight_recorder: Option<&FlightRecorder>,
        session_id: u64,
        epoch: u32,
        stamp: u32,
        invariant: &str,
        detail: &str,
    ) {
        self.counters.invariant_violations = self.counters.invariant_violations.saturating_add(1);
        *self
            .counters
            .invariant_violations_by_name
            .entry(invariant.to_string())
            .or_insert(0) += 1;

        if let Some(fr) = flight_recorder {
            fr.record_invariant_violation(session_id, epoch, stamp, invariant, detail);
        }
    }

    /// Reconciles a `DoDestinyUpdate` `updates` `PyRep` against `materialized`:
    /// - Dependent updates whose target entity is materialized later in the same frame are
    ///   reordered immediately after the first materializing `SetState` / `AddBalls*` entry.
    /// - Remaining dependent updates targeting unmaterialized entities are dropped (never queued),
    ///   counted per session/name, and emitted as `Dropped` ledger events.
    /// Returns `false` if all updates in the frame were dropped so the frame itself may be omitted.
    #[allow(clippy::too_many_arguments)]
    pub fn reconcile_destiny_updates_with_gate(
        &mut self,
        materialized: &mut HashSet<BallId>,
        flight_recorder: Option<&FlightRecorder>,
        session_id: u64,
        epoch: u32,
        frame_stamp: u32,
        updates: &mut PyRep,
        events: &mut Vec<SequencerLedgerEvent>,
    ) -> bool {
        let items_taken = match updates {
            PyRep::List(items) => std::mem::take(items),
            PyRep::Tuple(items) => std::mem::take(items),
            _ => return true,
        };

        if items_taken.is_empty() {
            return false;
        }

        let n = items_taken.len();
        let mut mat_effects: Vec<(bool, Vec<BallId>, Vec<BallId>)> = Vec::with_capacity(n);
        let mut dep_targets: Vec<Option<(String, BallId)>> = Vec::with_capacity(n);
        for entry in &items_taken {
            mat_effects.push(collect_materialization_effect(entry));
            dep_targets.push(extract_dependent_destiny_target_from_entry(entry));
        }

        // Pass 1: Identify premature dependent updates whose target is materialized later in the same dispatch.
        let mut deferred_after: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut is_deferred = vec![false; n];
        let mut sim_mat = materialized.clone();

        for i in 0..n {
            let (resets, ref added, ref removed) = mat_effects[i];
            if resets {
                sim_mat.clear();
            }
            for &id in added {
                sim_mat.insert(id);
            }
            for &id in removed {
                sim_mat.remove(&id);
            }
            if let Some((_, target_id)) = dep_targets[i] {
                if !sim_mat.contains(&target_id) {
                    if let Some(mat_idx) = ((i + 1)..n).find(|&m| mat_effects[m].1.contains(&target_id))
                    {
                        deferred_after[mat_idx].push(i);
                        is_deferred[i] = true;
                    }
                }
            }
        }

        let mut ordered_indices = Vec::with_capacity(n);
        for i in 0..n {
            if !is_deferred[i] {
                ordered_indices.push(i);
            }
            for &dep_idx in &deferred_after[i] {
                ordered_indices.push(dep_idx);
            }
        }

        // Pass 2: Advance real `materialized` and drop any remaining unmaterialized dependent updates.
        let mut entries_opt: Vec<Option<PyRep>> = items_taken.into_iter().map(Some).collect();
        let mut filtered = Vec::with_capacity(n);
        for idx in ordered_indices {
            let (resets, ref added, ref removed) = mat_effects[idx];
            if resets {
                materialized.clear();
            }
            for &id in added {
                materialized.insert(id);
            }
            for &id in removed {
                materialized.remove(&id);
            }
            if let Some((ref action_name, target_id)) = dep_targets[idx] {
                if !materialized.contains(&target_id) {
                    self.record_dependent_drop(
                        flight_recorder,
                        session_id,
                        epoch,
                        frame_stamp,
                        action_name,
                        target_id,
                        events,
                    );
                    continue;
                }
            }
            if let Some(entry) = entries_opt[idx].take() {
                filtered.push(entry);
            }
        }

        let keep = !filtered.is_empty();
        match updates {
            PyRep::List(items) | PyRep::Tuple(items) => {
                *items = filtered;
            }
            _ => {}
        }
        keep
    }

    /// Evaluates a standalone outgoing `MachoNet` notification against `materialized`.
    /// Returns `Some(SequencerLedgerEvent)` when dropped (`entity-not-materialized`).
    pub fn gate_notification(
        &mut self,
        materialized: &HashSet<BallId>,
        flight_recorder: Option<&FlightRecorder>,
        session_id: u64,
        epoch: u32,
        stamp: u32,
        event: &str,
        args: &[PyRep],
    ) -> Option<SequencerLedgerEvent> {
        let target_id = extract_dependent_notification_target(event, args)?;
        if materialized.contains(&target_id) {
            return None;
        }
        let mut events = Vec::with_capacity(1);
        Some(self.record_dependent_drop(
            flight_recorder,
            session_id,
            epoch,
            stamp,
            event,
            target_id,
            &mut events,
        ))
    }

    /// Validates all 4 Destiny group invariants on an outgoing `DoDestinyUpdate` frame (`GH #1084`).
    ///
    /// Always increments counters and records to `FlightRecorder` (if attached) before enforcing
    /// [`InvariantValidatorMode::StrictPanic`]. Returns any violated invariant names.
    pub fn validate_outgoing_destiny_frame(
        &mut self,
        flight_recorder: Option<&FlightRecorder>,
        session_id: u64,
        epoch: u32,
        frame_stamp: u32,
        updates: &PyRep,
    ) -> Vec<String> {
        let mut violations: Vec<(&'static str, String)> = Vec::new();
        let entries = match updates {
            PyRep::List(items) | PyRep::Tuple(items) => items.as_slice(),
            _ => &[],
        };

        // 1 & 2. `single-destiny-stamp-per-notification` and `group-stamp-matches-update-stamp`.
        let mut valid_stamps: Vec<u32> = Vec::with_capacity(entries.len());
        let mut invalid_entry_stamp = false;
        for entry in entries {
            match parse_destiny_entry(entry) {
                Some((Some(st), _, _)) => valid_stamps.push(st),
                Some((None, _, _)) => invalid_entry_stamp = true,
                None => {}
            }
        }
        if !valid_stamps.is_empty() {
            let first = valid_stamps[0];
            if let Some(&other) = valid_stamps.iter().find(|&&s| s != first) {
                violations.push((
                    INV_SINGLE_STAMP_PER_NOTIFICATION,
                    format!(
                        "destiny group mixes multiple entry stamps ({first} vs {other}) for session={session_id} epoch={epoch}"
                    ),
                ));
            } else if invalid_entry_stamp {
                violations.push((
                    INV_GROUP_STAMP_MATCHES_UPDATE_STAMP,
                    format!(
                        "destiny group contains invalid/non-u32 entry stamp while frame_stamp={frame_stamp}"
                    ),
                ));
            } else if first != frame_stamp {
                violations.push((
                    INV_GROUP_STAMP_MATCHES_UPDATE_STAMP,
                    format!(
                        "group stamp {frame_stamp} != update entries stamp {first} (session={session_id} epoch={epoch})"
                    ),
                ));
            }
        } else if invalid_entry_stamp {
            violations.push((
                INV_GROUP_STAMP_MATCHES_UPDATE_STAMP,
                format!(
                    "destiny group contains invalid/non-u32 entry stamp while frame_stamp={frame_stamp}"
                ),
            ));
        }

        // 3. `monotonic-destiny-notification-stamp` (non-decreasing per active space epoch).
        if let Some((prev_epoch, prev_stamp)) = self.last_emitted_epoch_and_stamp {
            if prev_epoch == epoch && frame_stamp < prev_stamp {
                violations.push((
                    INV_MONOTONIC_STAMP,
                    format!(
                        "destiny notification stamp regressed from {prev_stamp} to {frame_stamp} (session={session_id} epoch={epoch})"
                    ),
                ));
            }
        }
        self.last_emitted_epoch_and_stamp = Some(match self.last_emitted_epoch_and_stamp {
            Some((prev_epoch, prev_stamp)) if prev_epoch == epoch => {
                (epoch, prev_stamp.max(frame_stamp))
            }
            _ => (epoch, frame_stamp),
        });

        // 4. `remove-after-addballs` (`SetState` resets `last_addballs_stamp`).
        for entry in entries {
            let Some((entry_stamp, action, args_opt)) = parse_destiny_entry(entry) else {
                continue;
            };
            let eff_stamp = entry_stamp.unwrap_or(frame_stamp);
            if action == "SetState" {
                self.last_addballs_stamp.clear();
            } else if matches!(action, "AddBalls2" | "AddBalls" | "AddBall") {
                if let Some(args) = args_opt {
                    let mut added = Vec::new();
                    extract_add_balls_entities(action, args, &mut added);
                    for id in added {
                        self.last_addballs_stamp.insert(id, eff_stamp);
                    }
                }
            } else if matches!(action, "RemoveBalls" | "RemoveBall") {
                if let Some(args) = args_opt {
                    let mut removed = Vec::new();
                    extract_remove_balls_entities(args, &mut removed);
                    for id in removed {
                        if let Some(&add_stamp) = self.last_addballs_stamp.get(&id) {
                            if eff_stamp <= add_stamp {
                                violations.push((
                                    INV_REMOVE_AFTER_ADDBALLS,
                                    format!(
                                        "{action} for ball={id} at stamp={eff_stamp} <= last AddBalls stamp={add_stamp}"
                                    ),
                                ));
                                break;
                            }
                        }
                    }
                }
            }
        }

        let mut names = Vec::with_capacity(violations.len());
        for (inv_name, detail) in &violations {
            self.record_violation(
                flight_recorder,
                session_id,
                epoch,
                frame_stamp,
                inv_name,
                detail,
            );
            names.push((*inv_name).to_string());
        }

        if let Some((first_inv, first_detail)) = violations.first() {
            if self.mode == InvariantValidatorMode::StrictPanic {
                panic!(
                    "destiny group invariant violation [{first_inv}]: {first_detail}"
                );
            }
        }

        names
    }
}

fn positive_i64(rep: &PyRep) -> Option<BallId> {
    rep.as_i64().filter(|&id| id > 0)
}

fn extract_slim_ball_id(rep: &PyRep) -> Option<BallId> {
    rep.dict_get("itemID")
        .or_else(|| rep.dict_get("ballID"))
        .and_then(positive_i64)
}

pub(crate) fn parse_destiny_entry(entry: &PyRep) -> Option<(Option<u32>, &str, Option<&PyRep>)> {
    let outer = match entry {
        PyRep::Tuple(items) | PyRep::List(items) if items.len() >= 2 => items,
        _ => return None,
    };
    let stamp = outer[0].as_i64().and_then(|v| u32::try_from(v).ok());
    let action_pair = match &outer[1] {
        PyRep::Tuple(items) | PyRep::List(items) if !items.is_empty() => items,
        _ => return None,
    };
    let action = action_pair[0].as_str()?;
    Some((stamp, action, action_pair.get(1)))
}

fn push_unique(out: &mut Vec<BallId>, id: BallId) {
    if id > 0 && !out.contains(&id) {
        out.push(id);
    }
}

/// Extracts all ball IDs materialized by a `SetState` `args` payload (`ego`, `slims`, `state`).
pub fn extract_set_state_entities(args: &PyRep, out: &mut Vec<BallId>) {
    fn visit_set_state_dict(dict_rep: &PyRep, out: &mut Vec<BallId>) {
        if let Some(ego) = dict_rep.dict_get("ego").and_then(positive_i64) {
            push_unique(out, ego);
        }
        if let Some(PyRep::List(slims) | PyRep::Tuple(slims)) = dict_rep.dict_get("slims") {
            for slim in slims {
                if let Some(id) = extract_slim_ball_id(slim) {
                    push_unique(out, id);
                }
            }
        }
        if let Some(PyRep::Bytes(state_bytes)) = dict_rep.dict_get("state") {
            if let Ok((_, _, decoded)) = decode_add_balls(state_bytes) {
                for b in decoded {
                    push_unique(out, b.id);
                }
            }
        }
    }

    visit_set_state_dict(args, out);
    if let PyRep::Tuple(items) | PyRep::List(items) = args {
        for item in items {
            visit_set_state_dict(item, out);
        }
    }
}

/// Extracts all ball IDs materialized by an `AddBall`, `AddBalls`, or `AddBalls2` `args` payload.
pub fn extract_add_balls_entities(action: &str, args: &PyRep, out: &mut Vec<BallId>) {
    if action == "AddBall" {
        if let Some(id) = extract_slim_ball_id(args).or_else(|| positive_i64(args)) {
            push_unique(out, id);
            return;
        }
        if let PyRep::Tuple(items) | PyRep::List(items) = args {
            if let Some(first) = items.first() {
                if let Some(id) = extract_slim_ball_id(first).or_else(|| positive_i64(first)) {
                    push_unique(out, id);
                } else if let PyRep::Tuple(pair) | PyRep::List(pair) = first {
                    if let Some(id) = pair.first().and_then(extract_slim_ball_id) {
                        push_unique(out, id);
                    }
                }
            }
        }
        return;
    }

    fn collect_chunk(chunk: &PyRep, out: &mut Vec<BallId>) {
        if let Some(id) = positive_i64(chunk) {
            push_unique(out, id);
            return;
        }
        if let PyRep::Bytes(bytes) = chunk {
            if let Ok((_, _, decoded)) = decode_add_balls(bytes) {
                for b in decoded {
                    push_unique(out, b.id);
                }
            }
            return;
        }
        let parts = match chunk {
            PyRep::Tuple(items) | PyRep::List(items) => items,
            _ => return,
        };
        let before = out.len();
        if let Some(PyRep::Bytes(bytes)) = parts.first() {
            if let Ok((_, _, decoded)) = decode_add_balls(bytes) {
                for b in decoded {
                    push_unique(out, b.id);
                }
            }
        }
        if let Some(PyRep::List(extra_or_slims) | PyRep::Tuple(extra_or_slims)) = parts.get(1) {
            for elem in extra_or_slims {
                if let Some(id) = extract_slim_ball_id(elem).or_else(|| positive_i64(elem)) {
                    push_unique(out, id);
                } else if let PyRep::Tuple(pair) | PyRep::List(pair) = elem {
                    if let Some(id) = pair.first().and_then(extract_slim_ball_id) {
                        push_unique(out, id);
                    }
                }
            }
        }
        if let Some(PyRep::Dict(dmg_entries)) = parts.get(2) {
            for (k, _) in dmg_entries {
                if let Some(id) = positive_i64(k) {
                    push_unique(out, id);
                }
            }
        }
        if out.len() == before {
            for elem in parts {
                if let Some(id) = positive_i64(elem) {
                    push_unique(out, id);
                } else if let Some(id) = extract_slim_ball_id(elem) {
                    push_unique(out, id);
                }
            }
        }
    }

    match args {
        PyRep::Tuple(items) | PyRep::List(items) => {
            if items
                .first()
                .is_some_and(|first| matches!(first, PyRep::Bytes(_)))
            {
                collect_chunk(args, out);
            } else {
                for elem in items {
                    if let PyRep::List(nested) | PyRep::Tuple(nested) = elem {
                        if nested
                            .first()
                            .is_some_and(|f| matches!(f, PyRep::Tuple(_) | PyRep::List(_)))
                            && nested.iter().all(|f| matches!(f, PyRep::Tuple(_) | PyRep::List(_)))
                        {
                            for inner_chunk in nested {
                                collect_chunk(inner_chunk, out);
                            }
                            continue;
                        }
                    }
                    collect_chunk(elem, out);
                }
            }
        }
        other => collect_chunk(other, out),
    }
}

/// Extracts all ball IDs removed by a `RemoveBall` or `RemoveBalls` `args` payload.
pub fn extract_remove_balls_entities(args: &PyRep, out: &mut Vec<BallId>) {
    match args {
        PyRep::Integer(_) => {
            if let Some(id) = positive_i64(args) {
                push_unique(out, id);
            }
        }
        PyRep::Tuple(items) | PyRep::List(items) => {
            if let Some(PyRep::List(inner) | PyRep::Tuple(inner)) = items.first() {
                for elem in inner {
                    if let Some(id) = positive_i64(elem) {
                        push_unique(out, id);
                    }
                }
            } else {
                for elem in items {
                    if let Some(id) = positive_i64(elem) {
                        push_unique(out, id);
                    }
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn collect_materialization_effect(entry: &PyRep) -> (bool, Vec<BallId>, Vec<BallId>) {
    let Some((_, action, args_opt)) = parse_destiny_entry(entry) else {
        return (false, Vec::new(), Vec::new());
    };
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut resets = false;
    match (action, args_opt) {
        ("SetState", Some(args)) => {
            resets = true;
            extract_set_state_entities(args, &mut added);
        }
        ("SetState", None) => {
            resets = true;
        }
        ("AddBalls2" | "AddBalls" | "AddBall", Some(args)) => {
            extract_add_balls_entities(action, args, &mut added);
        }
        ("RemoveBalls" | "RemoveBall", Some(args)) => {
            extract_remove_balls_entities(args, &mut removed);
        }
        _ => {}
    }
    (resets, added, removed)
}

fn first_pos_arg(args: &PyRep) -> Option<BallId> {
    match args {
        PyRep::Integer(_) => positive_i64(args),
        PyRep::Tuple(items) | PyRep::List(items) => items.first().and_then(positive_i64),
        _ => None,
    }
}

pub(crate) fn extract_dependent_destiny_target_from_entry(entry: &PyRep) -> Option<(String, BallId)> {
    let (_, action, args_opt) = parse_destiny_entry(entry)?;
    let args = args_opt?;
    let target = extract_dependent_destiny_target(action, args)?;
    Some((action.to_string(), target))
}

/// Returns the target `BallId` required to be materialized for a `DoDestinyUpdate` action, or `None`
/// if the action does not depend on an existing client ball (or carries placeholder empty args).
pub fn extract_dependent_destiny_target(action: &str, args: &PyRep) -> Option<BallId> {
    match action {
        "OnSlimItemChange" => match args {
            PyRep::Tuple(items) | PyRep::List(items) => items
                .first()
                .and_then(|first| positive_i64(first).or_else(|| extract_slim_ball_id(first)))
                .or_else(|| items.get(1).and_then(extract_slim_ball_id)),
            other => extract_slim_ball_id(other).or_else(|| positive_i64(other)),
        },
        "OnSpecialFX"
        | "OnDamageStateChange"
        | "OnFleetDamageStateChange"
        | "OnDroneStateChange"
        | "OnDroneStateChange2"
        | "OnActivatingWarp"
        | "OnDeactivatingWarp"
        | "SetBallMass"
        | "SetBallRadius"
        | "SetBallPosition"
        | "SetBallVelocity"
        | "SetMaxSpeed"
        | "SetSpeedFraction"
        | "Stop"
        | "GotoPoint"
        | "GotoDirection"
        | "FollowBall"
        | "Orbit"
        | "WarpTo"
        | "CloakBall"
        | "UncloakBall"
        | "SetBallFree"
        | "SetBallHarmonic"
        | "SetBallModel"
        | "SetBallInteractive"
        | "SetBallTroll"
        | "SetBallMassive"
        | "SetControllerVariableOnBall"
        | "ScatterEwars" => first_pos_arg(args),
        "OnTarget" => {
            let items = match args {
                PyRep::Tuple(items) | PyRep::List(items) => items,
                _ => return None,
            };
            let mode = items.first().and_then(PyRep::as_str)?;
            if matches!(mode, "add" | "other_add") {
                items.get(2).and_then(positive_i64)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Returns the target `BallId` required to be materialized for a standalone `MachoNet` notification.
pub fn extract_dependent_notification_target(event: &str, args: &[PyRep]) -> Option<BallId> {
    match event {
        "OnSlimItemChange" => args
            .first()
            .and_then(|first| positive_i64(first).or_else(|| extract_slim_ball_id(first)))
            .or_else(|| args.get(1).and_then(extract_slim_ball_id)),
        "OnSpecialFX"
        | "OnDamageStateChange"
        | "OnFleetDamageStateChange"
        | "OnDroneStateChange"
        | "OnDroneStateChange2"
        | "OnActivatingWarp"
        | "OnDeactivatingWarp" => args.first().and_then(positive_i64),
        "OnTarget" => {
            let mode = args.first().and_then(PyRep::as_str)?;
            if matches!(mode, "add" | "otheradd" | "other_add") {
                args.get(1).and_then(positive_i64)
            } else {
                None
            }
        }
        "OnTargetOBO" => {
            let mode = args.first().and_then(PyRep::as_str)?;
            if matches!(mode, "add" | "otheradd" | "other_add") {
                args.get(2).and_then(positive_i64)
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_destiny::{Ball, MovementMode};
    use glam::DVec3;

    fn make_entry(stamp: i64, action: &str, args: Vec<PyRep>) -> PyRep {
        PyRep::Tuple(vec![
            PyRep::Integer(stamp),
            PyRep::Tuple(vec![PyRep::String(action.into()), PyRep::Tuple(args)]),
        ])
    }

    #[test]
    fn entity_extraction_handles_real_destiny_wire_encodings() {
        let mut park = eve_destiny::Ballpark::new(30_000_142);
        let b1 = Ball {
            id: 1001,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let b2 = Ball { id: 1002, ..b1.clone() };
        park.add_ball(b1.clone());
        park.add_ball(b2.clone());

        let set_state_payload = eve_destiny::wire::encode_set_state_payload(&park, 1001);
        let mut ss_ids = Vec::new();
        extract_set_state_entities(&PyRep::Tuple(vec![set_state_payload]), &mut ss_ids);
        assert!(ss_ids.contains(&1001));
        assert!(ss_ids.contains(&1002));

        let add2 = eve_destiny::wire::encode_add_balls2_action(15, &[&b2], Some(&park));
        let (_, action, args_opt) = parse_destiny_entry(&add2).unwrap();
        assert_eq!(action, "AddBalls2");
        let mut add2_ids = Vec::new();
        extract_add_balls_entities(action, args_opt.unwrap(), &mut add2_ids);
        assert_eq!(add2_ids, vec![1002]);
    }

    #[test]
    fn materialization_gate_reorders_same_frame_and_drops_unmaterialized() {
        let fr = FlightRecorder::new();
        let mut guard = SequencerGuardState::default();
        let mut materialized = HashSet::from([1001_i64]);
        let mut events = Vec::new();

        // Premature OnSlimItemChange(1002) before AddBalls2(1002) + unmaterialized OnSpecialFX(9999)
        let mut updates = PyRep::List(vec![
            make_entry(15, "OnSlimItemChange", vec![PyRep::Integer(1002), PyRep::None]),
            make_entry(15, "OnSpecialFX", vec![PyRep::Integer(9999)]),
            make_entry(15, "AddBalls2", vec![PyRep::Integer(1002)]),
        ]);

        let keep = guard.reconcile_destiny_updates_with_gate(
            &mut materialized,
            Some(&fr),
            55,
            1,
            15,
            &mut updates,
            &mut events,
        );
        assert!(keep);
        assert!(materialized.contains(&1002));
        assert!(!materialized.contains(&9999));

        let PyRep::List(items) = &updates else {
            panic!("expected List");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(parse_destiny_entry(&items[0]).unwrap().1, "AddBalls2");
        assert_eq!(parse_destiny_entry(&items[1]).unwrap().1, "OnSlimItemChange");

        assert_eq!(events.len(), 1);
        assert!(events[0].reason.starts_with(REASON_ENTITY_NOT_MATERIALIZED));
        assert_eq!(guard.counters.dropped_dependent_notifications, 1);
        assert_eq!(
            fr.session_sequencer_counters(55)
                .dropped_dependent_by_name
                .get("OnSpecialFX"),
            Some(&1)
        );
    }

    #[test]
    fn all_four_destiny_invariants_pass_and_catch_violations() {
        let fr = FlightRecorder::new();

        // 1. Positive clean sequence across SetState -> AddBalls2 -> RemoveBalls (later stamp) -> SetState reset
        let mut guard = SequencerGuardState {
            mode: InvariantValidatorMode::StrictPanic,
            ..Default::default()
        };
        assert!(guard
            .validate_outgoing_destiny_frame(
                Some(&fr),
                10,
                1,
                100,
                &PyRep::List(vec![make_entry(100, "AddBalls2", vec![PyRep::Integer(500)])]),
            )
            .is_empty());
        assert!(guard
            .validate_outgoing_destiny_frame(
                Some(&fr),
                10,
                1,
                101,
                &PyRep::List(vec![make_entry(
                    101,
                    "RemoveBalls",
                    vec![PyRep::List(vec![PyRep::Integer(500)])]
                )]),
            )
            .is_empty());
        // SetState resets per-entity last_addballs_stamp
        assert!(guard
            .validate_outgoing_destiny_frame(
                Some(&fr),
                10,
                1,
                102,
                &PyRep::List(vec![make_entry(102, "AddBalls2", vec![PyRep::Integer(500)])]),
            )
            .is_empty());
        assert!(guard
            .validate_outgoing_destiny_frame(
                Some(&fr),
                10,
                1,
                103,
                &PyRep::List(vec![
                    make_entry(103, "SetState", vec![PyRep::keyval([("ego", PyRep::Integer(1))])]),
                    make_entry(
                        103,
                        "RemoveBalls",
                        vec![PyRep::List(vec![PyRep::Integer(500)])]
                    ),
                ]),
            )
            .is_empty());
        assert_eq!(guard.counters.invariant_violations, 0);

        // 2. Negative checks in RecordAndEmit (counters + FlightRecorder) and StrictPanic (panic)
        let mut obs_guard = SequencerGuardState {
            mode: InvariantValidatorMode::RecordAndEmit,
            ..Default::default()
        };

        // 2a. single-destiny-stamp-per-notification
        let v1 = obs_guard.validate_outgoing_destiny_frame(
            Some(&fr),
            11,
            1,
            200,
            &PyRep::List(vec![
                make_entry(200, "Stop", vec![PyRep::Integer(1)]),
                make_entry(201, "GotoDirection", vec![PyRep::Integer(1)]),
            ]),
        );
        assert_eq!(v1, vec![INV_SINGLE_STAMP_PER_NOTIFICATION]);

        // 2b. group-stamp-matches-update-stamp
        let v2 = obs_guard.validate_outgoing_destiny_frame(
            Some(&fr),
            11,
            1,
            205,
            &PyRep::List(vec![make_entry(206, "Stop", vec![PyRep::Integer(1)])]),
        );
        assert_eq!(v2, vec![INV_GROUP_STAMP_MATCHES_UPDATE_STAMP]);

        // 2c. monotonic-destiny-notification-stamp
        let v3 = obs_guard.validate_outgoing_destiny_frame(
            Some(&fr),
            11,
            1,
            204,
            &PyRep::List(vec![make_entry(204, "Stop", vec![PyRep::Integer(1)])]),
        );
        assert_eq!(v3, vec![INV_MONOTONIC_STAMP]);

        // 2d. remove-after-addballs
        assert!(obs_guard
            .validate_outgoing_destiny_frame(
                Some(&fr),
                11,
                1,
                210,
                &PyRep::List(vec![make_entry(210, "AddBalls2", vec![PyRep::Integer(777)])]),
            )
            .is_empty());
        let v4 = obs_guard.validate_outgoing_destiny_frame(
            Some(&fr),
            11,
            1,
            210,
            &PyRep::List(vec![make_entry(
                210,
                "RemoveBalls",
                vec![PyRep::List(vec![PyRep::Integer(777)])],
            )]),
        );
        assert_eq!(v4, vec![INV_REMOVE_AFTER_ADDBALLS]);

        let c11 = fr.session_sequencer_counters(11);
        assert_eq!(c11.invariant_violations, 4);
        assert_eq!(fr.get_session_invariant_violations(11, 20).len(), 4);

        // Verify StrictPanic mode panics loudly
        let panic_res = std::panic::catch_unwind(move || {
            let mut strict = SequencerGuardState {
                mode: InvariantValidatorMode::StrictPanic,
                ..Default::default()
            };
            strict.validate_outgoing_destiny_frame(
                None,
                12,
                1,
                300,
                &PyRep::List(vec![make_entry(301, "Stop", vec![PyRep::Integer(1)])]),
            );
        });
        assert!(panic_res.is_err());
    }
}
