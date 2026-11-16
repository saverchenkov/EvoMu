#![forbid(unsafe_code)]
//! Authoritative V24 Destiny update delivery policy, stamp rules, and coalescing.
//!
//! All timing windows, client hold/fast-forward thresholds, update classes, and
//! echo/monotonicity invariants are defined here as named policy constants and
//! pure functions, matching V24.01 client `code.ccp` (`eve/client/script/remote/michelle.pyj`,
//! `destiny/net/client/_ticker.pyj`, `destiny/net/client/_baseticker.pyj`) and
//! documented in `docs/wiki/ballpark_and_physics/destiny_simulation_loop.md`.

use std::collections::{BTreeMap, HashMap};

use eve_destiny::{BallId, DVec3, DestinyEvent, MovementMode};
use eve_proto::PyRep;

/// Destiny simulation tick cadence in seconds (`const.simulationTimeStep` = 1 s).
///
/// Citation: `docs/wiki/ballpark_and_physics/destiny_simulation_loop.md` (Section: Tick Loop & V24 Client Timing)
/// and V24 `code.ccp` `eve/client/script/remote/michelle.pyj::Park.__init__`.
pub const DESTINY_STAMP_INTERVAL_SECS: u64 = 1;

/// Maximum forward lead (in ticks) allowed for an owner's direct command echo
/// above the owner's visible simulation stamp (`0..=1` ticks).
///
/// Direct RPC command echoes execute at live scene tick (`0` lead on sync flush)
/// or at most `OWNER_DIRECT_ECHO_LEAD_TICKS` ahead so the client never exceeds
/// the held-future window.
pub const OWNER_DIRECT_ECHO_LEAD_TICKS: u32 = 1;

/// Threshold (in ticks ahead of client `currentTime`) below which the V24
/// client holds future updates in `history` until `currentTime` catches up.
///
/// Measured V24 `code.ccp` `michelle.pyj::Park.DoPreTick` & `destiny/net/client/_ticker.pyj::do_pre_tick`:
/// `if eventStamp > currentTime and (eventStamp - currentTime) < 3: hold in history`.
pub const HELD_FUTURE_HOLD_THRESHOLD_TICKS: u32 = 3;

/// Maximum future lead (in ticks) that the V24 client holds smoothly (`1..=2` ticks).
pub const HELD_FUTURE_MAX_LEAD_TICKS: u32 = HELD_FUTURE_HOLD_THRESHOLD_TICKS - 1;

/// Minimum future lead (in ticks) that bypasses the V24 hold buffer and triggers
/// immediate `SynchroniseToSimulationTime` fast-forward (`>= 3` ticks ahead).
pub const POST_HELD_LEAD_TICKS: u32 = HELD_FUTURE_HOLD_THRESHOLD_TICKS;

/// Maximum allowed lead (in ticks) for owner critical movement/ship-prime updates
/// above the owner's visible stamp (`HELD_FUTURE_MAX_LEAD_TICKS` = 2 ticks).
pub const OWNER_CRITICAL_MOVEMENT_MAX_LEAD_TICKS: u32 = HELD_FUTURE_MAX_LEAD_TICKS;

/// Maximum number of snapshots retained by V24 `Park.StoreState` before pruning
/// (`michelle.pyj::Park.StoreState` keeps `states[:1] + states[3:3] + states[-5:]` when `len > 10`, retaining 6 snapshots).
pub const CLIENT_STATE_HISTORY_MAX_LEN: usize = 10;

/// Periodic sub-warp correction interval for the ship owner (0 = event-driven, no periodic spam).
pub const OWNER_SUBWARP_CORRECTION_CADENCE_TICKS: u32 = 0;

/// Periodic sub-warp correction interval for observers (0 = event-driven, no periodic spam).
pub const OBSERVER_SUBWARP_CORRECTION_CADENCE_TICKS: u32 = 0;

/// Periodic in-warp correction interval for the ship owner (0 = event-driven, no periodic spam).
pub const OWNER_WARP_CORRECTION_CADENCE_TICKS: u32 = 0;

/// Periodic in-warp correction interval for observers (0 = event-driven, no periodic spam).
pub const OBSERVER_WARP_CORRECTION_CADENCE_TICKS: u32 = 0;

/// Maximum acceptable observer delivery window after an owner command (0..=1 ticks).
pub const OBSERVER_MOVEMENT_MAX_LAG_TICKS: u32 = 1;

/// Cosine similarity threshold above which two `CmdGotoDirection` headings are
/// considered identical (`cos >= 0.9998`, ~1.15 deg), suppressing duplicate emissions.
pub const GOTO_DIRECTION_DUPLICATE_COSINE_THRESHOLD: f64 =
    eve_destiny::DESTINY_GOTO_DUPLICATE_COSINE_THRESHOLD;

/// Eight V24 Destiny delivery update classes controlling restamp, coalescing,
/// and lead caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateClass {
    /// `SetState` and full ballpark state resets.
    StateReset,
    /// Initial `AddBalls2` / `AddBalls` / `AddBall` grid acquisition.
    BootstrapAcquire,
    /// Pilot-issued steering/speed commands (`GotoDirection`, `Stop`, `FollowBall`, `Orbit`, `SetSpeedFraction`, `WarpTo`, `GotoPoint`).
    OwnerPilotCommand,
    /// Owner's own missile launch/state lifecycle events.
    OwnerMissileLifecycle,
    /// Observer-visible missile launch/state lifecycle events.
    ObserverMissileLifecycle,
    /// Critical movement state / ship prime (`SetBallPosition`, `SetBallVelocity`, `SetMaxSpeed`, `SetBallMassive`, `CloakBall`).
    CriticalMovementOrShipPrime,
    /// Non-critical combat / FX / slim notifications (`OnDamageStateChange`, `OnSpecialFX`, `OnSlimItemChange`, etc.).
    CombatNoncritical,
    /// Entity teardown / destruction (`RemoveBalls`, `RemoveBall`, `TerminalPlayDestructionEffect`).
    DestructionTeardown,
}

/// Delivery constraints for a specific [`UpdateClass`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryClassPolicy {
    pub class: UpdateClass,
    /// Whether an already-queued update below `last_sent_stamp` may be restamped forward to `last_sent_stamp`.
    pub allow_forward_restamp: bool,
    /// Whether same-stamp updates of this class are merged into one `DoDestinyUpdate` frame.
    pub coalesce_same_stamp: bool,
    /// Whether duplicate same-tick commands coalesce (owner keeps first, observers keep latest).
    pub coalesce_same_tick_steering: bool,
    /// Maximum ticks ahead of `owner_visible_stamp` permitted when targeting the owner session (`None` = uncapped).
    pub max_owner_lead_ticks: Option<u32>,
}

