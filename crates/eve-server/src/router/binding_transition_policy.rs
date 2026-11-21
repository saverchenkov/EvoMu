//! Scene-epoch transition tracking and unified late-call policy engine (`#1190`).
//!
//! Consolidates per-session monotonic `scene_epoch` counters, epoch-scoped
//! departed-scene / invalidated-binding grace records, `SafeLogoffTeardownGrace`
//! late-call allowlisting, and declarative `LateCallPolicy` evaluation (`Benign`,
//! `RebindToCurrentScene`, `StaleObjectGone`) into one table-driven module.

use crate::router::context::{BoundObjectEntry, ServerContext, NEXT_BOUND_ID};
use eve_db::lock_rank::{RankedRwLock, RANK_LEAF};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock, RwLock, Weak};

/// Retention window (`30s`, matching V24 `basesession.pyj::ObjectConnection.__del__ ->
/// DisconnectObject(30)`) for in-flight late calls arriving during/right after a
/// session transition (`#1146`, `#1150`, `#1209`, `#1211`, `#1190`).
pub(crate) const TRANSITION_DISCONNECT_GRACE_TICKS: i64 = 30 * 10_000_000;

/// Maximum transition-disconnected grace entries retained per session.
pub(crate) const MAX_TRANSITION_GRACE_PER_SESSION: usize = 256;

/// Maximum departed-scene epoch records retained per session.
const MAX_EPOCH_RECORDS_PER_SESSION: usize = 32;

/// Declarative location classification used when authorizing late bind fallback requests
/// for objects in recently departed scenes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub(crate) enum DepartedLocationKind {
    Dock,
    SolarSystem,
}

/// Safe response strategy for `LateCallPolicy::Benign`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LateCallBenignResponse {
    /// Route the read-only late call through the preserved departed-scene `BoundObjectEntry`.
    DepartedBindingReadOnly,
    /// Allow a read-only call inside `SafeLogoffTeardownGrace` character-selection teardown.
    /// When `creates_temp_binding` is `true`, dispatches via a short-lived `skillMgr2` binding.
    TeardownGraceReadOnly { creates_temp_binding: bool },
    /// Answer immediately with deterministic empty `PyRep::None` without calling a handler.
    #[allow(dead_code)]
    EmptyNone,
    /// Answer immediately with deterministic empty `PyRep::List([])` without calling a handler.
    #[allow(dead_code)]
    EmptyList,
}

impl LateCallBenignResponse {
    pub(crate) fn to_empty_pyrep(self) -> Option<PyRep> {
        match self {
            Self::DepartedBindingReadOnly | Self::TeardownGraceReadOnly { .. } => None,
            Self::EmptyNone => Some(PyRep::None),
            Self::EmptyList => Some(PyRep::List(Vec::new())),
        }
    }
}

/// Declarative policy for an RPC arriving against a bound object or departed scene
/// after a `scene_epoch` transition (`#1190`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LateCallPolicy {
    /// Handle benignly inside the active transition/teardown grace window.
    Benign(LateCallBenignResponse),
    /// Rebind/redirect the call onto the equivalent active bound object of the current scene.
    #[allow(dead_code)]
    RebindToCurrentScene,
    /// Reject fail-closed (`InvalidBoundObject` / `BindAuthorizationDenied`), never modal.
    StaleObjectGone,
}

/// Dispatch surface a declarative late-call rule governs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LateCallSurface {
    /// Late `ObjectCall` against a previously bound, transition-invalidated object.
    BoundGraceCall,
    /// Late `ObjectCall` or `MachoBindObject` fallback against an active departed location.
    BoundGraceOrDepartedBind,
    /// Late unbound call during `SafeLogoffTeardownGrace` at character selection.
    SafeLogoffTeardownUnbound,
    /// Late call during `SafeLogoffTeardownGrace` either unbound or on temp `skillMgr2` binding.
    SafeLogoffTeardownBoundOrUnbound,
}

/// Extra ownership or argument invariant required before authorizing a late call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LateCallAuthPredicate {
    None,
    /// `#1150/#1152/#1153`: target ship (`call_args[0]` or `session.ship_id`) is a ship owned by `char_id`.
    OwnedShipDirtTarget,
    /// `#1209/#1211`: RPC positional arguments must be empty.
    EmptyArgs,
    /// `#1386/#1434`: `account.GetCashBalance` limited to personal wallet (no corp flags/keys).
    PersonalCashBalanceArgs,
}

/// Declarative entry in `LATE_CALL_POLICY_TABLE` keyed by `(service, method)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LateCallPolicyRule {
    pub(crate) service: &'static str,
    pub(crate) method: &'static str,
    pub(crate) policy: LateCallPolicy,
    pub(crate) surface: LateCallSurface,
    pub(crate) predicate: LateCallAuthPredicate,
    pub(crate) allowed_departed_kinds: &'static [DepartedLocationKind],
}

/// Runtime resolution produced by evaluating `LATE_CALL_POLICY_TABLE` on a bound late call.
#[derive(Debug, Clone)]
pub(crate) enum LateCallResolution {
    DispatchWithBinding(BoundObjectEntry),
    RespondImmediate(PyRep),
    RejectStale,
}

/// Atomic record capturing one session transition (`departed_scene_epoch -> entered_scene_epoch`),
/// its departed scene locations, and the bound object IDs invalidated from that epoch.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransitionEpochRecord {
    pub(crate) session_id: u64,
    pub(crate) user_id: Option<u64>,
    pub(crate) character_id: Option<i64>,
    pub(crate) departed_scene_epoch: u64,
    pub(crate) entered_scene_epoch: u64,
    pub(crate) kind_label: &'static str,
    pub(crate) reason: String,
    pub(crate) departed_solar_system_id: Option<u32>,
    pub(crate) departed_dock_id: Option<i64>,
    pub(crate) invalidated_bound_ids: Vec<u64>,
    pub(crate) recorded_ticks: i64,
}

/// Parameters committed atomically into `ContextGraceStore` for one `SessionTransition`.
pub(crate) struct AtomicSceneTransitionRecord<'a> {
    pub(crate) session_id: u64,
    pub(crate) user_id: Option<u64>,
    pub(crate) char_id: i64,
    pub(crate) kind_label: &'static str,
    pub(crate) reason: &'a str,
    pub(crate) is_logout: bool,
    pub(crate) is_login: bool,
    pub(crate) prev_sys: Option<u32>,
    pub(crate) prev_dock: Option<i64>,
    pub(crate) next_sys: Option<u32>,
    pub(crate) next_dock: Option<i64>,
    pub(crate) can_record_departed: bool,
    pub(crate) removed_bindings: Vec<BoundObjectEntry>,
}

const DEPARTED_DOCK_ONLY: &[DepartedLocationKind] = &[DepartedLocationKind::Dock];
const DEPARTED_SYSTEM_OR_DOCK: &[DepartedLocationKind] =
    &[DepartedLocationKind::SolarSystem, DepartedLocationKind::Dock];

const fn rule_stale(service: &'static str, method: &'static str) -> LateCallPolicyRule {
    LateCallPolicyRule {
        service,
        method,
        policy: LateCallPolicy::StaleObjectGone,
        surface: LateCallSurface::BoundGraceCall,
        predicate: LateCallAuthPredicate::None,
        allowed_departed_kinds: &[],
    }
}