/// Returns the authoritative delivery policy for `class`.
pub const fn policy_for(class: UpdateClass) -> DeliveryClassPolicy {
    match class {
        UpdateClass::StateReset => DeliveryClassPolicy {
            class,
            allow_forward_restamp: false,
            coalesce_same_stamp: false,
            coalesce_same_tick_steering: false,
            max_owner_lead_ticks: Some(0),
        },
        UpdateClass::BootstrapAcquire => DeliveryClassPolicy {
            class,
            allow_forward_restamp: false,
            coalesce_same_stamp: true,
            coalesce_same_tick_steering: false,
            max_owner_lead_ticks: Some(0),
        },
        UpdateClass::OwnerPilotCommand => DeliveryClassPolicy {
            class,
            allow_forward_restamp: true,
            coalesce_same_stamp: true,
            coalesce_same_tick_steering: true,
            max_owner_lead_ticks: Some(OWNER_DIRECT_ECHO_LEAD_TICKS),
        },
        UpdateClass::OwnerMissileLifecycle => DeliveryClassPolicy {
            class,
            allow_forward_restamp: true,
            coalesce_same_stamp: true,
            coalesce_same_tick_steering: false,
            max_owner_lead_ticks: Some(OWNER_DIRECT_ECHO_LEAD_TICKS),
        },
        UpdateClass::ObserverMissileLifecycle => DeliveryClassPolicy {
            class,
            allow_forward_restamp: true,
            coalesce_same_stamp: true,
            coalesce_same_tick_steering: false,
            max_owner_lead_ticks: None,
        },
        UpdateClass::CriticalMovementOrShipPrime => DeliveryClassPolicy {
            class,
            allow_forward_restamp: true,
            coalesce_same_stamp: true,
            coalesce_same_tick_steering: false,
            max_owner_lead_ticks: Some(OWNER_CRITICAL_MOVEMENT_MAX_LEAD_TICKS),
        },
        UpdateClass::CombatNoncritical => DeliveryClassPolicy {
            class,
            allow_forward_restamp: true,
            coalesce_same_stamp: true,
            coalesce_same_tick_steering: false,
            max_owner_lead_ticks: None,
        },
        UpdateClass::DestructionTeardown => DeliveryClassPolicy {
            class,
            allow_forward_restamp: true,
            coalesce_same_stamp: true,
            coalesce_same_tick_steering: false,
            max_owner_lead_ticks: None,
        },
    }
}

/// Classifies a V24 Destiny wire action name into its [`UpdateClass`].
pub fn classify_destiny_action(action_name: &str, is_owner: bool, is_missile: bool) -> UpdateClass {
    match action_name {
        "SetState" => UpdateClass::StateReset,
        "AddBalls" | "AddBalls2" | "AddBall" => {
            if is_missile {
                if is_owner {
                    UpdateClass::OwnerMissileLifecycle
                } else {
                    UpdateClass::ObserverMissileLifecycle
                }
            } else {
                UpdateClass::BootstrapAcquire
            }
        }
        "RemoveBall"
        | "RemoveBalls"
        | "TerminalPlayDestructionEffect"
        | "TerminalExplosion" => {
            if is_missile && action_name == "RemoveBalls" {
                if is_owner {
                    UpdateClass::OwnerMissileLifecycle
                } else {
                    UpdateClass::ObserverMissileLifecycle
                }
            } else {
                UpdateClass::DestructionTeardown
            }
        }
        "GotoDirection" | "GotoPoint" | "FollowBall" | "Orbit" | "Stop" | "SetSpeedFraction"
        | "WarpTo" => {
            if is_owner {
                UpdateClass::OwnerPilotCommand
            } else {
                UpdateClass::CriticalMovementOrShipPrime
            }
        }
        "SetBallPosition" | "SetBallVelocity" | "SetMaxSpeed" | "SetBallMassive"
        | "SetBallFree" | "SetBallInteractive" | "SetBallHarmonic" | "SetBallRadius"
        | "CloakBall" | "UncloakBall" => UpdateClass::CriticalMovementOrShipPrime,
        _ => UpdateClass::CombatNoncritical,
    }
}

/// Returns `true` if `action_name` is a sub-warp movement/kinematic action that
/// must never replay at an older stamp than a fresh `AddBalls2` acquisition for
/// the same entity.
pub fn is_subwarp_movement_action(action_name: &str) -> bool {
    matches!(
        action_name,
        "GotoDirection"
            | "GotoPoint"
            | "FollowBall"
            | "Orbit"
            | "Stop"
            | "SetSpeedFraction"
            | "SetBallPosition"
            | "SetBallVelocity"
            | "SetMaxSpeed"
    )
}

/// Normalizes `v` safely, returning `None` if length squared is non-finite or negligible.
pub fn normalize_direction(v: DVec3) -> Option<DVec3> {
    let len_sq = v.length_squared();
    if !len_sq.is_finite() || len_sq <= 1e-18 {
        return None;
    }
    Some(v / len_sq.sqrt())
}

/// Returns `true` when two headings have cosine similarity `>= GOTO_DIRECTION_DUPLICATE_COSINE_THRESHOLD`.
pub fn directions_within_duplicate_threshold(a: DVec3, b: DVec3) -> bool {
    let (Some(na), Some(nb)) = (normalize_direction(a), normalize_direction(b)) else {
        return false;
    };
    na.dot(nb) >= GOTO_DIRECTION_DUPLICATE_COSINE_THRESHOLD
}

/// Outcome of deciding whether to emit a `GotoDirection` update for owner and observers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SameTickGotoDecision {
    /// `true` if the heading matched the current active heading within threshold and was completely suppressed.
    pub suppressed_duplicate: bool,
    /// Whether an echo should be sent to the ship's owner on this tick.
    pub emit_to_owner: bool,
    /// Whether observers should receive the new heading (replacing any un-drained same-tick `GotoDirection`).
    pub emit_to_observers: bool,
    /// Never queue a deferred second owner echo for a later tick.
    pub queue_deferred_owner_echo: bool,
}

/// Tracks same-tick `CmdGotoDirection` commands per ball so that:
/// - near-duplicate headings (`cos >= 0.9998`) emit nothing to owner or observers;
/// - two distinct headings in the same tick deliver ONLY the first heading to the
///   owner, deliver the latest heading to observers, and never queue a second echo
///   for a later tick.
#[derive(Debug, Default, Clone)]
pub struct SameTickGotoTracker {
    owner_echo_tick_by_ball: HashMap<BallId, u64>,
}

impl SameTickGotoTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Evaluates a `CmdGotoDirection` command for `ball_id` at simulation `tick`.
    pub fn evaluate_goto(
        &mut self,
        ball_id: BallId,
        tick: u64,
        current_mode: &MovementMode,
        current_speed_fraction: f64,
        requested_direction: DVec3,
    ) -> SameTickGotoDecision {
        if current_speed_fraction > 0.0 {
            if let MovementMode::GotoDirection { direction } = current_mode {
                if directions_within_duplicate_threshold(*direction, requested_direction) {
                    return SameTickGotoDecision {
                        suppressed_duplicate: true,
                        emit_to_owner: false,
                        emit_to_observers: false,
                        queue_deferred_owner_echo: false,
                    };
                }
            }
        }

        let owner_already_echoed = self.owner_echo_tick_by_ball.get(&ball_id) == Some(&tick);
        if !owner_already_echoed {
            self.owner_echo_tick_by_ball.insert(ball_id, tick);
        }

        SameTickGotoDecision {
            suppressed_duplicate: false,
            emit_to_owner: !owner_already_echoed,
            emit_to_observers: true,
            queue_deferred_owner_echo: false,
        }
    }

    /// Discards tracking entries from earlier ticks.
    pub fn prune_before(&mut self, current_tick: u64) {
        self.owner_echo_tick_by_ball
            .retain(|_, tick| *tick >= current_tick);
    }

    /// Clears same-tick tracking state for `ball_id` (e.g., on despawn, stop, or warp reset).
    pub fn clear_ball(&mut self, ball_id: BallId) {
        self.owner_echo_tick_by_ball.remove(&ball_id);
    }
}

/// Plans the exact V24 event sequence for `CmdStop` on ONE stamp.
///
/// - If already `MovementMode::Stop` and `speed_fraction <= 0.0` (and not cancelling
///   an aligning warp), returns an empty vector (emits nothing).
/// - Otherwise emits `[SetSpeedFraction(0.0), Stop, SetBallVelocity(pre_stop_velocity)]`.
pub fn plan_stop_events(
    ball_id: BallId,
    current_mode: &MovementMode,
    current_speed_fraction: f64,
    pre_stop_velocity: DVec3,
    cancelling_align_warp: bool,
) -> Vec<DestinyEvent> {
    if !cancelling_align_warp
        && matches!(current_mode, MovementMode::Stop)
        && current_speed_fraction <= 0.0
    {
        return Vec::new();
    }
    vec![
        DestinyEvent::SetSpeedFraction {
            ball_id,
            fraction: 0.0,
        },
        DestinyEvent::Stop { ball_id },
        DestinyEvent::SetBallVelocity {
            ball_id,
            velocity: pre_stop_velocity.to_array(),
        },
    ]
}

/// Plans the V24 Approach (`CmdFollowBall`) events on ONE stamp:
/// returns `(effective_speed_fraction, [Follow, SetSpeedFraction])`.
pub fn plan_approach_events(
    ball_id: BallId,
    target_id: BallId,
    range: f64,
    current_speed_fraction: f64,
) -> (f64, Vec<DestinyEvent>) {
    let effective_fraction = if current_speed_fraction <= 0.0 {
        1.0
    } else {
        current_speed_fraction
    };
    let events = vec![
        DestinyEvent::Follow {
            ball_id,
            target_id,
            range,
        },
        DestinyEvent::SetSpeedFraction {
            ball_id,
            fraction: effective_fraction,
        },
    ];
    (effective_fraction, events)
}

/// Clamps an owner critical-movement or direct-echo stamp so it never exceeds
/// `owner_visible_stamp + max_lead_ticks`.
pub const fn clamp_owner_critical_stamp(
    proposed_stamp: u32,
    owner_visible_stamp: u32,
    max_lead_ticks: u32,
) -> u32 {
    let cap = owner_visible_stamp.saturating_add(max_lead_ticks);
    if proposed_stamp > cap {
        cap
    } else {
        proposed_stamp
    }
}

/// Validates that an observer movement update stamp arrives within `[owner_cmd_stamp, owner_cmd_stamp + 1]`.
pub const fn validate_observer_movement_window(owner_cmd_stamp: u32, observer_stamp: u32) -> bool {
    observer_stamp >= owner_cmd_stamp
        && observer_stamp <= owner_cmd_stamp.saturating_add(OBSERVER_MOVEMENT_MAX_LAG_TICKS)
}

/// Reason a candidate `DoDestinyUpdate` batch or action was rejected by session delivery policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StampRejectionReason {
    /// Batch stamp is strictly less than the session's `last_sent_stamp`.
    BelowLastSentStamp {
        proposed_stamp: u32,
        last_sent_stamp: u32,
    },
    /// Batch stamp is strictly less than the active ballpark `set_state_stamp`.
    BelowSetStateStamp {
        proposed_stamp: u32,
        set_state_stamp: u32,
    },
    /// Owner critical movement stamp exceeded `owner_visible_stamp + max_lead`.
    OwnerStampExceededVisiblePlusLead {
        proposed_stamp: u32,
        owner_visible_stamp: u32,
        max_lead_ticks: u32,
    },
    /// Sub-warp movement replay attempted for `ball_id` at an older stamp than its live `AddBalls2` acquisition.
    SubwarpReplayAfterFreshAcquire {
        ball_id: BallId,
        action_stamp: u32,
        acquired_stamp: u32,
    },
}

/// Recorded rejection event for diagnostics / flight recorder observability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StampRejectionRecord {
    pub session_id: i64,
    pub reason: StampRejectionReason,
}

/// One encoded `(stamp, (action_name, args))` entry inside a `DoDestinyUpdate` batch.
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyDestinyFrame {
    pub stamp: u32,
    pub updates: Vec<PyRep>,
    pub has_set_state: bool,
    pub wait_for_bubble: bool,
}

fn extract_slim_item_id(rep: &PyRep) -> Option<BallId> {
    match rep {
        PyRep::Integer(id) => Some(*id),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| match (k, v) {
            (PyRep::String(key), PyRep::Integer(id)) if key == "itemID" => Some(*id),
            _ => None,
        }),
        PyRep::KeyVal(entries) => match entries.get("itemID") {
            Some(PyRep::Integer(id)) => Some(*id),
            _ => None,
        },
        PyRep::Object { args, .. } => extract_slim_item_id(args),
        PyRep::Tuple(items) => items.first().and_then(extract_slim_item_id),
        _ => None,
    }
}

fn extract_action_ball_ids(action_name: &str, args: &PyRep) -> Vec<BallId> {
    let args_slice: &[PyRep] = match args {
        PyRep::Tuple(items) | PyRep::List(items) => items.as_slice(),
        _ => return Vec::new(),
    };
    match action_name {
        "SetState" => {
            if let Some(payload) = args_slice.first() {
                let ego = match payload {
                    PyRep::KeyVal(entries) => match entries.get("ego") {
                        Some(PyRep::Integer(id)) => Some(*id),
                        _ => None,
                    },
                    PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| match (k, v) {
                        (PyRep::String(key), PyRep::Integer(id)) if key == "ego" => Some(*id),
                        _ => None,
                    }),
                    PyRep::Integer(id) => Some(*id),
                    _ => None,
                };
                if let Some(id) = ego {
                    return vec![id];
                }
            }
            Vec::new()
        }
        "AddBall" => args_slice
            .first()
            .and_then(extract_slim_item_id)
            .into_iter()
            .collect(),
        "AddBalls" => {
            let inner = match args_slice {
                [PyRep::Tuple(inner)] => inner.as_slice(),
                other => other,
            };
            if let Some(PyRep::List(slims)) = inner.get(1) {
                let ids: Vec<BallId> = slims.iter().filter_map(extract_slim_item_id).collect();
                if !ids.is_empty() {
                    return ids;
                }
            }
            if let Some(PyRep::Dict(dmg)) = inner.get(2) {
                let ids: Vec<BallId> = dmg
                    .iter()
                    .filter_map(|(k, _)| match k {
                        PyRep::Integer(id) => Some(*id),
                        _ => None,
                    })
                    .collect();
                if !ids.is_empty() {
                    return ids;
                }
            }
            args_slice
                .first()
                .and_then(extract_slim_item_id)
                .into_iter()
                .collect()
        }
        "AddBalls2" => {
            let inner = match args_slice {
                [PyRep::Tuple(inner)] => inner.as_slice(),
                other => other,
            };
            if let Some(PyRep::List(extra)) = inner.get(1) {
                let ids: Vec<BallId> = extra.iter().filter_map(extract_slim_item_id).collect();
                if !ids.is_empty() {
                    return ids;
                }
            }
            args_slice
                .first()
                .and_then(extract_slim_item_id)
                .into_iter()
                .collect()
        }
        "RemoveBalls" => {
            if let Some(PyRep::List(ids) | PyRep::Tuple(ids)) = args_slice.first() {
                return ids
                    .iter()
                    .filter_map(|v| match v {
                        PyRep::Integer(id) => Some(*id),
                        _ => None,
                    })
                    .collect();
            }
            match args_slice.first() {
                Some(PyRep::Integer(id)) => vec![*id],
                _ => Vec::new(),
            }
        }
        "OnTarget" => match args_slice.get(1) {
            Some(PyRep::Integer(id)) => vec![*id],
            _ => Vec::new(),
        },
        _ => match args_slice.first() {
            Some(PyRep::Integer(id)) => vec![*id],
            _ => Vec::new(),
        },
    }
}