const fn rule_teardown_skill_getter(method: &'static str) -> LateCallPolicyRule {
    LateCallPolicyRule {
        service: "skillMgr2",
        method,
        policy: LateCallPolicy::Benign(LateCallBenignResponse::TeardownGraceReadOnly {
            creates_temp_binding: false,
        }),
        surface: LateCallSurface::SafeLogoffTeardownBoundOrUnbound,
        predicate: LateCallAuthPredicate::None,
        allowed_departed_kinds: &[],
    }
}

/// Authoritative declarative policy table keyed by `(service, method)`.
/// Any unlisted `(service, method)` pair defaults fail-closed to `LateCallPolicy::StaleObjectGone`.
pub(crate) const LATE_CALL_POLICY_TABLE: &[LateCallPolicyRule] = &[
    // #1146: in-flight read-only invbroker metadata on transition-disconnected bindings
    LateCallPolicyRule {
        service: "invbroker",
        method: "GetSelfInvItem",
        policy: LateCallPolicy::Benign(LateCallBenignResponse::DepartedBindingReadOnly),
        surface: LateCallSurface::BoundGraceCall,
        predicate: LateCallAuthPredicate::None,
        allowed_departed_kinds: &[],
    },
    LateCallPolicyRule {
        service: "invbroker",
        method: "GetAvailableTurretSlots",
        policy: LateCallPolicy::Benign(LateCallBenignResponse::DepartedBindingReadOnly),
        surface: LateCallSurface::BoundGraceCall,
        predicate: LateCallAuthPredicate::None,
        allowed_departed_kinds: &[],
    },
    rule_stale("invbroker", "List"),
    rule_stale("invbroker", "ListByFlags"),
    rule_stale("invbroker", "Add"),
    rule_stale("invbroker", "MultiAdd"),
    rule_stale("invbroker", "Remove"),
    // #1150, #1152, #1153: owned ship dirt query racing Undock / /moveme
    LateCallPolicyRule {
        service: "ship",
        method: "GetDirtTimestamp",
        policy: LateCallPolicy::Benign(LateCallBenignResponse::DepartedBindingReadOnly),
        surface: LateCallSurface::BoundGraceOrDepartedBind,
        predicate: LateCallAuthPredicate::OwnedShipDirtTarget,
        allowed_departed_kinds: DEPARTED_DOCK_ONLY,
    },
    rule_stale("ship", "Board"),
    rule_stale("ship", "AssembleShip"),
    rule_stale("ship", "ResetDirtTimestamp"),
    rule_stale("ship", "Undock"),
    rule_stale("ship", "LeaveShip"),
    rule_stale("ship", "TrashItems"),
    // #1209, #1211: crimewatch client states racing overlapping transitions
    LateCallPolicyRule {
        service: "crimewatch",
        method: "GetClientStates",
        policy: LateCallPolicy::Benign(LateCallBenignResponse::DepartedBindingReadOnly),
        surface: LateCallSurface::BoundGraceOrDepartedBind,
        predicate: LateCallAuthPredicate::EmptyArgs,
        allowed_departed_kinds: DEPARTED_SYSTEM_OR_DOCK,
    },
    rule_stale("crimewatch", "SetSafetyLevel"),
    rule_stale("crimewatch", "StartDuelChallenge"),
    rule_stale("crimewatch", "RespondToDuelChallenge"),
    rule_stale("crimewatch", "ApplyTagsForSecStatus"),
    rule_stale("crimewatch", "GetMySecurityStatus"),
    rule_stale("crimewatch", "GetCharacterSecurityStatus"),
    rule_stale("crimewatch", "GetSecurityStatusTransactions"),
    // #1360, #1386, #1434, #1477: SafeLogoffTeardownGrace read-only client teardown RPCs
    LateCallPolicyRule {
        service: "skillMgr2",
        method: "GetMySkillHandler",
        policy: LateCallPolicy::Benign(LateCallBenignResponse::TeardownGraceReadOnly {
            creates_temp_binding: true,
        }),
        surface: LateCallSurface::SafeLogoffTeardownUnbound,
        predicate: LateCallAuthPredicate::None,
        allowed_departed_kinds: &[],
    },
    rule_teardown_skill_getter("GetSkills"),
    rule_teardown_skill_getter("GetSkillQueue"),
    rule_teardown_skill_getter("GetSkillQueueAndFreePoints"),
    rule_teardown_skill_getter("GetEndOfTraining"),
    rule_teardown_skill_getter("GetAttributes"),
    rule_teardown_skill_getter("GetFreeSkillPoints"),
    rule_teardown_skill_getter("GetImplants"),
    rule_teardown_skill_getter("GetBoosters"),
    rule_teardown_skill_getter("GetSkillHistory"),
    rule_teardown_skill_getter("GetRespecInfo"),
    rule_teardown_skill_getter("GetCharacterAttributeModifiers"),
    rule_teardown_skill_getter("GetSkillRequirements"),
    rule_stale("skillMgr2", "SaveSkillQueue"),
    LateCallPolicyRule {
        service: "account",
        method: "GetCashBalance",
        policy: LateCallPolicy::Benign(LateCallBenignResponse::TeardownGraceReadOnly {
            creates_temp_binding: false,
        }),
        surface: LateCallSurface::SafeLogoffTeardownUnbound,
        predicate: LateCallAuthPredicate::PersonalCashBalanceArgs,
        allowed_departed_kinds: &[],
    },
    LateCallPolicyRule {
        service: "accessGroupBookmarkMgr",
        method: "GetMyActiveBookmarks",
        policy: LateCallPolicy::Benign(LateCallBenignResponse::TeardownGraceReadOnly {
            creates_temp_binding: false,
        }),
        surface: LateCallSurface::SafeLogoffTeardownUnbound,
        predicate: LateCallAuthPredicate::None,
        allowed_departed_kinds: &[],
    },
    LateCallPolicyRule {
        service: "bookmark",
        method: "GetMyActiveBookmarks",
        policy: LateCallPolicy::Benign(LateCallBenignResponse::TeardownGraceReadOnly {
            creates_temp_binding: false,
        }),
        surface: LateCallSurface::SafeLogoffTeardownUnbound,
        predicate: LateCallAuthPredicate::None,
        allowed_departed_kinds: &[],
    },
    LateCallPolicyRule {
        service: "bookmark",
        method: "GetBookmarks",
        policy: LateCallPolicy::Benign(LateCallBenignResponse::TeardownGraceReadOnly {
            creates_temp_binding: false,
        }),
        surface: LateCallSurface::SafeLogoffTeardownUnbound,
        predicate: LateCallAuthPredicate::None,
        allowed_departed_kinds: &[],
    },
];

/// Looks up the declarative rule for `(service, method)` in `LATE_CALL_POLICY_TABLE`.
pub(crate) fn lookup_late_call_rule(
    service: &str,
    method: &str,
) -> Option<&'static LateCallPolicyRule> {
    LATE_CALL_POLICY_TABLE
        .iter()
        .find(|rule| rule.service == service && rule.method == method)
}