/// Extracts `(optional_inner_stamp, action_name, affected_ball_ids)` from a single
/// `(stamp, (action_name, args))` or `(action_name, args)` `PyRep` entry when possible,
/// supporting both real V24 wire payloads (`SetState` `ego`, `AddBall`/`AddBalls`/`AddBalls2`
/// `SlimItem` dicts/objects, `RemoveBalls` lists) and synthetic integer tuples.
pub fn extract_destiny_entry_ball_ids(entry: &PyRep) -> Option<(Option<u32>, &str, Vec<BallId>)> {
    let PyRep::Tuple(outer) = entry else {
        return None;
    };
    if outer.len() == 2 {
        if let (PyRep::Integer(stamp), PyRep::Tuple(func_tuple)) = (&outer[0], &outer[1]) {
            if let Some(PyRep::String(name)) = func_tuple.first() {
                let ids = func_tuple
                    .get(1)
                    .map(|args| extract_action_ball_ids(name.as_str(), args))
                    .unwrap_or_default();
                return Some((u32::try_from(*stamp).ok(), name.as_str(), ids));
            }
        }
        if let PyRep::String(name) = &outer[0] {
            let ids = extract_action_ball_ids(name.as_str(), &outer[1]);
            return Some((None, name.as_str(), ids));
        }
    }
    None
}

/// Extracts `(optional_inner_stamp, action_name, optional_primary_ball_id)` from a single
/// `(stamp, (action_name, args))` or `(action_name, args)` `PyRep` entry when possible.
pub fn parse_destiny_entry_summary(entry: &PyRep) -> Option<(Option<u32>, &str, Option<BallId>)> {
    let (stamp, name, ids) = extract_destiny_entry_ball_ids(entry)?;
    Some((stamp, name, ids.into_iter().next()))
}

/// Per-session `DoDestinyUpdate` monotonicity, coalescing, freshness, and lead guard.
///
/// Intended as the pure policy backend invoked before/alongside `SessionWireState`
/// in `outbound_sequencer.rs`.
#[derive(Debug, Clone)]
pub struct SessionStampPolicyState {
    pub session_id: i64,
    pub owner_ship_id: Option<BallId>,
    pub owner_visible_stamp: Option<u32>,
    pub last_sent_stamp: Option<u32>,
    pub set_state_stamp: Option<u32>,
    /// Live scene tick at which each `ball_id` was acquired via `AddBalls2` / `AddBalls` / `AddBall`.
    pub fresh_acquire_stamp_by_ball: BTreeMap<BallId, u32>,
    /// Rejections recorded with structured reasons for observability/tests.
    pub rejections: Vec<StampRejectionRecord>,
}

impl SessionStampPolicyState {
    fn entry_allows_forward_restamp(owner_ship_id: Option<BallId>, entry: &PyRep) -> bool {
        extract_destiny_entry_ball_ids(entry).is_none_or(|(_, action_name, ball_ids)| {
            let is_owner = owner_ship_id.is_some_and(|owner_id| ball_ids.contains(&owner_id));
            policy_for(classify_destiny_action(action_name, is_owner, false)).allow_forward_restamp
        })
    }

    pub fn new(session_id: i64, owner_ship_id: Option<BallId>) -> Self {
        Self {
            session_id,
            owner_ship_id,
            owner_visible_stamp: None,
            last_sent_stamp: None,
            set_state_stamp: None,
            fresh_acquire_stamp_by_ball: BTreeMap::new(),
            rejections: Vec::new(),
        }
    }

    /// Updates `owner_visible_stamp` as the simulation clock advances.
    pub fn advance_owner_visible_stamp(&mut self, stamp: u32) {
        self.owner_visible_stamp = Some(
            self.owner_visible_stamp
                .map_or(stamp, |prev| prev.max(stamp)),
        );
    }

    /// Records a fresh `AddBalls2` acquisition for `ball_id` at `live_scene_stamp`.
    pub fn record_fresh_acquire(&mut self, ball_id: BallId, live_scene_stamp: u32) {
        self.fresh_acquire_stamp_by_ball
            .entry(ball_id)
            .and_modify(|prev| *prev = (*prev).max(live_scene_stamp))
            .or_insert(live_scene_stamp);
    }