/// Evaluates the declarative `LateCallPolicy` for `(service, method)`, failing closed
/// to `LateCallPolicy::StaleObjectGone` for any unlisted service/method pair.
#[allow(dead_code)]
pub(crate) fn lookup_late_call_policy(service: &str, method: &str) -> LateCallPolicy {
    lookup_late_call_rule(service, method)
        .map(|rule| rule.policy)
        .unwrap_or(LateCallPolicy::StaleObjectGone)
}

/// Returns whether invalidated bindings for `service` participate in departed-epoch
/// bound grace (`BoundGraceCall` or `BoundGraceOrDepartedBind`) per `LATE_CALL_POLICY_TABLE`.
pub(crate) fn policy_retains_departed_binding(service: &str) -> bool {
    LATE_CALL_POLICY_TABLE.iter().any(|rule| {
        rule.service == service
            && matches!(
                rule.surface,
                LateCallSurface::BoundGraceCall | LateCallSurface::BoundGraceOrDepartedBind
            )
            && !matches!(rule.policy, LateCallPolicy::StaleObjectGone)
    })
}

pub(crate) fn is_personal_cash_balance_args(
    call_args: &[PyRep],
    call_kwargs: &HashMap<String, PyRep>,
) -> bool {
    let parse_corp_flag = |rep: &PyRep| -> bool {
        match rep {
            PyRep::Bool(b) => *b,
            PyRep::Integer(n) => *n != 0,
            PyRep::Real(r) => *r != 0.0,
            PyRep::String(s) | PyRep::Token(s) => s.starts_with("cash"),
            PyRep::Tuple(items) | PyRep::List(items) => match items.as_slice() {
                [first, ..] => {
                    first.as_bool() == Some(true)
                        || first.as_i64().unwrap_or(0) != 0
                        || first.as_str().is_some_and(|s| s.starts_with("cash"))
                }
                [] => false,
            },
            _ => false,
        }
    };
    let first_arg_corp = match call_args {
        [first, ..] => Some(parse_corp_flag(first)),
        [] => None,
    };
    let is_corp = first_arg_corp
        .or_else(|| call_kwargs.get("isCorpWallet").map(&parse_corp_flag))
        .or_else(|| {
            (call_args.is_empty()
                && call_kwargs
                    .get("accountKey")
                    .and_then(|v| v.as_i64())
                    .is_some_and(|k| (1000..=1006).contains(&k)))
            .then_some(true)
        })
        .unwrap_or(false);
    !is_corp
}

#[derive(Clone)]
struct TransitionDisconnectedBinding {
    entry: BoundObjectEntry,
    scene_epoch: u64,
    disconnected_ticks: i64,
}

#[derive(Clone)]
struct TransitionDepartedLocation {
    session_id: u64,
    user_id: u64,
    character_id: i64,
    kind: DepartedLocationKind,
    location_id: i64,
    scene_epoch: u64,
    departed_ticks: i64,
}

struct ContextGraceStore {
    owner: Weak<RankedRwLock<HashMap<u64, BoundObjectEntry>, RANK_LEAF>>,
    entries: HashMap<u64, TransitionDisconnectedBinding>,
    departed_locations: HashMap<(u64, DepartedLocationKind, i64), TransitionDepartedLocation>,
    scene_epochs: HashMap<u64, u64>,
    epoch_records: HashMap<u64, Vec<TransitionEpochRecord>>,
    pending_displaced_logoffs: HashMap<u64, (u64, i64)>,
    prelogoff_hold_sessions: HashMap<i64, (u64, u64)>,
    reconnect_hold_sessions: HashMap<i64, u64>,
}

/// RAII guard preserving an in-progress character reconnect against premature `on_advance_secs(0)`
/// disconnect-hold pruning while the replacement session already holds the online lock (`#1171`).
pub(crate) struct ReconnectHoldGuard<'a> {
    ctx: &'a ServerContext,
    char_id: i64,
    session_id: u64,
}

impl<'a> ReconnectHoldGuard<'a> {
    pub(crate) fn acquire(ctx: &'a ServerContext, char_id: i64, session_id: u64) -> Self {
        ctx.with_context_grace_store_mut(|store| {
            store.reconnect_hold_sessions.insert(char_id, session_id);
        });
        Self {
            ctx,
            char_id,
            session_id,
        }
    }
}

impl Drop for ReconnectHoldGuard<'_> {
    fn drop(&mut self) {
        let matches = self.ctx.read_context_grace_store(|s| {
            s.is_some_and(|st| {
                st.reconnect_hold_sessions.get(&self.char_id) == Some(&self.session_id)
            })
        });
        if matches {
            self.ctx.with_existing_grace_store_mut_and_prune(|store| {
                if store.reconnect_hold_sessions.get(&self.char_id) == Some(&self.session_id) {
                    store.reconnect_hold_sessions.remove(&self.char_id);
                }
            });
        }
    }
}

static TRANSITION_DISCONNECT_GRACE: OnceLock<RwLock<HashMap<u64, ContextGraceStore>>> =
    OnceLock::new();

fn transition_disconnect_grace() -> &'static RwLock<HashMap<u64, ContextGraceStore>> {
    TRANSITION_DISCONNECT_GRACE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn current_scene_target_object_id(session: &SessionState, service: &str) -> Option<i64> {
    match service {
        "station" | "stationSvc" | "corpStationMgr" | "repairSvc" | "reprocessingSvc"
        | "insuranceSvc" => session.station_id.or(session.structure_id),
        "beyonce"
        | "scanMgr"
        | "entity"
        | "crimewatch"
        | "planetOrbitalRegistryBroker"
        | "keeper"
        | "posMgr"
        | "devIndexManager" => session.solar_system_id.map(i64::from),
        "ship" => session
            .station_id
            .or(session.structure_id)
            .or_else(|| session.solar_system_id.map(i64::from)),
        "invbroker" => session
            .ship_id
            .or_else(|| session.station_id.or(session.structure_id)),
        _ => None,
    }
}

impl ServerContext {
    fn is_grace_ticks_active(recorded_ticks: i64, now_ticks: i64) -> bool {
        now_ticks >= recorded_ticks
            && now_ticks.saturating_sub(recorded_ticks) <= TRANSITION_DISCONNECT_GRACE_TICKS
    }

    fn is_grace_entry_active(binding: &TransitionDisconnectedBinding, now_ticks: i64) -> bool {
        Self::is_grace_ticks_active(binding.disconnected_ticks, now_ticks)
    }

    pub(crate) fn online_session_id_and_user_for_char(&self, char_id: i64) -> Option<(u64, u64)> {
        self.db.online_sessions_snapshot().into_iter().find_map(|(uid, sid)| {
            self.db
                .get_online_session(uid)
                .filter(|rec| rec.session_id == sid && rec.character_id == Some(char_id))
                .map(|_| (sid, uid))
        })
    }

    pub(crate) fn has_matching_online_session_ids(
        &self,
        session_id: u64,
        uid: u64,
        cid: i64,
    ) -> bool {
        self.db.get_online_session(uid).is_none_or(|online| {
            online.session_id == session_id && online.character_id == Some(cid)
        })
    }

    fn has_matching_online_session(&self, session: &SessionState, uid: u64, cid: i64) -> bool {
        self.has_matching_online_session_ids(session.session_id, uid, cid)
    }

    fn is_station_or_structure_id(&self, object_id: i64) -> bool {
        object_id > 0
            && (self.sde.stations.contains_key(&object_id)
                || self
                    .db
                    .with_snapshot(|snap| snap.structures.contains_key(&object_id)))
    }

    fn is_owned_ship_dirt_target(
        &self,
        session: &SessionState,
        char_id: i64,
        call_args: &[PyRep],
    ) -> bool {
        let first_ship = match call_args {
            [first, ..] => first.as_i64(),
            [] => None,
        };
        let Some(target_ship_id) = first_ship.or(session.ship_id) else {
            return false;
        };
        if target_ship_id <= 0 {
            return false;
        }
        if session.ship_id == Some(target_ship_id) {
            return true;
        }
        self.db.with_snapshot(|snap| {
            snap.items.get(&target_ship_id).is_some_and(|item| {
                item.owner_id == char_id
                    && self
                        .sde
                        .types
                        .get(&item.type_id)
                        .is_none_or(|t| t.category_id == 6)
            })
        })
    }

    fn evaluate_late_call_predicate(
        &self,
        predicate: LateCallAuthPredicate,
        session: &SessionState,
        char_id: i64,
        call_args: &[PyRep],
        call_kwargs: &HashMap<String, PyRep>,
    ) -> bool {
        match predicate {
            LateCallAuthPredicate::None => true,
            LateCallAuthPredicate::OwnedShipDirtTarget => {
                self.is_owned_ship_dirt_target(session, char_id, call_args)
            }
            LateCallAuthPredicate::EmptyArgs => call_args.is_empty(),
            LateCallAuthPredicate::PersonalCashBalanceArgs => {
                is_personal_cash_balance_args(call_args, call_kwargs)
            }
        }
    }

    fn excess_session_grace_keys<K: Ord>(mut keyed_ticks: Vec<(i64, K)>) -> Vec<K> {
        if keyed_ticks.len() <= MAX_TRANSITION_GRACE_PER_SESSION {
            return Vec::new();
        }
        keyed_ticks.sort_unstable();
        let excess = keyed_ticks.len() - MAX_TRANSITION_GRACE_PER_SESSION;
        keyed_ticks.into_iter().take(excess).map(|(_, k)| k).collect()
    }

    fn trim_store_session_caps(store: &mut ContextGraceStore, touched_sessions: &[u64]) {
        for &sid in touched_sessions {
            let excess_bids = Self::excess_session_grace_keys(
                store
                    .entries
                    .iter()
                    .filter(|(_, v)| v.entry.session_id == sid)
                    .map(|(&bid, v)| (v.disconnected_ticks, bid))
                    .collect(),
            );
            for bid in excess_bids {
                store.entries.remove(&bid);
            }
            let excess_locations = Self::excess_session_grace_keys(
                store
                    .departed_locations
                    .iter()
                    .filter(|((s, _, _), v)| *s == sid || v.session_id == sid)
                    .map(|(&key, v)| (v.departed_ticks, key))
                    .collect(),
            );
            for key in excess_locations {
                store.departed_locations.remove(&key);
            }
        }
    }

    fn read_context_grace_store<R>(&self, f: impl FnOnce(Option<&ContextGraceStore>) -> R) -> R {
        let outer = transition_disconnect_grace()
            .read()
            .unwrap_or_else(|e| e.into_inner());
        f(outer.get(&self.instance_id))
    }

    fn is_grace_store_empty(store: &ContextGraceStore) -> bool {
        store.entries.is_empty()
            && store.departed_locations.is_empty()
            && store.scene_epochs.is_empty()
            && store.epoch_records.is_empty()
            && store.pending_displaced_logoffs.is_empty()
            && store.prelogoff_hold_sessions.is_empty()
            && store.reconnect_hold_sessions.is_empty()
    }

    fn with_existing_grace_store_mut_and_prune<R>(
        &self,
        f: impl FnOnce(&mut ContextGraceStore) -> R,
    ) -> Option<R> {
        let lock = TRANSITION_DISCONNECT_GRACE.get()?;
        let mut outer = lock.write().unwrap_or_else(|e| e.into_inner());
        outer.retain(|_, store| store.owner.strong_count() > 0);
        let res = outer.get_mut(&self.instance_id).map(f);
        if outer
            .get(&self.instance_id)
            .is_some_and(Self::is_grace_store_empty)
        {
            outer.remove(&self.instance_id);
        }
        res
    }

    fn with_context_grace_store_mut<R>(&self, f: impl FnOnce(&mut ContextGraceStore) -> R) -> R {
        let owner = Arc::downgrade(&self.bound_objects);
        let mut outer = transition_disconnect_grace()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        outer.retain(|_, store| store.owner.strong_count() > 0);
        let store = outer.entry(self.instance_id).or_insert_with(|| ContextGraceStore {
            owner,
            entries: HashMap::new(),
            departed_locations: HashMap::new(),
            scene_epochs: HashMap::new(),
            epoch_records: HashMap::new(),
            pending_displaced_logoffs: HashMap::new(),
            prelogoff_hold_sessions: HashMap::new(),
            reconnect_hold_sessions: HashMap::new(),
        });
        f(store)
    }

    /// Returns the monotonic `scene_epoch` currently associated with `session_id` (`0` before
    /// the session's first transition).
    pub(crate) fn session_scene_epoch(&self, session_id: u64) -> u64 {
        self.read_context_grace_store(|s| {
            s.and_then(|st| st.scene_epochs.get(&session_id).copied())
                .unwrap_or(0)
        })
    }

    /// Returns whether `expected_epoch` matches `session_id`'s current authoritative `scene_epoch`.
    #[allow(dead_code)]
    pub(crate) fn verify_session_scene_epoch(&self, session_id: u64, expected_epoch: u64) -> bool {
        self.session_scene_epoch(session_id) == expected_epoch
    }

    /// Returns the most recent `TransitionEpochRecord` recorded for `session_id`, if any.
    #[allow(dead_code)]
    pub(crate) fn current_transition_epoch_record(
        &self,
        session_id: u64,
    ) -> Option<TransitionEpochRecord> {
        self.read_context_grace_store(|s| {
            s.and_then(|st| st.epoch_records.get(&session_id))
                .and_then(|records| records.last().cloned())
        })
    }

    /// Returns the bounded history of `TransitionEpochRecord`s recorded for `session_id`.
    #[allow(dead_code)]
    pub(crate) fn transition_epoch_records(&self, session_id: u64) -> Vec<TransitionEpochRecord> {
        self.read_context_grace_store(|s| {
            s.and_then(|st| st.epoch_records.get(&session_id).cloned())
                .unwrap_or_default()
        })
    }

    /// Clears `scene_epochs` tracking upon full transport disconnect teardown so closed connections
    /// do not accumulate entries, while keeping `scene_epoch` monotonic across in-connection
    /// safe-logoff/re-login cycles (`#1190`).
    pub(crate) fn clear_session_scene_epoch(&self, session_id: u64) {
        self.with_existing_grace_store_mut_and_prune(|store| {
            store.scene_epochs.remove(&session_id);
            store.epoch_records.remove(&session_id);
        });
    }