    /// Validates, filters, and merges candidate `DoDestinyUpdate` frames in one dispatch:
    /// 1. Rejects any frame below `set_state_stamp` or `last_sent_stamp` (unless `has_set_state` resets baseline) and records the exact [`StampRejectionReason`].
    /// 2. Drops sub-warp movement replays for an entity older than its recorded `AddBalls2` `acquired_stamp`.
    /// 3. Enforces `stamp <= owner_visible_stamp + max_lead` for owner critical movement / pilot commands (either clamping or rejecting when `strict_reject_excess_lead` is set).
    /// 4. Merges equal-stamp batches inside the same dispatch into a single [`PolicyDestinyFrame`].
    pub fn process_dispatch_batches(
        &mut self,
        batches: Vec<PolicyDestinyFrame>,
        strict_reject_excess_lead: bool,
    ) -> Vec<PolicyDestinyFrame> {
        let mut working_set_state_stamp = self.set_state_stamp;
        let mut working_last_sent_stamp = self.last_sent_stamp;
        let mut working_fresh_acquires = self.fresh_acquire_stamp_by_ball.clone();
        let mut emitted: Vec<PolicyDestinyFrame> = Vec::new();
        let mut merged_by_stamp: BTreeMap<u32, PolicyDestinyFrame> = BTreeMap::new();

        for batch in batches {
            let orig_stamp = batch.stamp;
            let mut stamp = orig_stamp;
            let batch_has_set_state = batch.has_set_state
                || batch.updates.iter().any(|upd| {
                    extract_destiny_entry_ball_ids(upd)
                        .is_some_and(|(_, name, _)| name == "SetState")
                });

            if !batch_has_set_state {
                if let Some(ss) = working_set_state_stamp {
                    if stamp < ss {
                        self.rejections.push(StampRejectionRecord {
                            session_id: self.session_id,
                            reason: StampRejectionReason::BelowSetStateStamp {
                                proposed_stamp: stamp,
                                set_state_stamp: ss,
                            },
                        });
                        continue;
                    }
                }
                if let Some(last) = working_last_sent_stamp {
                    if stamp < last {
                        self.rejections.push(StampRejectionRecord {
                            session_id: self.session_id,
                            reason: StampRejectionReason::BelowLastSentStamp {
                                proposed_stamp: stamp,
                                last_sent_stamp: last,
                            },
                        });
                        continue;
                    }
                }
            }

            let mut batch_fresh_acquires = if batch_has_set_state {
                BTreeMap::new()
            } else {
                working_fresh_acquires.clone()
            };
            let mut filtered_updates: Vec<PyRep> = Vec::with_capacity(batch.updates.len());
            let mut batch_rejected_for_lead = false;
            // Compute up front across the whole frame so an early OwnerPilotCommand cannot
            // clamp `stamp` before a later SetState/AddBalls2 disables forward restamping.
            let batch_allow_forward_restamp = batch
                .updates
                .iter()
                .all(|upd| Self::entry_allows_forward_restamp(self.owner_ship_id, upd));

            for entry in batch.updates {
                let mut drop_entry = false;
                if let Some((entry_stamp_opt, action_name, ball_ids)) =
                    extract_destiny_entry_ball_ids(&entry)
                {
                    let effective_stamp = entry_stamp_opt.unwrap_or(stamp);
                    let is_owner = self
                        .owner_ship_id
                        .is_some_and(|owner_id| ball_ids.contains(&owner_id));
                    let class = classify_destiny_action(action_name, is_owner, false);
                    let pol = policy_for(class);

                    if matches!(action_name, "AddBalls2" | "AddBalls" | "AddBall") {
                        for &bid in &ball_ids {
                            batch_fresh_acquires
                                .entry(bid)
                                .and_modify(|prev| *prev = (*prev).max(effective_stamp))
                                .or_insert(effective_stamp);
                        }
                    } else if is_subwarp_movement_action(action_name) {
                        for &bid in &ball_ids {
                            if let Some(&acq_stamp) = batch_fresh_acquires.get(&bid) {
                                if effective_stamp < acq_stamp {
                                    self.rejections.push(StampRejectionRecord {
                                        session_id: self.session_id,
                                        reason:
                                            StampRejectionReason::SubwarpReplayAfterFreshAcquire {
                                                ball_id: bid,
                                                action_stamp: effective_stamp,
                                                acquired_stamp: acq_stamp,
                                            },
                                    });
                                    drop_entry = true;
                                    break;
                                }
                            }
                        }
                    }

                    if drop_entry {
                        continue;
                    }

                    if is_owner
                        && matches!(
                            class,
                            UpdateClass::OwnerPilotCommand
                                | UpdateClass::CriticalMovementOrShipPrime
                        )
                    {
                        if let Some(visible) = self.owner_visible_stamp {
                            let max_lead = pol
                                .max_owner_lead_ticks
                                .unwrap_or(OWNER_CRITICAL_MOVEMENT_MAX_LEAD_TICKS);
                            let cap = visible.saturating_add(max_lead);
                            let check_stamp = effective_stamp.max(stamp);
                            if check_stamp > cap {
                                if strict_reject_excess_lead || !batch_allow_forward_restamp {
                                    self.rejections.push(StampRejectionRecord {
                                        session_id: self.session_id,
                                        reason: StampRejectionReason::OwnerStampExceededVisiblePlusLead {
                                            proposed_stamp: check_stamp,
                                            owner_visible_stamp: visible,
                                            max_lead_ticks: max_lead,
                                        },
                                    });
                                    batch_rejected_for_lead = true;
                                    break;
                                } else {
                                    stamp =
                                        clamp_owner_critical_stamp(check_stamp, visible, max_lead)
                                            .min(stamp);
                                }
                            }
                        }
                    }
                }
                filtered_updates.push(entry);
            }

            if batch_rejected_for_lead || filtered_updates.is_empty() {
                continue;
            }

            if stamp != orig_stamp && !batch_has_set_state {
                if let Some(ss) = working_set_state_stamp {
                    if stamp < ss {
                        self.rejections.push(StampRejectionRecord {
                            session_id: self.session_id,
                            reason: StampRejectionReason::BelowSetStateStamp {
                                proposed_stamp: stamp,
                                set_state_stamp: ss,
                            },
                        });
                        continue;
                    }
                }
                if let Some(last) = working_last_sent_stamp {
                    if stamp < last {
                        self.rejections.push(StampRejectionRecord {
                            session_id: self.session_id,
                            reason: StampRejectionReason::BelowLastSentStamp {
                                proposed_stamp: stamp,
                                last_sent_stamp: last,
                            },
                        });
                        continue;
                    }
                }
            }

            // Re-check sub-warp freshness against the normalized frame `stamp` whenever
            // clamping or inner-entry normalization will shift a sub-warp entry to `stamp`.
            let mut post_norm_updates = Vec::with_capacity(filtered_updates.len());
            for upd in filtered_updates {
                let mut stale = false;
                if let Some((_, action_name, ball_ids)) = extract_destiny_entry_ball_ids(&upd) {
                    if is_subwarp_movement_action(action_name) {
                        for &bid in &ball_ids {
                            if let Some(&acq_stamp) = batch_fresh_acquires.get(&bid) {
                                if stamp < acq_stamp {
                                    self.rejections.push(StampRejectionRecord {
                                        session_id: self.session_id,
                                        reason: StampRejectionReason::SubwarpReplayAfterFreshAcquire {
                                            ball_id: bid,
                                            action_stamp: stamp,
                                            acquired_stamp: acq_stamp,
                                        },
                                    });
                                    stale = true;
                                    break;
                                }
                            }
                        }
                    }
                }
                if !stale {
                    post_norm_updates.push(upd);
                }
            }
            filtered_updates = post_norm_updates;
            if filtered_updates.is_empty() {
                continue;
            }

            for upd in &mut filtered_updates {
                let entry_allows_restamp =
                    Self::entry_allows_forward_restamp(self.owner_ship_id, upd);
                if let PyRep::Tuple(outer) = upd {
                    if outer.len() == 2
                        && matches!(outer[0], PyRep::Integer(_))
                        && matches!(outer[1], PyRep::Tuple(_))
                    {
                        if let PyRep::Integer(inner_stamp) = outer[0] {
                            if inner_stamp != i64::from(stamp) && entry_allows_restamp {
                                outer[0] = PyRep::Integer(i64::from(stamp));
                            }
                        }
                    }
                }
            }

            working_fresh_acquires = batch_fresh_acquires;

            if batch_has_set_state {
                // V24 `Park.FlushState` / `RealFlushState` inspect only `state[0][1][0] == 'SetState'`,
                // and `UpdateClass::StateReset` has `coalesce_same_stamp: false`.
                let mut set_state_entries = Vec::new();
                let mut follow_entries = Vec::new();
                for upd in filtered_updates {
                    let is_ss = extract_destiny_entry_ball_ids(&upd)
                        .is_some_and(|(_, name, _)| name == "SetState");
                    if is_ss {
                        set_state_entries.push(upd);
                    } else {
                        follow_entries.push(upd);
                    }
                }
                set_state_entries.extend(follow_entries);

                let pending_keys: Vec<u32> = merged_by_stamp.keys().copied().collect();
                for k in pending_keys {
                    if let Some(frame) = merged_by_stamp.remove(&k) {
                        if k < stamp {
                            emitted.push(frame);
                        }
                    }
                }

                working_set_state_stamp = Some(stamp);
                working_last_sent_stamp = Some(
                    working_last_sent_stamp.map_or(stamp, |prev| prev.max(stamp)),
                );
                emitted.push(PolicyDestinyFrame {
                    stamp,
                    updates: set_state_entries,
                    has_set_state: true,
                    wait_for_bubble: batch.wait_for_bubble,
                });
            } else if let Some(existing) = merged_by_stamp.get_mut(&stamp) {
                existing.updates.extend(filtered_updates);
                existing.wait_for_bubble |= batch.wait_for_bubble;
            } else {
                merged_by_stamp.insert(
                    stamp,
                    PolicyDestinyFrame {
                        stamp,
                        updates: filtered_updates,
                        has_set_state: false,
                        wait_for_bubble: batch.wait_for_bubble,
                    },
                );
            }
        }

        emitted.extend(merged_by_stamp.into_values());
        self.set_state_stamp = working_set_state_stamp;
        self.fresh_acquire_stamp_by_ball = working_fresh_acquires;
        if let Some(last) = emitted.last() {
            self.last_sent_stamp = Some(
                self.last_sent_stamp
                    .map_or(last.stamp, |prev| prev.max(last.stamp)),
            );
        }
        emitted
    }
}