    /// Atomically advances `session_id`'s `scene_epoch`, purges or records departed locations and
    /// invalidated bindings under `(session_id, departed_scene_epoch)`, and writes a single
    /// `TransitionEpochRecord` inside one grace-store write lock acquisition (`#1190`).
    pub(crate) fn record_atomic_session_transition(
        &self,
        record: AtomicSceneTransitionRecord<'_>,
    ) -> u64 {
        let now_ticks = self.now_machonet_wallclock_filetime_ticks();
        let AtomicSceneTransitionRecord {
            session_id,
            user_id,
            char_id,
            kind_label,
            reason,
            is_logout,
            is_login,
            prev_sys,
            prev_dock,
            next_sys,
            next_dock,
            can_record_departed,
            removed_bindings,
        } = record;

        let invalidated_bound_ids: Vec<u64> =
            removed_bindings.iter().map(|e| e.bound_id).collect();
        let (loc_record, loc_purge) = if is_logout || is_login {
            (Vec::new(), Vec::new())
        } else {
            Self::compute_departed_location_deltas(prev_sys, prev_dock, next_sys, next_dock)
        };

        self.with_grace_store_mut(now_ticks, |store| {
            let departed_epoch = store.scene_epochs.get(&session_id).copied().unwrap_or(0);
            let entered_epoch = departed_epoch.saturating_add(1);
            store.scene_epochs.insert(session_id, entered_epoch);

            if is_logout || is_login {
                store.entries.retain(|_, v| {
                    v.entry.session_id != session_id && v.entry.character_id != char_id
                });
                store.departed_locations.retain(|&(sid, _, _), v| {
                    sid != session_id && v.session_id != session_id && v.character_id != char_id
                });
                store.epoch_records.remove(&session_id);
            } else {
                let mut touched_sessions = Vec::new();
                for &(kind, lid) in &loc_purge {
                    store.departed_locations.remove(&(session_id, kind, lid));
                }
                if can_record_departed {
                    if let Some(uid) = user_id {
                        for &(kind, lid) in &loc_record {
                            if !touched_sessions.contains(&session_id) {
                                touched_sessions.push(session_id);
                            }
                            store.departed_locations.insert(
                                (session_id, kind, lid),
                                TransitionDepartedLocation {
                                    session_id,
                                    user_id: uid,
                                    character_id: char_id,
                                    kind,
                                    location_id: lid,
                                    scene_epoch: departed_epoch,
                                    departed_ticks: now_ticks,
                                },
                            );
                        }
                    }
                }
                Self::insert_departed_bindings_into_store(
                    self,
                    store,
                    &removed_bindings,
                    Some(departed_epoch),
                    now_ticks,
                    &mut touched_sessions,
                );
                if !touched_sessions.is_empty() {
                    Self::trim_store_session_caps(store, &touched_sessions);
                }
            }

            let epoch_rec = TransitionEpochRecord {
                session_id,
                user_id,
                character_id: Some(char_id),
                departed_scene_epoch: departed_epoch,
                entered_scene_epoch: entered_epoch,
                kind_label,
                reason: reason.to_string(),
                departed_solar_system_id: prev_sys,
                departed_dock_id: prev_dock,
                invalidated_bound_ids,
                recorded_ticks: now_ticks,
            };
            let list = store.epoch_records.entry(session_id).or_default();
            list.push(epoch_rec);
            if list.len() > MAX_EPOCH_RECORDS_PER_SESSION {
                let drain_count = list.len() - MAX_EPOCH_RECORDS_PER_SESSION;
                list.drain(0..drain_count);
            }

            entered_epoch
        })
    }

    pub(crate) fn record_pending_displaced_logoff(
        &self,
        session_id: u64,
        user_id: u64,
        char_id: i64,
    ) {
        self.with_context_grace_store_mut(|store| {
            store
                .pending_displaced_logoffs
                .insert(session_id, (user_id, char_id));
        });
    }

    pub(crate) fn take_pending_displaced_logoff(&self, session_id: u64) -> Option<(u64, i64)> {
        let has_entry = self.read_context_grace_store(|s| {
            s.is_some_and(|st| st.pending_displaced_logoffs.contains_key(&session_id))
        });
        if !has_entry {
            return None;
        }
        self.with_existing_grace_store_mut_and_prune(|store| {
            store.pending_displaced_logoffs.remove(&session_id)
        })
        .flatten()
    }

    pub(crate) fn clear_pending_displaced_logoffs_for_char(&self, user_id: u64, char_id: i64) {
        let has_match = self.read_context_grace_store(|s| {
            s.is_some_and(|st| {
                st.pending_displaced_logoffs
                    .values()
                    .any(|&(uid, cid)| uid == user_id && cid == char_id)
            })
        });
        if !has_match {
            return;
        }
        self.with_existing_grace_store_mut_and_prune(|store| {
            store
                .pending_displaced_logoffs
                .retain(|_, &mut (uid, cid)| !(uid == user_id && cid == char_id));
        });
    }

    pub(crate) fn record_prelogoff_hold_session(
        &self,
        char_id: i64,
        session: Option<(u64, u64)>,
    ) {
        match session {
            Some(pair) => {
                self.with_context_grace_store_mut(|store| {
                    store.prelogoff_hold_sessions.insert(char_id, pair);
                });
            }
            None => {
                let has_entry = self.read_context_grace_store(|s| {
                    s.is_some_and(|st| st.prelogoff_hold_sessions.contains_key(&char_id))
                });
                if has_entry {
                    self.with_existing_grace_store_mut_and_prune(|store| {
                        store.prelogoff_hold_sessions.remove(&char_id);
                    });
                }
            }
        }
    }

    pub(crate) fn prelogoff_hold_session(&self, char_id: i64) -> Option<(u64, u64)> {
        self.read_context_grace_store(|s| {
            s.and_then(|st| st.prelogoff_hold_sessions.get(&char_id).copied())
        })
    }

    pub(crate) fn clear_prelogoff_hold_session_if_matches(
        &self,
        char_id: i64,
        expected: (u64, u64),
    ) {
        let matches = self.read_context_grace_store(|s| {
            s.is_some_and(|st| st.prelogoff_hold_sessions.get(&char_id) == Some(&expected))
        });
        if matches {
            self.with_existing_grace_store_mut_and_prune(|store| {
                if store.prelogoff_hold_sessions.get(&char_id) == Some(&expected) {
                    store.prelogoff_hold_sessions.remove(&char_id);
                }
            });
        }
    }

    pub(crate) fn snapshot_prelogoff_hold_sessions(&self) -> HashMap<i64, (u64, u64)> {
        self.read_context_grace_store(|s| {
            s.map(|st| st.prelogoff_hold_sessions.clone())
                .unwrap_or_default()
        })
    }

    pub(crate) fn reconnect_hold_session(&self, char_id: i64) -> Option<u64> {
        self.read_context_grace_store(|s| {
            s.and_then(|st| st.reconnect_hold_sessions.get(&char_id).copied())
        })
    }

    fn with_grace_store_mut<R>(&self, now_ticks: i64, f: impl FnOnce(&mut ContextGraceStore) -> R) -> R {
        self.with_context_grace_store_mut(|store| {
            store
                .entries
                .retain(|_, v| Self::is_grace_entry_active(v, now_ticks));
            store
                .departed_locations
                .retain(|_, v| Self::is_grace_ticks_active(v.departed_ticks, now_ticks));
            f(store)
        })
    }

    fn departed_location_kind_for_entry(
        &self,
        entry: &BoundObjectEntry,
    ) -> Option<DepartedLocationKind> {
        if entry.object_id <= 0 || entry.user_id == 0 || entry.character_id <= 0 {
            return None;
        }
        if matches!(
            entry.service.as_str(),
            "station"
                | "stationSvc"
                | "corpStationMgr"
                | "repairSvc"
                | "reprocessingSvc"
                | "insuranceSvc"
        ) || self.is_station_or_structure_id(entry.object_id)
        {
            return Some(DepartedLocationKind::Dock);
        }
        let is_sys_service = matches!(
            entry.service.as_str(),
            "beyonce" | "scanMgr" | "entity" | "crimewatch" | "planetOrbitalRegistryBroker"
        ) || (matches!(
            entry.service.as_str(),
            "keeper" | "posMgr" | "devIndexManager"
        ) && (30_000_000..=39_999_999).contains(&entry.object_id));
        if is_sys_service && crate::router::binding_auth::is_solar_system_id(self, entry.object_id)
        {
            return Some(DepartedLocationKind::SolarSystem);
        }
        None
    }

    fn insert_departed_bindings_into_store(
        ctx: &ServerContext,
        store: &mut ContextGraceStore,
        entries: &[BoundObjectEntry],
        explicit_scene_epoch: Option<u64>,
        now_ticks: i64,
        touched_sessions: &mut Vec<u64>,
    ) {
        for entry in entries {
            let epoch = explicit_scene_epoch.unwrap_or_else(|| {
                store
                    .scene_epochs
                    .get(&entry.session_id)
                    .copied()
                    .unwrap_or(0)
            });
            if policy_retains_departed_binding(&entry.service) {
                if !touched_sessions.contains(&entry.session_id) {
                    touched_sessions.push(entry.session_id);
                }
                store.entries.insert(
                    entry.bound_id,
                    TransitionDisconnectedBinding {
                        entry: entry.clone(),
                        scene_epoch: epoch,
                        disconnected_ticks: now_ticks,
                    },
                );
            }
            if let Some(kind) = ctx.departed_location_kind_for_entry(entry) {
                if !touched_sessions.contains(&entry.session_id) {
                    touched_sessions.push(entry.session_id);
                }
                store.departed_locations.insert(
                    (entry.session_id, kind, entry.object_id),
                    TransitionDepartedLocation {
                        session_id: entry.session_id,
                        user_id: entry.user_id,
                        character_id: entry.character_id,
                        kind,
                        location_id: entry.object_id,
                        scene_epoch: epoch,
                        departed_ticks: now_ticks,
                    },
                );
            }
        }
    }

    pub(crate) fn record_transition_grace_bindings(&self, entries: &[BoundObjectEntry]) {
        if entries.is_empty() {
            return;
        }
        let has_work = entries.iter().any(|e| {
            policy_retains_departed_binding(&e.service)
                || self.departed_location_kind_for_entry(e).is_some()
        });
        if !has_work {
            return;
        }
        let now_ticks = self.now_machonet_wallclock_filetime_ticks();
        self.with_grace_store_mut(now_ticks, |store| {
            let mut touched_sessions = Vec::new();
            Self::insert_departed_bindings_into_store(
                self,
                store,
                entries,
                None,
                now_ticks,
                &mut touched_sessions,
            );
            Self::trim_store_session_caps(store, &touched_sessions);
        });
    }

    pub(crate) fn purge_transition_grace_with<FE, FL>(&self, mut entry_pred: FE, mut loc_pred: FL)
    where
        FE: FnMut(&u64, &BoundObjectEntry) -> bool,
        FL: FnMut(&(u64, DepartedLocationKind, i64), i64) -> bool,
    {
        self.with_existing_grace_store_mut_and_prune(|store| {
            store.entries.retain(|bid, v| !entry_pred(bid, &v.entry));
            store
                .departed_locations
                .retain(|k, v| !loc_pred(k, v.character_id));
        });
    }

    pub(crate) fn purge_transition_grace_for_session(&self, session_id: u64) {
        self.with_existing_grace_store_mut_and_prune(|store| {
            store.entries.retain(|_, v| v.entry.session_id != session_id);
            store
                .departed_locations
                .retain(|&(sid, _, _), v| sid != session_id && v.session_id != session_id);
            store.epoch_records.remove(&session_id);
        });
    }

    pub(crate) fn purge_transition_grace_for_user(&self, user_id: u64) {
        self.purge_transition_grace_with(
            |_, e| e.user_id == user_id,
            |_, _| false,
        );
        self.with_existing_grace_store_mut_and_prune(|store| {
            store
                .departed_locations
                .retain(|_, v| v.user_id != user_id);
        });
    }

    pub(crate) fn purge_transition_grace_entry(&self, session_id: u64, bound_id: u64) {
        self.purge_transition_grace_with(
            |&bid, e| bid == bound_id && e.session_id == session_id,
            |_, _| false,
        );
    }

    fn compute_departed_location_deltas(
        prev_sys: Option<u32>,
        prev_dock: Option<i64>,
        next_sys: Option<u32>,
        next_dock: Option<i64>,
    ) -> (Vec<(DepartedLocationKind, i64)>, Vec<(DepartedLocationKind, i64)>) {
        let mut record = Vec::new();
        let mut purge = Vec::new();
        let prev_sys_id = prev_sys.map(i64::from);
        let next_sys_id = next_sys.map(i64::from);
        if prev_sys_id != next_sys_id {
            if let Some(old_sys) = prev_sys_id.filter(|&id| id > 0) {
                record.push((DepartedLocationKind::SolarSystem, old_sys));
            }
            if let Some(new_sys) = next_sys_id.filter(|&id| id > 0) {
                purge.push((DepartedLocationKind::SolarSystem, new_sys));
            }
        }
        if prev_dock != next_dock {
            if let Some(old_dock) = prev_dock.filter(|&id| id > 0) {
                record.push((DepartedLocationKind::Dock, old_dock));
            }
            if let Some(new_dock) = next_dock.filter(|&id| id > 0) {
                purge.push((DepartedLocationKind::Dock, new_dock));
            }
        }
        (record, purge)
    }

    #[cfg(test)]
    pub(crate) fn update_transition_departed_dock(
        &self,
        session: &SessionState,
        prev_dock: Option<i64>,
        next_dock: Option<i64>,
    ) {
        self.update_transition_departed_locations(session, None, prev_dock, None, next_dock);
    }

    pub(crate) fn update_transition_departed_locations(
        &self,
        session: &SessionState,
        prev_sys: Option<u32>,
        prev_dock: Option<i64>,
        next_sys: Option<u32>,
        next_dock: Option<i64>,
    ) {
        let (record, purge) =
            Self::compute_departed_location_deltas(prev_sys, prev_dock, next_sys, next_dock);
        self.apply_departed_location_deltas(session, &record, &purge);
    }