/// Validates that a warp grid hand-off emits destination acquisition (`AddBalls` /
/// `AddBalls2`) and departure removal (`RemoveBalls` / `RemoveBall`) on ONE stamp
/// while keeping static scenery and `mover_id` out of removal lists.
pub fn validate_warp_handoff_single_stamp(
    mover_id: BallId,
    static_scenery_ids: &[BallId],
    mover_grid_events: &[(u64, &DestinyEvent)],
    removed_ball_ids_during_handoff: &[BallId],
) -> bool {
    let mut add_stamps = Vec::new();
    let mut remove_stamps = Vec::new();
    let mut event_removed_ids = Vec::new();
    for (tick, ev) in mover_grid_events {
        match ev {
            DestinyEvent::AddBalls { .. } | DestinyEvent::AddBalls2 { .. } => {
                add_stamps.push(*tick);
            }
            DestinyEvent::RemoveBalls { ball_ids } => {
                remove_stamps.push(*tick);
                event_removed_ids.extend(ball_ids.iter().copied());
            }
            DestinyEvent::RemoveBall { ball_id } => {
                remove_stamps.push(*tick);
                event_removed_ids.push(*ball_id);
            }
            _ => {}
        }
    }
    let scenery_and_mover_kept = static_scenery_ids
        .iter()
        .chain(std::iter::once(&mover_id))
        .all(|sid| {
            !removed_ball_ids_during_handoff.contains(sid) && !event_removed_ids.contains(sid)
        });
    if !scenery_and_mover_kept || add_stamps.is_empty() || remove_stamps.is_empty() {
        return false;
    }
    let target = add_stamps[0];
    add_stamps.iter().all(|s| *s == target) && remove_stamps.iter().all(|s| *s == target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_entry(stamp: u32, action: &str, ball_id: BallId) -> PyRep {
        PyRep::Tuple(vec![
            PyRep::Integer(stamp as i64),
            PyRep::Tuple(vec![
                PyRep::String(action.to_string()),
                PyRep::Tuple(vec![PyRep::Integer(ball_id)]),
            ]),
        ])
    }

    #[test]
    fn constants_match_v24_client_measurements() {
        assert_eq!(DESTINY_STAMP_INTERVAL_SECS, 1);
        assert_eq!(OWNER_DIRECT_ECHO_LEAD_TICKS, 1);
        assert_eq!(HELD_FUTURE_MAX_LEAD_TICKS, 2);
        assert_eq!(POST_HELD_LEAD_TICKS, 3);
        assert_eq!(HELD_FUTURE_HOLD_THRESHOLD_TICKS, 3);
        assert_eq!(CLIENT_STATE_HISTORY_MAX_LEN, 10);
        assert_eq!(OWNER_SUBWARP_CORRECTION_CADENCE_TICKS, 0);
        assert_eq!(OBSERVER_SUBWARP_CORRECTION_CADENCE_TICKS, 0);
        assert_eq!(OWNER_WARP_CORRECTION_CADENCE_TICKS, 0);
        assert_eq!(OBSERVER_WARP_CORRECTION_CADENCE_TICKS, 0);
        assert!((GOTO_DIRECTION_DUPLICATE_COSINE_THRESHOLD - 0.9998).abs() < 1e-12);
    }

    #[test]
    fn near_duplicate_steer_suppressed_and_same_tick_distinct_keeps_first_for_owner() {
        let mut tracker = SameTickGotoTracker::new();
        let mode = MovementMode::GotoDirection {
            direction: DVec3::new(1.0, 0.0, 0.0),
        };
        // ~0.5 deg offset -> cos ~ 0.99996 >= 0.9998 -> suppressed
        let near = DVec3::new(0.99996, 0.0087, 0.0);
        let dup = tracker.evaluate_goto(9001, 100, &mode, 1.0, near);
        assert!(dup.suppressed_duplicate);
        assert!(!dup.emit_to_owner);
        assert!(!dup.emit_to_observers);

        // Distinct heading #1 at tick 100 -> owner + observers
        let h1 = DVec3::new(0.0, 1.0, 0.0);
        let d1 = tracker.evaluate_goto(9001, 100, &mode, 1.0, h1);
        assert!(!d1.suppressed_duplicate);
        assert!(d1.emit_to_owner);
        assert!(d1.emit_to_observers);
        assert!(!d1.queue_deferred_owner_echo);

        // Distinct heading #2 at SAME tick 100 -> observer gets latest, owner gets NO second echo
        let h2 = DVec3::new(0.0, 0.0, 1.0);
        let d2 = tracker.evaluate_goto(9001, 100, &MovementMode::GotoDirection { direction: h1 }, 1.0, h2);
        assert!(!d2.suppressed_duplicate);
        assert!(!d2.emit_to_owner);
        assert!(d2.emit_to_observers);
        assert!(!d2.queue_deferred_owner_echo);
    }

    #[test]
    fn stop_when_already_stopped_emits_nothing_and_moving_stop_emits_triple() {
        let idle = plan_stop_events(77, &MovementMode::Stop, 0.0, DVec3::ZERO, false);
        assert!(idle.is_empty());

        let pre_vel = DVec3::new(120.0, -30.0, 10.0);
        let moving = plan_stop_events(
            77,
            &MovementMode::GotoDirection {
                direction: DVec3::X,
            },
            0.75,
            pre_vel,
            false,
        );
        assert_eq!(
            moving,
            vec![
                DestinyEvent::SetSpeedFraction {
                    ball_id: 77,
                    fraction: 0.0,
                },
                DestinyEvent::Stop { ball_id: 77 },
                DestinyEvent::SetBallVelocity {
                    ball_id: 77,
                    velocity: pre_vel.to_array(),
                },
            ]
        );
    }

    #[test]
    fn monotonicity_merges_equal_stamps_and_rejects_regressing_and_stale_replay() {
        let mut state = SessionStampPolicyState::new(1, Some(1001));
        state.advance_owner_visible_stamp(50);

        let out1 = state.process_dispatch_batches(
            vec![
                PolicyDestinyFrame {
                    stamp: 50,
                    updates: vec![mk_entry(50, "GotoDirection", 1001)],
                    has_set_state: false,
                    wait_for_bubble: false,
                },
                PolicyDestinyFrame {
                    stamp: 50,
                    updates: vec![mk_entry(50, "SetSpeedFraction", 1001)],
                    has_set_state: false,
                    wait_for_bubble: false,
                },
            ],
            true,
        );
        assert_eq!(out1.len(), 1);
        assert_eq!(out1[0].stamp, 50);
        assert_eq!(out1[0].updates.len(), 2);

        // Regressing stamp 49 is rejected and recorded
        let out2 = state.process_dispatch_batches(
            vec![PolicyDestinyFrame {
                stamp: 49,
                updates: vec![mk_entry(49, "Stop", 1001)],
                has_set_state: false,
                wait_for_bubble: false,
            }],
            true,
        );
        assert!(out2.is_empty());
        assert_eq!(
            state.rejections.last().map(|r| &r.reason),
            Some(&StampRejectionReason::BelowLastSentStamp {
                proposed_stamp: 49,
                last_sent_stamp: 50,
            })
        );

        // Fresh AddBalls2 at 55 prevents older subwarp replay at 54
        state.advance_owner_visible_stamp(55);
        state.record_fresh_acquire(2002, 55);
        let out3 = state.process_dispatch_batches(
            vec![PolicyDestinyFrame {
                stamp: 55,
                updates: vec![mk_entry(54, "GotoDirection", 2002)],
                has_set_state: false,
                wait_for_bubble: false,
            }],
            true,
        );
        assert!(out3.is_empty());
        assert!(matches!(
            state.rejections.last().map(|r| &r.reason),
            Some(StampRejectionReason::SubwarpReplayAfterFreshAcquire {
                ball_id: 2002,
                action_stamp: 54,
                acquired_stamp: 55,
            })
        ));
    }

    #[test]
    fn owner_stamp_capped_at_visible_plus_lead() {
        let mut state = SessionStampPolicyState::new(2, Some(1001));
        state.advance_owner_visible_stamp(200);
        let out = state.process_dispatch_batches(
            vec![PolicyDestinyFrame {
                stamp: 205,
                updates: vec![mk_entry(205, "GotoDirection", 1001)],
                has_set_state: false,
                wait_for_bubble: false,
            }],
            true,
        );
        assert!(out.is_empty());
        assert!(matches!(
            state.rejections.last().map(|r| &r.reason),
            Some(StampRejectionReason::OwnerStampExceededVisiblePlusLead {
                proposed_stamp: 205,
                owner_visible_stamp: 200,
                max_lead_ticks: OWNER_DIRECT_ECHO_LEAD_TICKS,
            })
        ));
    }

    #[test]
    fn clamping_restamps_entries_and_rejects_monotonicity_regression() {
        let mut state = SessionStampPolicyState::new(3, Some(1001));
        state.advance_owner_visible_stamp(200);
        // Non-strict clamp pulls 205 -> 201 and restamps inner wire entry tuple.
        let clamped_out = state.process_dispatch_batches(
            vec![PolicyDestinyFrame {
                stamp: 205,
                updates: vec![mk_entry(205, "GotoDirection", 1001)],
                has_set_state: false,
                wait_for_bubble: false,
            }],
            false,
        );
        assert_eq!(clamped_out.len(), 1);
        assert_eq!(clamped_out[0].stamp, 200 + OWNER_DIRECT_ECHO_LEAD_TICKS);
        assert_eq!(
            parse_destiny_entry_summary(&clamped_out[0].updates[0]).and_then(|s| s.0),
            Some(200 + OWNER_DIRECT_ECHO_LEAD_TICKS)
        );

        // If visible lags behind last_sent_stamp (201) such that cap (199) < 201,
        // clamping a stamp >= 201 cannot regress below last_sent_stamp=201.
        state.owner_visible_stamp = Some(198);
        let rejected = state.process_dispatch_batches(
            vec![PolicyDestinyFrame {
                stamp: 202,
                updates: vec![mk_entry(202, "GotoDirection", 1001)],
                has_set_state: false,
                wait_for_bubble: false,
            }],
            false,
        );
        assert!(rejected.is_empty());
        assert!(matches!(
            state.rejections.last().map(|r| &r.reason),
            Some(StampRejectionReason::BelowLastSentStamp {
                proposed_stamp: 199,
                last_sent_stamp: 201,
            })
        ));
    }

    #[test]
    fn wire_payload_extraction_handles_real_set_state_add_balls2_and_remove_balls() {
        let set_state = PyRep::Tuple(vec![
            PyRep::Integer(40),
            PyRep::Tuple(vec![
                PyRep::String("SetState".into()),
                PyRep::Tuple(vec![PyRep::keyval([(
                    "ego",
                    PyRep::Integer(1001),
                )])]),
            ]),
        ]);
        assert_eq!(
            extract_destiny_entry_ball_ids(&set_state),
            Some((Some(40), "SetState", vec![1001]))
        );

        let add_ball = PyRep::Tuple(vec![
            PyRep::Integer(41),
            PyRep::Tuple(vec![
                PyRep::String("AddBall".into()),
                PyRep::Tuple(vec![
                    PyRep::Tuple(vec![
                        PyRep::Object {
                            type_name: "foo.SlimItem".into(),
                            args: Box::new(PyRep::Dict(vec![(
                                PyRep::String("itemID".into()),
                                PyRep::Integer(2001),
                            )])),
                        },
                        PyRep::Tuple(vec![]),
                    ]),
                    PyRep::Bool(false),
                ]),
            ]),
        ]);
        assert_eq!(
            parse_destiny_entry_summary(&add_ball),
            Some((Some(41), "AddBall", Some(2001)))
        );

        let slim_2002 = PyRep::Dict(vec![(
            PyRep::String("itemID".into()),
            PyRep::Integer(2002),
        )]);
        let slim_2003 = PyRep::Dict(vec![(
            PyRep::String("itemID".into()),
            PyRep::Integer(2003),
        )]);
        let add_balls2 = PyRep::Tuple(vec![
            PyRep::Integer(42),
            PyRep::Tuple(vec![
                PyRep::String("AddBalls2".into()),
                PyRep::Tuple(vec![PyRep::Tuple(vec![
                    PyRep::Bytes(vec![0u8; 4]),
                    PyRep::List(vec![
                        PyRep::Tuple(vec![slim_2002, PyRep::Tuple(vec![])]),
                        slim_2003,
                    ]),
                ])]),
            ]),
        ]);
        assert_eq!(
            extract_destiny_entry_ball_ids(&add_balls2),
            Some((Some(42), "AddBalls2", vec![2002, 2003]))
        );

        let remove_balls = PyRep::Tuple(vec![
            PyRep::Integer(43),
            PyRep::Tuple(vec![
                PyRep::String("RemoveBalls".into()),
                PyRep::Tuple(vec![PyRep::List(vec![
                    PyRep::Integer(2002),
                    PyRep::Integer(2003),
                ])]),
            ]),
        ]);
        assert_eq!(
            extract_destiny_entry_ball_ids(&remove_balls),
            Some((Some(43), "RemoveBalls", vec![2002, 2003]))
        );

        // Wire AddBalls2 auto-records fresh acquire without manual record_fresh_acquire call.
        let mut policy = SessionStampPolicyState::new(11, Some(1001));
        policy.advance_owner_visible_stamp(42);
        let acq_out = policy.process_dispatch_batches(
            vec![PolicyDestinyFrame {
                stamp: 42,
                updates: vec![add_balls2],
                has_set_state: false,
                wait_for_bubble: false,
            }],
            true,
        );
        assert_eq!(acq_out.len(), 1);
        assert_eq!(policy.fresh_acquire_stamp_by_ball.get(&2002), Some(&42));
        assert_eq!(policy.fresh_acquire_stamp_by_ball.get(&2003), Some(&42));

        let stale = policy.process_dispatch_batches(
            vec![PolicyDestinyFrame {
                stamp: 42,
                updates: vec![mk_entry(41, "GotoDirection", 2003)],
                has_set_state: false,
                wait_for_bubble: false,
            }],
            true,
        );
        assert!(stale.is_empty());
        assert_eq!(
            policy.rejections.last().map(|r| &r.reason),
            Some(&StampRejectionReason::SubwarpReplayAfterFreshAcquire {
                ball_id: 2003,
                action_stamp: 41,
                acquired_stamp: 42,
            })
        );
    }

    #[test]
    fn set_state_stays_unmerged_at_index_zero_and_rejected_batch_is_transactional() {
        let mut policy = SessionStampPolicyState::new(12, Some(1001));
        policy.advance_owner_visible_stamp(50);

        // Rejected strict batch containing AddBalls2 + over-lead owner command must NOT
        // mutate fresh_acquire_stamp_by_ball or set_state_stamp.
        let rej = policy.process_dispatch_batches(
            vec![PolicyDestinyFrame {
                stamp: 55,
                updates: vec![
                    mk_entry(55, "SetState", 1001),
                    mk_entry(55, "AddBalls2", 2002),
                    mk_entry(55, "GotoDirection", 1001),
                ],
                has_set_state: true,
                wait_for_bubble: true,
            }],
            true,
        );
        assert!(rej.is_empty());
        assert_eq!(policy.set_state_stamp, None);
        assert!(policy.fresh_acquire_stamp_by_ball.is_empty());

        // SetState keeps index 0 within its own frame, remains unmerged (`coalesce_same_stamp: false`),
        // and supersedes any same-stamp pre-reset delta buffered earlier in the same dispatch.
        let out = policy.process_dispatch_batches(
            vec![
                PolicyDestinyFrame {
                    stamp: 50,
                    updates: vec![mk_entry(50, "GotoDirection", 2002)],
                    has_set_state: false,
                    wait_for_bubble: false,
                },
                PolicyDestinyFrame {
                    stamp: 50,
                    updates: vec![
                        mk_entry(50, "SetSpeedFraction", 1001),
                        mk_entry(50, "SetState", 1001),
                    ],
                    has_set_state: true,
                    wait_for_bubble: true,
                },
                PolicyDestinyFrame {
                    stamp: 50,
                    updates: vec![mk_entry(50, "GotoDirection", 1001)],
                    has_set_state: false,
                    wait_for_bubble: false,
                },
            ],
            false,
        );
        assert_eq!(out.len(), 2);
        assert!(out[0].has_set_state);
        assert_eq!(
            parse_destiny_entry_summary(&out[0].updates[0]).map(|s| s.1),
            Some("SetState")
        );
        assert!(!out[1].has_set_state);
        assert_eq!(
            parse_destiny_entry_summary(&out[1].updates[0]).map(|s| s.1),
            Some("GotoDirection")
        );
        assert_eq!(policy.set_state_stamp, Some(50));
        assert_eq!(policy.last_sent_stamp, Some(50));

        // Inner entry exceeding owner visible+lead is caught even when outer batch.stamp == cap.
        let inner_lead_out = policy.process_dispatch_batches(
            vec![PolicyDestinyFrame {
                stamp: 51,
                updates: vec![mk_entry(55, "GotoDirection", 1001)],
                has_set_state: false,
                wait_for_bubble: false,
            }],
            true,
        );
        assert!(inner_lead_out.is_empty());
        assert_eq!(
            policy.rejections.last().map(|r| &r.reason),
            Some(&StampRejectionReason::OwnerStampExceededVisiblePlusLead {
                proposed_stamp: 55,
                owner_visible_stamp: 50,
                max_lead_ticks: OWNER_DIRECT_ECHO_LEAD_TICKS,
            })
        );
    }

    #[test]
    fn tracker_clear_and_warp_handoff_validator_reject_mover_or_scenery_removal() {
        let mut tracker = SameTickGotoTracker::new();
        let stop = MovementMode::Stop;
        let d1 = tracker.evaluate_goto(1001, 10, &stop, 0.0, DVec3::X);
        assert!(d1.emit_to_owner);
        let d2 = tracker.evaluate_goto(1001, 10, &stop, 0.0, DVec3::Y);
        assert!(!d2.emit_to_owner);
        tracker.clear_ball(1001);
        let d3 = tracker.evaluate_goto(1001, 10, &stop, 0.0, DVec3::Z);
        assert!(d3.emit_to_owner);
        tracker.prune_before(11);
        let d4 = tracker.evaluate_goto(1001, 10, &stop, 0.0, DVec3::X);
        assert!(d4.emit_to_owner);

        let add = DestinyEvent::AddBalls2 { balls: Vec::new() };
        let remove_mover = DestinyEvent::RemoveBalls {
            ball_ids: vec![1001],
        };
        let remove_scenery = DestinyEvent::RemoveBall { ball_id: 50_001 };
        let remove_other = DestinyEvent::RemoveBalls {
            ball_ids: vec![2002],
        };

        assert!(!validate_warp_handoff_single_stamp(
            1001,
            &[50_001],
            &[(25, &add), (25, &remove_mover)],
            &[],
        ));
        assert!(!validate_warp_handoff_single_stamp(
            1001,
            &[50_001],
            &[(25, &add), (25, &remove_scenery)],
            &[],
        ));
        assert!(validate_warp_handoff_single_stamp(
            1001,
            &[50_001],
            &[(25, &add), (25, &remove_other)],
            &[2002],
        ));
    }

    #[test]
    fn non_strict_mixed_frame_with_trailing_addballs2_rejects_and_checks_post_norm_freshness() {
        let mut policy = SessionStampPolicyState::new(13, Some(1001));
        policy.advance_owner_visible_stamp(50);

        // OwnerPilotCommand precedes AddBalls2 in non-strict mode: must NOT clamp `stamp`
        // or leave mixed inner stamps or mutate `fresh_acquire_stamp_by_ball`.
        let mixed_out = policy.process_dispatch_batches(
            vec![PolicyDestinyFrame {
                stamp: 55,
                updates: vec![
                    mk_entry(55, "GotoDirection", 1001),
                    mk_entry(55, "AddBalls2", 2002),
                ],
                has_set_state: false,
                wait_for_bubble: false,
            }],
            false,
        );
        assert!(mixed_out.is_empty());
        assert!(policy.fresh_acquire_stamp_by_ball.is_empty());
        assert_eq!(
            policy.rejections.last().map(|r| &r.reason),
            Some(&StampRejectionReason::OwnerStampExceededVisiblePlusLead {
                proposed_stamp: 55,
                owner_visible_stamp: 50,
                max_lead_ticks: OWNER_DIRECT_ECHO_LEAD_TICKS,
            })
        );

        // Post-normalization freshness catches an inner sub-warp entry shifted down to frame stamp
        // even when `stamp == orig_stamp` (outer stamp unchanged).
        policy.record_fresh_acquire(2002, 53);
        let stale_inner = policy.process_dispatch_batches(
            vec![PolicyDestinyFrame {
                stamp: 51,
                updates: vec![mk_entry(55, "GotoDirection", 2002)],
                has_set_state: false,
                wait_for_bubble: false,
            }],
            false,
        );
        assert!(stale_inner.is_empty());
        assert_eq!(
            policy.rejections.last().map(|r| &r.reason),
            Some(&StampRejectionReason::SubwarpReplayAfterFreshAcquire {
                ball_id: 2002,
                action_stamp: 51,
                acquired_stamp: 53,
            })
        );
    }
}