    pub(crate) fn sync_transition_grace_on_session_change(
        &self,
        prev: &SessionState,
        next: &SessionState,
    ) {
        if prev.character_id.is_some() && next.character_id != prev.character_id {
            self.purge_transition_grace_for_session(next.session_id);
        } else if prev.character_id == next.character_id
            && next.character_id.is_some()
            && next.session_id != 0
        {
            self.update_transition_departed_locations(
                next,
                prev.solar_system_id,
                prev.station_id.or(prev.structure_id),
                next.solar_system_id,
                next.station_id.or(next.structure_id),
            );
        }
    }

    fn apply_departed_location_deltas(
        &self,
        session: &SessionState,
        record: &[(DepartedLocationKind, i64)],
        purge: &[(DepartedLocationKind, i64)],
    ) {
        if record.is_empty() && purge.is_empty() {
            return;
        }
        if record.is_empty() {
            self.with_existing_grace_store_mut_and_prune(|store| {
                store.departed_locations.retain(|&(sid, kind, lid), _| {
                    !(sid == session.session_id && purge.contains(&(kind, lid)))
                });
            });
            return;
        }
        let can_record = session
            .user_id
            .zip(session.character_id)
            .filter(|&(uid, cid)| self.has_matching_online_session(session, uid, cid));
        let Some((uid, cid)) = can_record else {
            if !purge.is_empty() {
                self.with_existing_grace_store_mut_and_prune(|store| {
                    store.departed_locations.retain(|&(sid, kind, lid), _| {
                        !(sid == session.session_id && purge.contains(&(kind, lid)))
                    });
                });
            }
            return;
        };
        let now_ticks = self.now_machonet_wallclock_filetime_ticks();
        self.with_grace_store_mut(now_ticks, |store| {
            let epoch = store.scene_epochs.get(&session.session_id).copied().unwrap_or(0);
            for &(kind, lid) in purge {
                store
                    .departed_locations
                    .remove(&(session.session_id, kind, lid));
            }
            for &(kind, lid) in record {
                store.departed_locations.insert(
                    (session.session_id, kind, lid),
                    TransitionDepartedLocation {
                        session_id: session.session_id,
                        user_id: uid,
                        character_id: cid,
                        kind,
                        location_id: lid,
                        scene_epoch: epoch,
                        departed_ticks: now_ticks,
                    },
                );
            }
            Self::trim_store_session_caps(store, &[session.session_id]);
        });
    }

    /// Allocates a temporary epoch-scoped grace binding (kept out of active `bound_objects`)
    /// when `MachoBindObject` against `(service, object_id)` failed with `BindAuthorizationDenied`,
    /// provided `(service, method)` has a declarative `LateCallSurface::BoundGraceOrDepartedBind`
    /// rule whose predicate and departed-location kind match (`#1150`, `#1209`, `#1190`).
    pub(crate) fn allocate_late_call_grace_binding(
        &self,
        session: &SessionState,
        service: &str,
        object_id: i64,
        method: &str,
        call_args: &[PyRep],
        call_kwargs: &HashMap<String, PyRep>,
    ) -> Option<u64> {
        if object_id <= 0 {
            return None;
        }
        let rule = lookup_late_call_rule(service, method)?;
        if rule.surface != LateCallSurface::BoundGraceOrDepartedBind
            || matches!(rule.policy, LateCallPolicy::StaleObjectGone)
            || rule.allowed_departed_kinds.is_empty()
        {
            return None;
        }
        let uid = session.user_id?;
        let cid = session.character_id?;
        if !self.has_matching_online_session(session, uid, cid) {
            return None;
        }
        if !self.evaluate_late_call_predicate(rule.predicate, session, cid, call_args, call_kwargs)
        {
            return None;
        }
        let now_ticks = self.now_machonet_wallclock_filetime_ticks();
        self.with_grace_store_mut(now_ticks, |store| {
            let current_epoch = store.scene_epochs.get(&session.session_id).copied().unwrap_or(0);
            let matching_epoch = rule.allowed_departed_kinds.iter().find_map(|&kind| {
                store
                    .departed_locations
                    .get(&(session.session_id, kind, object_id))
                    .filter(|departed| {
                        Self::is_grace_ticks_active(departed.departed_ticks, now_ticks)
                            && departed.session_id == session.session_id
                            && departed.user_id == uid
                            && departed.character_id == cid
                            && departed.kind == kind
                            && departed.location_id == object_id
                            && departed.scene_epoch <= current_epoch
                    })
                    .map(|departed| departed.scene_epoch)
            })?;
            let bound_id = NEXT_BOUND_ID.fetch_add(1, Ordering::Relaxed);
            let entry = self.make_bound_object_entry(
                session.session_id,
                bound_id,
                uid,
                cid,
                service,
                object_id,
            );
            store.entries.insert(
                bound_id,
                TransitionDisconnectedBinding {
                    entry,
                    scene_epoch: matching_epoch,
                    disconnected_ticks: now_ticks,
                },
            );
            Self::trim_store_session_caps(store, &[session.session_id]);
            Some(bound_id)
        })
    }

    fn lookup_transition_grace_entry_with_epoch(
        &self,
        session: &SessionState,
        bound_id: u64,
    ) -> Option<(BoundObjectEntry, u64, u64)> {
        let uid = session.user_id?;
        let cid = session.character_id?;
        let now_ticks = self.now_machonet_wallclock_filetime_ticks();
        let (entry, entry_epoch, current_epoch) = {
            let outer = TRANSITION_DISCONNECT_GRACE
                .get()?
                .read()
                .unwrap_or_else(|e| e.into_inner());
            let store = outer.get(&self.instance_id)?;
            let binding = store.entries.get(&bound_id)?;
            let current_epoch = store.scene_epochs.get(&session.session_id).copied().unwrap_or(0);
            if !Self::is_grace_entry_active(binding, now_ticks)
                || binding.scene_epoch > current_epoch
                || binding.entry.session_id != session.session_id
                || binding.entry.user_id != uid
                || binding.entry.character_id != cid
            {
                return None;
            }
            (binding.entry.clone(), binding.scene_epoch, current_epoch)
        };
        if !self.has_matching_online_session(session, uid, cid) {
            return None;
        }
        Some((entry, entry_epoch, current_epoch))
    }

    pub(crate) fn transition_grace_service_for_bound_id(
        &self,
        session: &SessionState,
        bound_id: u64,
    ) -> Option<String> {
        self.lookup_transition_grace_entry_with_epoch(session, bound_id)
            .map(|(entry, _, _)| entry.service)
    }

    /// Evaluates `LATE_CALL_POLICY_TABLE` for a late `ObjectCall` whose `bound_id` failed
    /// active `verify_bound_object` (`InvalidBoundObject`).
    pub(crate) fn resolve_late_call_with_policy(
        &self,
        session: &SessionState,
        bound_id: u64,
        method: &str,
        call_args: &[PyRep],
        call_kwargs: &HashMap<String, PyRep>,
    ) -> LateCallResolution {
        let Some((entry, _, _)) = self.lookup_transition_grace_entry_with_epoch(session, bound_id)
        else {
            return LateCallResolution::RejectStale;
        };
        self.apply_late_call_rule_to_entry(session, entry, method, call_args, call_kwargs)
    }

    /// Epoch-pinned variant of [`Self::resolve_late_call_with_policy`] that additionally rejects
    /// cross-epoch replay if `expected_departed_epoch` does not match the binding's scene epoch or
    /// is not strictly older than the session's current epoch (`#1190`).
    #[allow(dead_code)]
    pub(crate) fn resolve_late_call_for_epoch(
        &self,
        session: &SessionState,
        bound_id: u64,
        expected_departed_epoch: u64,
        method: &str,
        call_args: &[PyRep],
        call_kwargs: &HashMap<String, PyRep>,
    ) -> LateCallResolution {
        let Some((entry, entry_epoch, current_epoch)) =
            self.lookup_transition_grace_entry_with_epoch(session, bound_id)
        else {
            return LateCallResolution::RejectStale;
        };
        if entry_epoch != expected_departed_epoch || expected_departed_epoch >= current_epoch {
            return LateCallResolution::RejectStale;
        }
        self.apply_late_call_rule_to_entry(session, entry, method, call_args, call_kwargs)
    }

    fn apply_late_call_rule_to_entry(
        &self,
        session: &SessionState,
        entry: BoundObjectEntry,
        method: &str,
        call_args: &[PyRep],
        call_kwargs: &HashMap<String, PyRep>,
    ) -> LateCallResolution {
        let Some(rule) = lookup_late_call_rule(&entry.service, method) else {
            return LateCallResolution::RejectStale;
        };
        if !matches!(
            rule.surface,
            LateCallSurface::BoundGraceCall | LateCallSurface::BoundGraceOrDepartedBind
        ) {
            return LateCallResolution::RejectStale;
        }
        let Some(cid) = session.character_id else {
            return LateCallResolution::RejectStale;
        };
        if !self.evaluate_late_call_predicate(rule.predicate, session, cid, call_args, call_kwargs)
        {
            return LateCallResolution::RejectStale;
        }
        self.execute_late_call_policy(session, entry, rule.policy)
    }

    fn execute_late_call_policy(
        &self,
        session: &SessionState,
        entry: BoundObjectEntry,
        policy: LateCallPolicy,
    ) -> LateCallResolution {
        match policy {
            LateCallPolicy::Benign(LateCallBenignResponse::DepartedBindingReadOnly) => {
                LateCallResolution::DispatchWithBinding(entry)
            }
            LateCallPolicy::Benign(response) => match response.to_empty_pyrep() {
                Some(rep) => LateCallResolution::RespondImmediate(rep),
                None => LateCallResolution::RejectStale,
            },
            LateCallPolicy::RebindToCurrentScene => {
                match self.rebind_entry_to_current_scene(session, &entry) {
                    Some(rebound) => LateCallResolution::DispatchWithBinding(rebound),
                    None => LateCallResolution::RejectStale,
                }
            }
            LateCallPolicy::StaleObjectGone => LateCallResolution::RejectStale,
        }
    }

    fn rebind_entry_to_current_scene(
        &self,
        session: &SessionState,
        departed_entry: &BoundObjectEntry,
    ) -> Option<BoundObjectEntry> {
        let current_obj_id = current_scene_target_object_id(session, &departed_entry.service)?;
        let existing_bid = self.bound_objects.read().ok().and_then(|map| {
            map.iter()
                .find(|(_, e)| {
                    e.session_id == session.session_id
                        && e.service == departed_entry.service
                        && e.object_id == current_obj_id
                })
                .map(|(&bid, _)| bid)
        });
        let bid = match existing_bid {
            Some(bid) => bid,
            None => self
                .bind_object(session, &departed_entry.service, current_obj_id)
                .ok()?,
        };
        self.verify_bound_object(session, bid).ok()
    }

    /// Authorizes a departed-scene bound object via declarative policy lookup when service logic
    /// (such as `invbroker`) inspects whether `bound_id` refers to `object_id` under active
    /// transition grace (`#1146`).
    pub(crate) fn authorize_departed_object_late_call(
        &self,
        session: &SessionState,
        service: &str,
        bound_id: u64,
        object_id: i64,
    ) -> bool {
        if !policy_retains_departed_binding(service) {
            return false;
        }
        self.lookup_transition_grace_entry_with_epoch(session, bound_id)
            .is_some_and(|(entry, _, _)| entry.service == service && entry.object_id == object_id)
    }

    /// Evaluates the shared `LATE_CALL_POLICY_TABLE` for `SafeLogoffTeardownGrace` read-only RPCs
    /// arriving while `session` sits at character selection (`#1360`, `#1386`, `#1434`, `#1477`).
    /// Returns `Some(creates_temp_binding)` if allowed, or `None` to fail closed.
    pub(crate) fn evaluate_safe_logoff_teardown_late_call(
        &self,
        session: &SessionState,
        grace_char_id: i64,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
        call_args: &[PyRep],
        call_kwargs: &HashMap<String, PyRep>,
    ) -> Option<bool> {
        if let Some(bid) = bound_id {
            let bound_service = self.bound_objects.read().ok().and_then(|m| {
                m.get(&bid)
                    .filter(|e| {
                        e.session_id == session.session_id
                            && Some(e.user_id) == session.user_id
                            && e.character_id == grace_char_id
                    })
                    .map(|e| e.service.clone())
            })?;
            let rule = lookup_late_call_rule(&bound_service, method)?;
            if rule.surface != LateCallSurface::SafeLogoffTeardownBoundOrUnbound {
                return None;
            }
            let LateCallPolicy::Benign(LateCallBenignResponse::TeardownGraceReadOnly {
                creates_temp_binding,
            }) = rule.policy
            else {
                return None;
            };
            if !self.evaluate_late_call_predicate(
                rule.predicate,
                session,
                grace_char_id,
                call_args,
                call_kwargs,
            ) {
                return None;
            }
            return Some(creates_temp_binding);
        }

        let rule = lookup_late_call_rule(service, method)?;
        if !matches!(
            rule.surface,
            LateCallSurface::SafeLogoffTeardownUnbound
                | LateCallSurface::SafeLogoffTeardownBoundOrUnbound
        ) {
            return None;
        }
        let LateCallPolicy::Benign(LateCallBenignResponse::TeardownGraceReadOnly {
            creates_temp_binding,
        }) = rule.policy
        else {
            return None;
        };
        if !self.evaluate_late_call_predicate(
            rule.predicate,
            session,
            grace_char_id,
            call_args,
            call_kwargs,
        ) {
            return None;
        }
        Some(creates_temp_binding)
    }

    /// Removes temporary `skillMgr2` bound objects created during `SafeLogoffTeardownGrace`
    /// when teardown grace expires, clears, or is displaced.
    pub(crate) fn clear_safe_logoff_temp_bindings(&self, session_id: u64, char_id: i64) {
        self.retain_bound_objects(|_, e| {
            !(e.session_id == session_id && e.character_id == char_id && e.service == "skillMgr2")
        });
    }
}

#[cfg(test)]
#[path = "binding_transition_policy_tests.rs"]
mod binding_transition_policy_tests;
