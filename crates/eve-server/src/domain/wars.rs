//! Protocol-agnostic corporation/alliance war domain state transitions, predicates,
//! and Windows FILETIME conversion helpers (`#1488`, `#506`, `#1196`).
//!
//! All reads of "active war / active ally" honor war combat warm-up (`WAR_TIMER_SECS` for
//! non-mutual wars), scheduled finish timestamps (`finish_at_unix`), and ally windows
//! (`[effective_unix, ally_expires_unix)`). All state mutations update `snap.wars`,
//! `snap.war_state`, and `snap.war_lifecycle_meta` together so the three stores never diverge.

#![forbid(unsafe_code)]

use crate::domain::DomainError;
use eve_db::clock::{FILETIME_TICKS_PER_SEC, FILETIME_UNIX_EPOCH_DIFF_SECS, WallUnixMs};
use eve_db::snapshot_ext::WarAllyRecord;
use eve_db::{
    IskCents, PersistedNegKind, PersistedPeaceTreaty, PersistedWarMeta, PersistedWarNegotiation,
    WarRecord, WorldStateSnapshot,
};

/// War declaration ISK fee charged to the aggressor corporation's master wallet (`#506`).
pub const WAR_DECLARATION_FEE_ISK: i64 = 100_000_000;
/// 24-hour warm-up / cooldown window (seconds) for war start, surrender, ally entry, and retraction.
pub const WAR_TIMER_SECS: u64 = 86_400;
/// 14-day peace treaty duration (seconds) after an accepted surrender negotiation.
pub const PEACE_TREATY_SECS: u64 = 14 * 86_400;
/// Maximum relative extension duration accepted by `GMExtendAllyContract` (seconds, 365 days).
pub const MAX_ALLY_CONTRACT_EXTEND_SECS: u64 = 365 * 86_400;

/// Action requested against a pending `"Offered"` ally negotiation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllyNegotiationAction {
    Accept,
    Decline,
    Retract,
}

/// Outcome of [`accept_mutual_war_invite`], committing stale-invite cleanup on soft rejections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutualWarAcceptOutcome {
    /// Invite accepted and mutual war created.
    Started { war_id: i64 },
    /// Both corps belong to the same alliance; stale invite was removed.
    RejectedSameAlliance,
    /// Corps are already at war; stale invite was removed.
    RejectedAlreadyAtWar,
}

impl AllyNegotiationAction {
    /// Wire status string written to `PersistedWarNegotiation::status`.
    pub const fn status_str(self) -> &'static str {
        match self {
            Self::Accept => "Accepted",
            Self::Decline => "Declined",
            Self::Retract => "Retracted",
        }
    }
}

/// Converts Unix epoch seconds (`>= 0`) into Windows FILETIME ticks (`100ns` since `1601-01-01 UTC`).
#[inline]
pub fn unix_to_filetime(unix_secs: u64) -> i64 {
    WallUnixMs::checked_from_secs(unix_secs)
        .and_then(|ms| ms.to_wall_filetime())
        .map_or_else(
            || {
                i64::try_from(unix_secs)
                    .unwrap_or(i64::MAX)
                    .saturating_add(FILETIME_UNIX_EPOCH_DIFF_SECS)
                    .saturating_mul(FILETIME_TICKS_PER_SEC)
            },
            |ft| ft.as_ticks(),
        )
}

/// Converts an optional Unix timestamp into Windows FILETIME ticks.
#[inline]
pub fn opt_unix_to_filetime(unix_secs: Option<u64>) -> Option<i64> {
    unix_secs.map(unix_to_filetime)
}

/// Resolves the positive alliance ID for a corporation (`CorporationRecord::alliance_id`)
/// or a known alliance entity (`snap.alliances` or any member corporation's `alliance_id`).
pub fn resolve_entity_alliance_id(snap: &WorldStateSnapshot, entity_id: i64) -> Option<i64> {
    if entity_id <= 0 {
        return None;
    }
    if let Some(corp) = snap.corporations.get(&entity_id) {
        return corp.alliance_id.filter(|&a| a > 0);
    }
    (snap.alliances.contains_key(&entity_id)
        || snap
            .corporations
            .values()
            .any(|c| c.alliance_id == Some(entity_id)))
    .then_some(entity_id)
}

#[inline]
fn effective_alliance_id(
    snap: &WorldStateSnapshot,
    entity_id: i64,
    alliance_hint: Option<i64>,
) -> Option<i64> {
    alliance_hint
        .filter(|&a| a > 0)
        .or_else(|| resolve_entity_alliance_id(snap, entity_id))
}

/// Returns `true` when `entity_a` and `entity_b` share the same positive alliance.
pub fn same_alliance(snap: &WorldStateSnapshot, entity_a: i64, entity_b: i64) -> bool {
    let aa = resolve_entity_alliance_id(snap, entity_a);
    aa.is_some() && aa == resolve_entity_alliance_id(snap, entity_b)
}

/// Returns `true` if `entity_id` matches war owner/ally `owner_id` directly, or belongs to
/// alliance `owner_id` (when `owner_id` is an alliance entity rather than a corporation).
pub fn entity_matches_war_owner(
    snap: &WorldStateSnapshot,
    entity_id: i64,
    entity_alliance_hint: Option<i64>,
    owner_id: i64,
) -> bool {
    if owner_id <= 0 {
        return false;
    }
    if entity_id > 0 && entity_id == owner_id {
        return true;
    }
    let resolved_alliance = effective_alliance_id(snap, entity_id, entity_alliance_hint);
    !snap.corporations.contains_key(&owner_id) && resolved_alliance == Some(owner_id)
}

/// Returns `true` when `corp_id` is `side_id` itself or is the executor corp of alliance `side_id`.
pub fn corp_represents_war_side(snap: &WorldStateSnapshot, corp_id: i64, side_id: i64) -> bool {
    corp_id > 0
        && side_id > 0
        && (corp_id == side_id
            || snap
                .alliances
                .get(&side_id)
                .is_some_and(|a| a.executor_corp_id == corp_id))
}

/// Returns `true` when `[effective_unix, expires_unix)` covers `at_unix`.
#[inline]
pub fn is_ally_window_active_at(
    effective_unix: u64,
    expires_unix: Option<u64>,
    at_unix: u64,
) -> bool {
    at_unix >= effective_unix && expires_unix.is_none_or(|exp| at_unix < exp)
}

/// Returns `true` when accepted ally negotiation `n` is active at `at_unix`
/// (`at_unix >= effective_unix` and not past any configured `ally_expires_unix`).
pub fn is_ally_neg_active_at(
    snap: &WorldStateSnapshot,
    n: &PersistedWarNegotiation,
    at_unix: u64,
) -> bool {
    n.kind == PersistedNegKind::Ally
        && n.status == "Accepted"
        && is_ally_window_active_at(
            n.effective_unix.unwrap_or(0),
            snap.war_state.ally_expires_unix.get(&(n.war_id, n.from_corp)).copied(),
            at_unix,
        )
}

/// Returns `true` when `w` permits legal war combat at `now_unix`:
/// `w.active`, not past `finish_at_unix`, and either `w.mutual` or past 24h `WAR_TIMER_SECS`.
pub fn is_war_combat_active_at(snap: &WorldStateSnapshot, w: &WarRecord, now_unix: u64) -> bool {
    if !w.active {
        return false;
    }
    if snap
        .war_state
        .wars
        .get(&w.war_id)
        .and_then(|m| m.finish_at_unix)
        .is_some_and(|f| now_unix >= f)
    {
        return false;
    }
    w.mutual || now_unix >= w.started_unix.saturating_add(WAR_TIMER_SECS)
}

#[inline]
fn is_defender_or_ally_opt(
    snap: &WorldStateSnapshot,
    w: &WarRecord,
    entity_id: i64,
    alliance_hint: Option<i64>,
    now_unix_opt: Option<u64>,
) -> bool {
    entity_matches_war_owner(snap, entity_id, alliance_hint, w.defender_corp_id)
        || snap.war_state.negotiations.iter().any(|n| {
            let ally_ok = match now_unix_opt {
                Some(now) => is_ally_neg_active_at(snap, n, now),
                None => {
                    n.kind == PersistedNegKind::Ally
                        && n.status == "Accepted"
                        && snap
                            .war_state
                            .ally_expires_unix
                            .get(&(n.war_id, n.from_corp))
                            .is_none_or(|&exp| exp > n.effective_unix.unwrap_or(0))
                }
            };
            n.war_id == w.war_id
                && ally_ok
                && entity_matches_war_owner(snap, entity_id, alliance_hint, n.from_corp)
        })
}

/// Returns `true` when an unended war (`w.active`) already exists directly between `corp_a` and `corp_b`
/// (including during the initial 24h declaration warm-up, preventing duplicate declarations).
pub fn already_at_war(snap: &WorldStateSnapshot, corp_a: i64, corp_b: i64) -> bool {
    snap.wars.values().any(|w| {
        w.active
            && ((w.aggressor_corp_id == corp_a && w.defender_corp_id == corp_b)
                || (w.aggressor_corp_id == corp_b && w.defender_corp_id == corp_a))
    })
}

/// Canonical check for whether `entity_a` and `entity_b` are at combat-active war at `now_unix`,
/// honoring `w.active`, `finish_at_unix`, 24h spool-up, alliance wars, and `ally_expires_unix`.
pub fn active_war_between(
    snap: &WorldStateSnapshot,
    entity_a: i64,
    entity_b: i64,
    now_unix: u64,
) -> bool {
    active_war_between_with_hints(snap, entity_a, None, entity_b, None, now_unix)
}

/// Same as [`active_war_between`], also accepting explicit character/session `alliance_id` hints
/// when `CorporationRecord::alliance_id` is absent (such as isolated CrimeWatch test snapshots).
pub fn active_war_between_with_hints(
    snap: &WorldStateSnapshot,
    entity_a: i64,
    alliance_a_hint: Option<i64>,
    entity_b: i64,
    alliance_b_hint: Option<i64>,
    now_unix: u64,
) -> bool {
    active_war_between_opt(
        snap,
        entity_a,
        alliance_a_hint,
        entity_b,
        alliance_b_hint,
        Some(now_unix),
    )
}

/// Shared active-war check supporting both timestamped combat windows (`Some(now)`) and
/// untimed structure ACL checks (`None`, enforcing `w.active`, alliance ownership, and allies).
pub fn active_war_between_opt(
    snap: &WorldStateSnapshot,
    entity_a: i64,
    alliance_a_hint: Option<i64>,
    entity_b: i64,
    alliance_b_hint: Option<i64>,
    now_unix_opt: Option<u64>,
) -> bool {
    if entity_a > 0 && entity_a == entity_b {
        return false;
    }
    let all_a = effective_alliance_id(snap, entity_a, alliance_a_hint);
    let all_b = effective_alliance_id(snap, entity_b, alliance_b_hint);
    if (entity_a <= 0 && all_a.is_none()) || (entity_b <= 0 && all_b.is_none()) {
        return false;
    }
    if all_a.is_some() && all_a == all_b {
        return false;
    }
    snap.wars.values().any(|w| {
        let war_ok = match now_unix_opt {
            Some(now) => is_war_combat_active_at(snap, w, now),
            None => w.active,
        };
        if !war_ok {
            return false;
        }
        let a_agg = entity_matches_war_owner(snap, entity_a, alliance_a_hint, w.aggressor_corp_id);
        let b_agg = entity_matches_war_owner(snap, entity_b, alliance_b_hint, w.aggressor_corp_id);
        (a_agg && is_defender_or_ally_opt(snap, w, entity_b, alliance_b_hint, now_unix_opt))
            || (b_agg && is_defender_or_ally_opt(snap, w, entity_a, alliance_a_hint, now_unix_opt))
    })
}

/// Returns `true` when `entity_id` (or `alliance_hint`) is engaged in an ongoing unended war
/// (`w.active` and not past `finish_at_unix`), including active defender allies.
pub fn is_entity_in_ongoing_war(
    snap: &WorldStateSnapshot,
    entity_id: i64,
    alliance_hint: Option<i64>,
    now_unix: u64,
) -> bool {
    snap.wars.values().any(|w| {
        if !w.active
            || snap
                .war_state
                .wars
                .get(&w.war_id)
                .and_then(|m| m.finish_at_unix)
                .is_some_and(|f| now_unix >= f)
        {
            return false;
        }
        entity_matches_war_owner(snap, entity_id, alliance_hint, w.aggressor_corp_id)
            || is_defender_or_ally_opt(snap, w, entity_id, alliance_hint, Some(now_unix))
    })
}

/// Returns `true` when `helper` assisting `beneficiary` crosses a combat-active war boundary
/// (CrimeWatch war-interference suspect rule), honoring `ally_expires_unix`.
pub fn is_war_interference(
    snap: &WorldStateSnapshot,
    helper_corp: i64,
    helper_alliance_hint: Option<i64>,
    beneficiary_corp: i64,
    beneficiary_alliance_hint: Option<i64>,
    now_unix: u64,
) -> bool {
    snap.wars.values().any(|w| {
        if !is_war_combat_active_at(snap, w, now_unix) {
            return false;
        }
        let ben_agg = entity_matches_war_owner(
            snap,
            beneficiary_corp,
            beneficiary_alliance_hint,
            w.aggressor_corp_id,
        );
        let helper_agg =
            entity_matches_war_owner(snap, helper_corp, helper_alliance_hint, w.aggressor_corp_id);
        let ben_def = is_defender_or_ally_opt(
            snap,
            w,
            beneficiary_corp,
            beneficiary_alliance_hint,
            Some(now_unix),
        );
        let helper_def = is_defender_or_ally_opt(
            snap,
            w,
            helper_corp,
            helper_alliance_hint,
            Some(now_unix),
        );
        (ben_agg && !helper_agg) || (ben_def && !helper_def)
    })
}

/// Returns `true` when `entity_id` (or `alliance_hint`) is a party to `war_id` at `now_unix`:
/// direct aggressor/defender, member of aggressor/defender alliance, or an active defender ally
/// honoring `[effective_unix, ally_expires_unix)`.
pub fn is_war_participant(
    snap: &WorldStateSnapshot,
    war_id: i64,
    entity_id: i64,
    alliance_hint: Option<i64>,
    now_unix: u64,
) -> bool {
    let Some(w) = snap.wars.get(&war_id) else {
        return false;
    };
    let eff_all = effective_alliance_id(snap, entity_id, alliance_hint);
    if entity_id <= 0 && eff_all.is_none() {
        return false;
    }
    if entity_id > 0 && (entity_id == w.aggressor_corp_id || entity_id == w.defender_corp_id) {
        return true;
    }
    if eff_all.is_some()
        && (eff_all == Some(w.aggressor_corp_id)
            || eff_all == Some(w.defender_corp_id)
            || eff_all == resolve_entity_alliance_id(snap, w.aggressor_corp_id)
            || eff_all == resolve_entity_alliance_id(snap, w.defender_corp_id))
    {
        return true;
    }
    snap.war_state.negotiations.iter().any(|n| {
        n.war_id == war_id
            && is_ally_neg_active_at(snap, n, now_unix)
            && entity_matches_war_owner(snap, entity_id, alliance_hint, n.from_corp)
    })
}

fn is_war_defender_or_ally_for_killmail(
    snap: &WorldStateSnapshot,
    war_id: i64,
    defender_corp_id: i64,
    corp: i64,
    alliance: Option<i64>,
    kill_time_unix: u64,
) -> bool {
    entity_matches_war_owner(snap, corp, alliance, defender_corp_id)
        || snap.war_state.negotiations.iter().any(|n| {
            n.war_id == war_id
                && n.kind == PersistedNegKind::Ally
                && n.status == "Accepted"
                && (kill_time_unix == 0 || is_ally_neg_active_at(snap, n, kill_time_unix))
                && entity_matches_war_owner(snap, corp, alliance, n.from_corp)
        })
}

fn war_matches_killmail(
    snap: &WorldStateSnapshot,
    war_id: i64,
    w: &WarRecord,
    vic_corp: i64,
    vic_alliance: Option<i64>,
    attackers: &[(i64, Option<i64>)],
    kill_time_unix: u64,
) -> bool {
    if kill_time_unix > 0 {
        if kill_time_unix < w.started_unix {
            return false;
        }
        if snap
            .war_state
            .wars
            .get(&war_id)
            .and_then(|m| m.finish_at_unix)
            .is_some_and(|fin| kill_time_unix > fin)
        {
            return false;
        }
    }
    let is_agg = |corp: i64, alliance: Option<i64>| {
        entity_matches_war_owner(snap, corp, alliance, w.aggressor_corp_id)
    };
    let is_def = |corp: i64, alliance: Option<i64>| {
        is_war_defender_or_ally_for_killmail(
            snap,
            war_id,
            w.defender_corp_id,
            corp,
            alliance,
            kill_time_unix,
        )
    };
    (is_agg(vic_corp, vic_alliance) && attackers.iter().any(|&(c, a)| is_def(c, a)))
        || (is_def(vic_corp, vic_alliance) && attackers.iter().any(|&(c, a)| is_agg(c, a)))
}

/// Returns `true` when `caller_corp` (with optional `caller_alliance`) is authorized to read a
/// killmail (`victim` vs `attackers`) at `kill_time_unix` via an active or historical war.
///
/// Honors war `[started_unix, finish_at_unix]` and ally `[effective_unix, ally_expires_unix)`
/// windows whenever `kill_time_unix > 0`.
pub fn is_war_killmail_authorized(
    snap: &WorldStateSnapshot,
    caller_corp: i64,
    caller_alliance: Option<i64>,
    vic_corp: i64,
    vic_alliance: Option<i64>,
    attackers: &[(i64, Option<i64>)],
    kill_time_unix: u64,
) -> bool {
    if caller_corp <= 0 || (1_000_000..2_000_000).contains(&caller_corp) {
        return false;
    }
    snap.wars.iter().any(|(&war_id, w)| {
        war_matches_killmail(
            snap,
            war_id,
            w,
            vic_corp,
            vic_alliance,
            attackers,
            kill_time_unix,
        ) && (entity_matches_war_owner(snap, caller_corp, caller_alliance, w.aggressor_corp_id)
            || is_war_defender_or_ally_for_killmail(
                snap,
                war_id,
                w.defender_corp_id,
                caller_corp,
                caller_alliance,
                kill_time_unix,
            ))
    })
}

/// Resolves the lowest matching `war_id` for a killmail (`victim` vs `attackers`) at
/// `kill_time_unix`, honoring alliance wars, war `[started_unix, finish_at_unix]` bounds, and
/// ally `[effective_unix, ally_expires_unix)` windows.
pub fn resolve_killmail_war_id(
    snap: &WorldStateSnapshot,
    vic_corp: i64,
    vic_alliance: Option<i64>,
    attackers: &[(i64, Option<i64>)],
    kill_time_unix: u64,
) -> Option<i64> {
    if (vic_corp <= 0 && vic_alliance.is_none()) || attackers.is_empty() {
        return None;
    }
    snap.wars
        .iter()
        .filter(|(&war_id, w)| {
            war_matches_killmail(
                snap,
                war_id,
                w,
                vic_corp,
                vic_alliance,
                attackers,
                kill_time_unix,
            )
        })
        .map(|(&war_id, _)| war_id)
        .min()
}

fn ally_record_for_entity(snap: &WorldStateSnapshot, ally_id: i64) -> WarAllyRecord {
    if !snap.corporations.contains_key(&ally_id)
        && resolve_entity_alliance_id(snap, ally_id) == Some(ally_id)
    {
        WarAllyRecord {
            corporation_id: None,
            alliance_id: Some(ally_id),
        }
    } else {
        WarAllyRecord {
            corporation_id: Some(ally_id),
            alliance_id: None,
        }
    }
}

fn ally_record_matches_id(ally: &WarAllyRecord, ally_id: i64) -> bool {
    ally.corporation_id == Some(ally_id) || ally.alliance_id == Some(ally_id)
}

fn is_lifecycle_ally_expired(
    snap: &WorldStateSnapshot,
    war_id: i64,
    ally: &WarAllyRecord,
    now_unix: u64,
) -> bool {
    [ally.corporation_id, ally.alliance_id]
        .into_iter()
        .flatten()
        .any(|id| {
            snap.war_state
                .ally_expires_unix
                .get(&(war_id, id))
                .is_some_and(|&exp| now_unix >= exp)
        })
}

/// Returns active defender [`WarAllyRecord`]s for `war_id` at `at_unix`, honoring ally
/// `[effective_unix, ally_expires_unix)` windows while preserving unnegotiated lifecycle seeds.
pub fn active_war_ally_records(
    snap: &WorldStateSnapshot,
    war_id: i64,
    at_unix: u64,
) -> Vec<WarAllyRecord> {
    let lm_allies = snap
        .war_lifecycle_meta
        .get(&war_id)
        .map(|m| m.allies.as_slice())
        .unwrap_or(&[]);
    let mut out: Vec<WarAllyRecord> = Vec::new();
    for n in &snap.war_state.negotiations {
        if n.war_id == war_id && is_ally_neg_active_at(snap, n, at_unix) {
            let rec = lm_allies
                .iter()
                .find(|a| ally_record_matches_id(a, n.from_corp))
                .cloned()
                .unwrap_or_else(|| ally_record_for_entity(snap, n.from_corp));
            if !out.contains(&rec) {
                out.push(rec);
            }
        }
    }
    for ally in lm_allies {
        let has_neg = snap.war_state.negotiations.iter().any(|n| {
            n.war_id == war_id
                && n.kind == PersistedNegKind::Ally
                && ally_record_matches_id(ally, n.from_corp)
        });
        if !has_neg
            && !is_lifecycle_ally_expired(snap, war_id, ally, at_unix)
            && !out.contains(ally)
        {
            out.push(ally.clone());
        }
    }
    out
}

/// Returns `true` when any `war_lifecycle_meta` ally entry has expired at or before `now_unix`.
pub fn has_expired_lifecycle_allies(snap: &WorldStateSnapshot, now_unix: u64) -> bool {
    snap.war_lifecycle_meta.iter().any(|(&war_id, lm)| {
        lm.allies
            .iter()
            .any(|ally| is_lifecycle_ally_expired(snap, war_id, ally, now_unix))
    })
}

/// Prunes expired allies (`now_unix >= ally_expires_unix`) from `snap.war_lifecycle_meta`.
pub fn prune_expired_lifecycle_allies(snap: &mut WorldStateSnapshot, now_unix: u64) {
    let exp_map = &snap.war_state.ally_expires_unix;
    for (&war_id, lm) in &mut snap.war_lifecycle_meta {
        lm.allies.retain(|ally| {
            ![ally.corporation_id, ally.alliance_id]
                .into_iter()
                .flatten()
                .any(|id| exp_map.get(&(war_id, id)).is_some_and(|&exp| now_unix >= exp))
        });
    }
}

/// Resolves `(time_started_unix, time_finished_unix)` for an accepted ally in `war_id`.
pub fn ally_contract_window(
    snap: &WorldStateSnapshot,
    war_id: i64,
    ally_id: i64,
    fallback_now_unix: u64,
) -> (u64, u64) {
    let started = snap
        .war_state
        .negotiations
        .iter()
        .rev()
        .find(|n| {
            n.war_id == war_id
                && n.kind == PersistedNegKind::Ally
                && n.status == "Accepted"
                && n.from_corp == ally_id
        })
        .and_then(|n| n.effective_unix)
        .unwrap_or(fallback_now_unix);
    let finished = snap
        .war_state
        .ally_expires_unix
        .get(&(war_id, ally_id))
        .copied()
        .unwrap_or_else(|| started.saturating_add(WAR_TIMER_SECS));
    (started, finished)
}

/// Resolves a valid GM defender ally into a [`WarAllyRecord`] (corporation or alliance), rejecting
/// invalid IDs and war participants/same-alliance entities.
pub fn war_ally_record_for_entity(
    snap: &WorldStateSnapshot,
    war: &WarRecord,
    ally_id: i64,
) -> Result<WarAllyRecord, DomainError> {
    if ally_id <= 0 {
        return Err(DomainError::invalid_input(
            "InvalidArgument",
            "allyID must be a positive corporation or alliance ID",
        ));
    }
    if ally_id == war.aggressor_corp_id
        || ally_id == war.defender_corp_id
        || same_alliance(snap, ally_id, war.aggressor_corp_id)
        || same_alliance(snap, ally_id, war.defender_corp_id)
    {
        return Err(DomainError::conflict(
            "CannotAllyOwnWar",
            format!("Target {ally_id} cannot ally with its own war {}", war.war_id),
        ));
    }
    if snap.corporations.contains_key(&ally_id) {
        Ok(WarAllyRecord {
            corporation_id: Some(ally_id),
            alliance_id: None,
        })
    } else if resolve_entity_alliance_id(snap, ally_id) == Some(ally_id) {
        Ok(WarAllyRecord {
            corporation_id: None,
            alliance_id: Some(ally_id),
        })
    } else {
        Err(DomainError::invalid_input(
            "InvalidWarTarget",
            format!("Unknown corporation or alliance allyID {ally_id}"),
        ))
    }
}

/// Debits `amount` from `corp_id` master wallet division 1.
pub fn debit_corp(
    snap: &mut WorldStateSnapshot,
    corp_id: i64,
    amount: IskCents,
) -> Result<(), DomainError> {
    let corp = snap.corporations.get_mut(&corp_id).ok_or_else(|| {
        DomainError::not_found("CorporationNotFound", format!("Corporation {corp_id} not found"))
    })?;
    corp.ensure_wallet_divisions();
    let cur = corp.division_balance(1);
    let next = cur.checked_sub(amount).ok_or_else(|| {
        DomainError::precondition_failed(
            "InsufficientFunds",
            format!(
                "Corporation {corp_id} master wallet cannot cover {} ISK",
                amount.to_wire_f64()
            ),
        )
    })?;
    corp.set_division_balance(1, next);
    Ok(())
}

/// Credits `amount` to `corp_id` master wallet division 1 (no-op if `corp_id` is absent).
pub fn credit_corp(snap: &mut WorldStateSnapshot, corp_id: i64, amount: IskCents) {
    if let Some(corp) = snap.corporations.get_mut(&corp_id) {
        corp.credit_division(1, amount);
    }
}

fn insert_war_record(
    snap: &mut WorldStateSnapshot,
    war_id: i64,
    aggressor_corp_id: i64,
    defender_corp_id: i64,
    mutual: bool,
    war_hq_id: Option<i64>,
    now_unix: u64,
) {
    snap.wars.insert(
        war_id,
        WarRecord {
            war_id,
            aggressor_corp_id,
            defender_corp_id,
            mutual,
            started_unix: now_unix,
            active: true,
        },
    );
    snap.war_state.wars.insert(
        war_id,
        PersistedWarMeta {
            war_hq_id,
            ..Default::default()
        },
    );
    let started_unix = if mutual {
        now_unix
    } else {
        now_unix.saturating_add(WAR_TIMER_SECS)
    };
    let meta = snap.war_lifecycle_meta.entry(war_id).or_default();
    meta.declared_unix = Some(now_unix);
    meta.started_unix = Some(started_unix);
    meta.retracted_unix = None;
    meta.finished_unix = None;
    meta.open_for_allies = Some(false);
    meta.allies.clear();
}

/// Charges `fee` from `aggressor_corp_id` and records a new non-mutual war across
/// `snap.wars`, `snap.war_state`, and `snap.war_lifecycle_meta`.
pub fn declare_war(
    snap: &mut WorldStateSnapshot,
    aggressor_corp_id: i64,
    defender_corp_id: i64,
    war_hq_id: i64,
    now_unix: u64,
    fee: IskCents,
) -> Result<i64, DomainError> {
    debit_corp(snap, aggressor_corp_id, fee)?;
    snap.next_entity_id += 1;
    let war_id = snap.next_entity_id;
    insert_war_record(
        snap,
        war_id,
        aggressor_corp_id,
        defender_corp_id,
        false,
        Some(war_hq_id),
        now_unix,
    );
    Ok(war_id)
}

/// Accepts a pending mutual war invite `(from_corp, to_corp)` and inserts the mutual war across
/// `snap.wars`, `snap.war_state`, and `snap.war_lifecycle_meta`.
///
/// Soft domain rejections (`RejectedSameAlliance`, `RejectedAlreadyAtWar`) return `Ok(...)` after
/// deleting the stale invite so transactional callers (`try_with_mut_snapshot`) commit the cleanup.
pub fn accept_mutual_war_invite(
    snap: &mut WorldStateSnapshot,
    from_corp: i64,
    to_corp: i64,
    now_unix: u64,
) -> Result<MutualWarAcceptOutcome, DomainError> {
    if !snap.war_state.mutual_invites.contains_key(&(from_corp, to_corp)) {
        return Err(DomainError::not_found(
            "MutualWarInviteNotFound",
            format!("No pending mutual war invite from {from_corp}"),
        ));
    }
    if same_alliance(snap, from_corp, to_corp) {
        snap.war_state.mutual_invites.remove(&(from_corp, to_corp));
        return Ok(MutualWarAcceptOutcome::RejectedSameAlliance);
    }
    if already_at_war(snap, from_corp, to_corp) {
        snap.war_state.mutual_invites.remove(&(from_corp, to_corp));
        return Ok(MutualWarAcceptOutcome::RejectedAlreadyAtWar);
    }
    snap.war_state.mutual_invites.remove(&(from_corp, to_corp));
    snap.war_state.mutual_invites.remove(&(to_corp, from_corp));
    snap.next_entity_id += 1;
    let war_id = snap.next_entity_id;
    insert_war_record(snap, war_id, from_corp, to_corp, true, None, now_unix);
    Ok(MutualWarAcceptOutcome::Started { war_id })
}

/// Marks `war_id` inactive across `snap.wars`, `snap.war_state.wars`, and `snap.war_lifecycle_meta`.
pub fn deactivate_war(snap: &mut WorldStateSnapshot, war_id: i64, finish_unix: u64) {
    if let Some(w) = snap.wars.get_mut(&war_id) {
        w.active = false;
    }
    snap.war_state
        .wars
        .entry(war_id)
        .or_default()
        .finish_at_unix
        .get_or_insert(finish_unix);
    snap.war_lifecycle_meta
        .entry(war_id)
        .or_default()
        .finished_unix
        .get_or_insert(finish_unix);
}

/// Updates `open_for_allies` across `snap.war_state.wars` and `snap.war_lifecycle_meta`, returning
/// the previous `open_for_allies` flag.
pub fn set_open_for_allies(snap: &mut WorldStateSnapshot, war_id: i64, open: bool) -> bool {
    snap.war_lifecycle_meta
        .entry(war_id)
        .or_default()
        .open_for_allies = Some(open);
    let meta = snap.war_state.wars.entry(war_id).or_default();
    std::mem::replace(&mut meta.open_for_allies, open)
}

/// Updates `started_unix` (`timeDeclared`) and lifecycle metadata (`declared_unix`, `started_unix`),
/// returning the previous declaration Unix timestamp.
pub fn set_declare_time(
    snap: &mut WorldStateSnapshot,
    war_id: i64,
    declared_unix: u64,
) -> Result<u64, DomainError> {
    let Some(w) = snap.wars.get_mut(&war_id) else {
        return Err(DomainError::not_found(
            "WarNotFound",
            format!("War {war_id} not found"),
        ));
    };
    let prev_declared = w.started_unix;
    let is_mutual = w.mutual;
    w.started_unix = declared_unix;
    let lm = snap.war_lifecycle_meta.entry(war_id).or_default();
    lm.declared_unix = Some(declared_unix);
    lm.started_unix = Some(if is_mutual {
        declared_unix
    } else {
        declared_unix.saturating_add(WAR_TIMER_SECS)
    });
    Ok(prev_declared)
}

/// Updates war finish timestamp (`finish_at_unix`, `finished_unix`, and `active`), returning the
/// previous `finish_at_unix`.
pub fn set_finish_time(
    snap: &mut WorldStateSnapshot,
    war_id: i64,
    new_finish: Option<u64>,
    now_unix: u64,
) -> Result<Option<u64>, DomainError> {
    let Some(w) = snap.wars.get_mut(&war_id) else {
        return Err(DomainError::not_found(
            "WarNotFound",
            format!("War {war_id} not found"),
        ));
    };
    let prev_finish = snap.war_state.wars.get(&war_id).and_then(|m| m.finish_at_unix);
    snap.war_state.wars.entry(war_id).or_default().finish_at_unix = new_finish;
    snap.war_lifecycle_meta.entry(war_id).or_default().finished_unix = new_finish;
    w.active = new_finish.is_none_or(|f| f > now_unix);
    Ok(prev_finish)
}

/// Retracts a mutual war, scheduling termination after `WAR_TIMER_SECS` in both `war_state.wars`
/// and `war_lifecycle_meta`, returning `finish_at_unix`.
pub fn retract_mutual_war(snap: &mut WorldStateSnapshot, war_id: i64, now_unix: u64) -> u64 {
    let finish = now_unix + WAR_TIMER_SECS;
    snap.war_state.wars.entry(war_id).or_default().finish_at_unix = Some(finish);
    let lm = snap.war_lifecycle_meta.entry(war_id).or_default();
    lm.retracted_unix = Some(now_unix);
    lm.finished_unix = Some(finish);
    finish
}

fn has_unexpired_accepted_ally(
    snap: &WorldStateSnapshot,
    war_id: i64,
    ally_id: i64,
    now_unix: u64,
    skip_idx: Option<usize>,
) -> bool {
    let ally_unexpired = snap
        .war_state
        .ally_expires_unix
        .get(&(war_id, ally_id))
        .is_none_or(|&exp| exp > now_unix);
    ally_unexpired
        && snap.war_state.negotiations.iter().enumerate().any(|(i, other)| {
            Some(i) != skip_idx
                && other.war_id == war_id
                && other.kind == PersistedNegKind::Ally
                && other.from_corp == ally_id
                && other.status == "Accepted"
        })
}

/// Inserts a new `"Offered"` surrender or ally negotiation in `snap.war_state.negotiations`,
/// enforcing ally offer uniqueness and cancelling superseded pending surrender offers.
pub fn create_negotiation(
    snap: &mut WorldStateSnapshot,
    war_id: i64,
    kind: PersistedNegKind,
    from_corp: i64,
    isk: IskCents,
    message: String,
    now_unix: u64,
) -> Result<i64, DomainError> {
    if kind == PersistedNegKind::Ally {
        let has_offered = snap.war_state.negotiations.iter().any(|neg| {
            neg.war_id == war_id
                && neg.kind == PersistedNegKind::Ally
                && neg.from_corp == from_corp
                && neg.status == "Offered"
        });
        if has_offered || has_unexpired_accepted_ally(snap, war_id, from_corp, now_unix, None) {
            return Err(DomainError::conflict(
                "AllyOfferAlreadyExists",
                "Your corporation already has an active or pending ally offer for this war",
            ));
        }
    } else {
        for neg in &mut snap.war_state.negotiations {
            if neg.war_id == war_id
                && neg.kind == PersistedNegKind::Surrender
                && neg.status == "Offered"
            {
                neg.status = "Cancelled".into();
            }
        }
    }
    let st = &mut snap.war_state;
    st.next_negotiation_id += 1;
    let id = st.next_negotiation_id;
    st.negotiations.push(PersistedWarNegotiation {
        id,
        war_id,
        kind,
        from_corp,
        isk,
        message,
        status: "Offered".into(),
        effective_unix: None,
    });
    Ok(id)
}

fn find_offered_negotiation(
    snap: &WorldStateSnapshot,
    neg_id: i64,
    expected_kind: PersistedNegKind,
) -> Result<(usize, PersistedWarNegotiation), DomainError> {
    let Some((idx, neg)) = snap
        .war_state
        .negotiations
        .iter()
        .enumerate()
        .find(|(_, n)| n.id == neg_id && n.status == "Offered")
    else {
        return Err(DomainError::not_found(
            "NegotiationNotFound",
            format!("No open negotiation {neg_id}"),
        ));
    };
    if neg.kind != expected_kind {
        return Err(DomainError::not_found(
            "NegotiationNotFound",
            format!("Negotiation {neg_id} is not of the requested kind"),
        ));
    }
    Ok((idx, neg.clone()))
}

/// Resolves a pending surrender negotiation (`AcceptSurrender` / `DeclineSurrender`), updating
/// war finish timestamps, peace treaty state, and `war_lifecycle_meta` together.
pub fn resolve_surrender_negotiation(
    snap: &mut WorldStateSnapshot,
    neg_id: i64,
    caller_corp: i64,
    aggressor_corp_id: i64,
    defender_corp_id: i64,
    accepting: bool,
    now_unix: u64,
) -> Result<Option<(u64, u64)>, DomainError> {
    let (neg_idx, neg) = find_offered_negotiation(snap, neg_id, PersistedNegKind::Surrender)?;
    let Some(w_active) = snap.wars.get(&neg.war_id).map(|w| w.active) else {
        return Err(DomainError::not_found(
            "WarNotFound",
            format!("War {} not found", neg.war_id),
        ));
    };
    if !accepting {
        snap.war_state.negotiations[neg_idx].status = "Declined".into();
        return Ok(None);
    }
    if !w_active {
        return Err(DomainError::precondition_failed(
            "WarNotActive",
            format!("War {} has ended", neg.war_id),
        ));
    }
    if snap
        .war_state
        .wars
        .get(&neg.war_id)
        .and_then(|wm| wm.finish_at_unix)
        .is_some()
    {
        return Err(DomainError::conflict(
            "WarAlreadyEnding",
            format!("War {} surrender has already been accepted", neg.war_id),
        ));
    }
    debit_corp(snap, neg.from_corp, neg.isk)?;
    credit_corp(snap, caller_corp, neg.isk);

    let effective = now_unix + WAR_TIMER_SECS;
    let expiry = effective + PEACE_TREATY_SECS;
    let st = &mut snap.war_state;
    st.negotiations[neg_idx].status = "Accepted".into();
    st.negotiations[neg_idx].effective_unix = Some(effective);
    st.wars.entry(neg.war_id).or_default().finish_at_unix = Some(effective);
    for (i, other) in st.negotiations.iter_mut().enumerate() {
        if i != neg_idx
            && other.war_id == neg.war_id
            && other.kind == PersistedNegKind::Surrender
            && other.status == "Offered"
        {
            other.status = "Cancelled".into();
        }
    }
    st.peace_treaties.push(PersistedPeaceTreaty {
        war_id: neg.war_id,
        corp_a: aggressor_corp_id,
        corp_b: defender_corp_id,
        expiry_unix: expiry,
    });
    snap.war_lifecycle_meta
        .entry(neg.war_id)
        .or_default()
        .finished_unix = Some(effective);
    Ok(Some((effective, expiry)))
}

fn ensure_lifecycle_ally(snap: &mut WorldStateSnapshot, war_id: i64, ally_rec: WarAllyRecord) {
    let lm = snap.war_lifecycle_meta.entry(war_id).or_default();
    if !lm.allies.iter().any(|a| a == &ally_rec) {
        lm.allies.push(ally_rec);
    }
}

/// Resolves an `"Offered"` ally negotiation (`AcceptAllyNegotiation`, `DeclineAllyOffer`, or
/// `RetractWarAllyOffer`), syncing `negotiations`, `ally_expires_unix`, and `war_lifecycle_meta`.
pub fn resolve_ally_negotiation(
    snap: &mut WorldStateSnapshot,
    neg_id: i64,
    caller_corp: i64,
    action: AllyNegotiationAction,
    now_unix: u64,
) -> Result<Option<u64>, DomainError> {
    let (neg_idx, neg) = find_offered_negotiation(snap, neg_id, PersistedNegKind::Ally)?;
    let Some(w_active) = snap.wars.get(&neg.war_id).map(|w| w.active) else {
        return Err(DomainError::not_found(
            "WarNotFound",
            format!("War {} not found", neg.war_id),
        ));
    };
    if action != AllyNegotiationAction::Accept {
        snap.war_state.negotiations[neg_idx].status = action.status_str().into();
        return Ok(None);
    }
    if !w_active {
        return Err(DomainError::precondition_failed(
            "WarNotActive",
            format!("War {} has ended", neg.war_id),
        ));
    }
    if has_unexpired_accepted_ally(snap, neg.war_id, neg.from_corp, now_unix, Some(neg_idx)) {
        return Err(DomainError::conflict(
            "AlreadyWarAlly",
            format!(
                "Corporation {} is already an ally in war {}",
                neg.from_corp, neg.war_id
            ),
        ));
    }
    debit_corp(snap, caller_corp, neg.isk)?;
    credit_corp(snap, neg.from_corp, neg.isk);

    let effective = now_unix + WAR_TIMER_SECS;
    let st = &mut snap.war_state;
    st.negotiations[neg_idx].status = "Accepted".into();
    st.negotiations[neg_idx].effective_unix = Some(effective);
    st.ally_expires_unix.remove(&(neg.war_id, neg.from_corp));
    for (i, other) in st.negotiations.iter_mut().enumerate() {
        if i != neg_idx
            && other.war_id == neg.war_id
            && other.kind == PersistedNegKind::Ally
            && other.from_corp == neg.from_corp
        {
            if other.status == "Offered" {
                other.status = "Cancelled".into();
            } else if other.status == "Accepted" {
                other.status = "Expired".into();
            }
        }
    }
    ensure_lifecycle_ally(
        snap,
        neg.war_id,
        WarAllyRecord {
            corporation_id: Some(neg.from_corp),
            alliance_id: None,
        },
    );
    Ok(Some(effective))
}

/// Applies `warRegistry.GMJoinDefender` across `negotiations`, `ally_expires_unix`, `war_state.wars`,
/// and `war_lifecycle_meta`. Returns previous `open_for_allies`.
pub fn gm_join_defender(
    snap: &mut WorldStateSnapshot,
    war_id: i64,
    ally_id: i64,
    ally_rec: WarAllyRecord,
    effective_unix: u64,
) -> bool {
    if let Some(existing) = snap.war_state.negotiations.iter_mut().find(|n| {
        n.war_id == war_id
            && n.kind == PersistedNegKind::Ally
            && n.from_corp == ally_id
            && n.status == "Accepted"
    }) {
        existing.effective_unix = Some(effective_unix);
    } else {
        snap.war_state.next_negotiation_id += 1;
        let neg_id = snap.war_state.next_negotiation_id;
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: neg_id,
            war_id,
            kind: PersistedNegKind::Ally,
            from_corp: ally_id,
            isk: IskCents::ZERO,
            message: "GMJoinDefender".into(),
            status: "Accepted".into(),
            effective_unix: Some(effective_unix),
        });
    }
    snap.war_state.ally_expires_unix.remove(&(war_id, ally_id));
    ensure_lifecycle_ally(snap, war_id, ally_rec);
    set_open_for_allies(snap, war_id, true)
}

/// Applies `warRegistry.GMActivateDefender`, returning `(prev_active, prev_open)`.
pub fn gm_activate_defender(
    snap: &mut WorldStateSnapshot,
    war_id: i64,
    ally_target: Option<(i64, WarAllyRecord)>,
    now_unix: u64,
) -> Result<(bool, bool), DomainError> {
    let Some(prev_active) = snap.wars.get(&war_id).map(|w| w.active) else {
        return Err(DomainError::not_found(
            "WarNotFound",
            format!("War {war_id} not found"),
        ));
    };
    if let Some((aid, ally_rec)) = ally_target {
        if let Some(existing) = snap.war_state.negotiations.iter_mut().find(|n| {
            n.war_id == war_id
                && n.kind == PersistedNegKind::Ally
                && n.from_corp == aid
                && matches!(n.status.as_str(), "Accepted" | "Offered")
        }) {
            existing.status = "Accepted".into();
            existing.effective_unix = Some(now_unix);
        } else {
            snap.war_state.next_negotiation_id += 1;
            let neg_id = snap.war_state.next_negotiation_id;
            snap.war_state.negotiations.push(PersistedWarNegotiation {
                id: neg_id,
                war_id,
                kind: PersistedNegKind::Ally,
                from_corp: aid,
                isk: IskCents::ZERO,
                message: "GMActivateDefender".into(),
                status: "Accepted".into(),
                effective_unix: Some(now_unix),
            });
        }
        if snap
            .war_state
            .ally_expires_unix
            .get(&(war_id, aid))
            .is_some_and(|&exp| exp <= now_unix)
        {
            snap.war_state.ally_expires_unix.remove(&(war_id, aid));
        }
        ensure_lifecycle_ally(snap, war_id, ally_rec);
    } else {
        for neg in &mut snap.war_state.negotiations {
            if neg.war_id == war_id
                && neg.kind == PersistedNegKind::Ally
                && neg.status == "Accepted"
                && neg.effective_unix.is_none_or(|e| e > now_unix)
            {
                neg.effective_unix = Some(now_unix);
            }
        }
        snap.war_state
            .ally_expires_unix
            .retain(|&(wid, _), exp| wid != war_id || *exp > now_unix);
    }
    if let Some(w_mut) = snap.wars.get_mut(&war_id) {
        w_mut.active = true;
    }
    let meta = snap.war_state.wars.entry(war_id).or_default();
    let prev_open = meta.open_for_allies;
    meta.open_for_allies = true;
    let clear_expired = meta.finish_at_unix.is_some_and(|f| f <= now_unix);
    if clear_expired {
        meta.finish_at_unix = None;
    }
    let lm = snap.war_lifecycle_meta.entry(war_id).or_default();
    lm.open_for_allies = Some(true);
    if clear_expired {
        lm.finished_unix = None;
    }
    Ok((prev_active, prev_open))
}

/// Applies `warRegistry.GMDeactivateDefender` across `negotiations`, `ally_expires_unix`,
/// `war_state.wars`, and `war_lifecycle_meta`. Returns `Ok(Some(prev_open))` when deactivating all
/// allies, or `Ok(None)` when deactivating a single `ally_id`.
pub fn gm_deactivate_defender(
    snap: &mut WorldStateSnapshot,
    war_id: i64,
    ally_id: Option<i64>,
) -> Result<Option<bool>, DomainError> {
    if let Some(aid) = ally_id {
        let has_neg = snap.war_state.negotiations.iter().any(|n| {
            n.war_id == war_id
                && n.kind == PersistedNegKind::Ally
                && n.from_corp == aid
                && matches!(n.status.as_str(), "Accepted" | "Offered")
        });
        let has_lm = snap.war_lifecycle_meta.get(&war_id).is_some_and(|lm| {
            lm.allies
                .iter()
                .any(|a| a.corporation_id == Some(aid) || a.alliance_id == Some(aid))
        });
        if !has_neg && !has_lm {
            return Err(DomainError::not_found(
                "AllyNotFound",
                format!("Defender ally {aid} is not active in war {war_id}"),
            ));
        }
        for neg in &mut snap.war_state.negotiations {
            if neg.war_id == war_id
                && neg.kind == PersistedNegKind::Ally
                && neg.from_corp == aid
                && matches!(neg.status.as_str(), "Accepted" | "Offered")
            {
                neg.status = "GMDeactivated".into();
            }
        }
        if let Some(lm) = snap.war_lifecycle_meta.get_mut(&war_id) {
            lm.allies
                .retain(|a| a.corporation_id != Some(aid) && a.alliance_id != Some(aid));
        }
        snap.war_state.ally_expires_unix.remove(&(war_id, aid));
        Ok(None)
    } else {
        for neg in &mut snap.war_state.negotiations {
            if neg.war_id == war_id
                && neg.kind == PersistedNegKind::Ally
                && matches!(neg.status.as_str(), "Accepted" | "Offered")
            {
                neg.status = "GMDeactivated".into();
            }
        }
        if let Some(lm) = snap.war_lifecycle_meta.get_mut(&war_id) {
            lm.allies.clear();
        }
        snap.war_state
            .ally_expires_unix
            .retain(|&(wid, _), _| wid != war_id);
        let prev_open = set_open_for_allies(snap, war_id, false);
        Ok(Some(prev_open))
    }
}

/// Extends or sets the ally contract expiry timestamp (`ally_expires_unix`) for `(war_id, ally_id)`.
pub fn gm_extend_ally_contract(
    snap: &mut WorldStateSnapshot,
    war_id: i64,
    ally_id: i64,
    ts_or_delta: u64,
    now_unix: u64,
) -> Result<u64, DomainError> {
    let Some(effective_unix) = snap
        .war_state
        .negotiations
        .iter()
        .rev()
        .find(|n| {
            n.war_id == war_id
                && n.kind == PersistedNegKind::Ally
                && n.from_corp == ally_id
                && n.status == "Accepted"
        })
        .map(|n| n.effective_unix.unwrap_or(now_unix))
    else {
        return Err(DomainError::not_found(
            "AllyNotFound",
            format!("Accepted ally {ally_id} not found in war {war_id}"),
        ));
    };
    let current_exp = snap
        .war_state
        .ally_expires_unix
        .get(&(war_id, ally_id))
        .copied();
    let updated_exp = if ts_or_delta <= MAX_ALLY_CONTRACT_EXTEND_SECS {
        current_exp
            .unwrap_or(effective_unix)
            .max(now_unix)
            .max(effective_unix)
            .saturating_add(ts_or_delta)
    } else {
        ts_or_delta
    };
    snap.war_state
        .ally_expires_unix
        .insert((war_id, ally_id), updated_exp);
    if updated_exp <= now_unix {
        if let Some(lm) = snap.war_lifecycle_meta.get_mut(&war_id) {
            lm.allies.retain(|a| !ally_record_matches_id(a, ally_id));
        }
    } else {
        let rec = ally_record_for_entity(snap, ally_id);
        ensure_lifecycle_ally(snap, war_id, rec);
    }
    Ok(updated_exp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_db::snapshot_ext::WarAllyRecord;

    #[test]
    fn unix_to_filetime_matches_windows_epoch_ticks() {
        assert_eq!(unix_to_filetime(0), 116_444_736_000_000_000);
        assert_eq!(
            unix_to_filetime(1_760_000_000),
            (1_760_000_000_i64 + 11_644_473_600) * 10_000_000
        );
        assert_eq!(opt_unix_to_filetime(None), None);
        assert_eq!(
            opt_unix_to_filetime(Some(1_760_000_000)),
            Some((1_760_000_000_i64 + 11_644_473_600) * 10_000_000)
        );
    }

    #[test]
    fn active_war_between_and_killmail_auth_honor_ally_expiry_and_store_sync() {
        let mut snap = WorldStateSnapshot::default();
        let agg = 98_110_001_i64;
        let def = 98_110_002_i64;
        let ally = 98_110_003_i64;
        let mut corp = eve_db::CorporationRecord::default();
        corp.ensure_wallet_divisions();
        for cid in [agg, def, ally] {
            let mut c = corp.clone();
            c.corporation_id = cid;
            c.credit_division(1, IskCents::from_isk(500_000_000));
            snap.corporations.insert(cid, c);
        }

        let t0 = 1_760_000_000_u64;
        let war_id = declare_war(
            &mut snap,
            agg,
            def,
            880_001,
            t0,
            IskCents::from_isk(WAR_DECLARATION_FEE_ISK),
        )
        .expect("declare_war");
        assert!(snap.wars.contains_key(&war_id));
        assert_eq!(snap.war_state.wars[&war_id].war_hq_id, Some(880_001));
        assert_eq!(snap.war_lifecycle_meta[&war_id].declared_unix, Some(t0));
        assert_eq!(
            snap.war_lifecycle_meta[&war_id].started_unix,
            Some(t0 + WAR_TIMER_SECS)
        );

        let prev_open = set_open_for_allies(&mut snap, war_id, true);
        assert!(!prev_open);
        assert_eq!(snap.war_lifecycle_meta[&war_id].open_for_allies, Some(true));

        let eff = t0 + WAR_TIMER_SECS;
        let exp = eff + 3_600;
        let _prev = gm_join_defender(
            &mut snap,
            war_id,
            ally,
            WarAllyRecord {
                corporation_id: Some(ally),
                alliance_id: None,
            },
            eff,
        );
        gm_extend_ally_contract(&mut snap, war_id, ally, exp, eff).expect("extend ally");

        // Before 24h spool-up: declared war active for already_at_war, not yet combat active.
        assert!(already_at_war(&snap, agg, def));
        assert!(!active_war_between(&snap, agg, def, eff - 1));
        assert!(active_war_between(&snap, agg, def, eff));

        // Ally active in [eff, exp), excluded before eff and at/after exp across all predicates.
        assert!(!active_war_between(&snap, agg, ally, eff - 1));
        assert!(active_war_between(&snap, agg, ally, eff));
        assert!(active_war_between(&snap, ally, agg, exp - 1));
        assert!(!active_war_between(&snap, agg, ally, exp));
        assert!(!active_war_between(&snap, ally, agg, exp + 60));

        assert!(is_war_interference(&snap, ally, None, agg, None, exp - 1));
        assert!(!is_war_interference(&snap, ally, None, def, None, exp - 1));
        assert!(is_war_interference(&snap, ally, None, def, None, exp));
        assert!(is_war_participant(&snap, war_id, ally, None, exp - 1));
        assert!(!is_war_participant(&snap, war_id, ally, None, exp));
        assert!(active_war_ally_records(&snap, war_id, eff - 1).is_empty());
        assert_eq!(active_war_ally_records(&snap, war_id, exp - 1).len(), 1);
        assert!(active_war_ally_records(&snap, war_id, exp).is_empty());
        assert!(has_expired_lifecycle_allies(&snap, exp));
        prune_expired_lifecycle_allies(&mut snap, exp);
        assert!(snap.war_lifecycle_meta[&war_id].allies.is_empty());

        assert!(is_war_killmail_authorized(
            &snap,
            ally,
            None,
            agg,
            None,
            &[(ally, None)],
            exp - 1
        ));
        assert!(!is_war_killmail_authorized(
            &snap,
            ally,
            None,
            agg,
            None,
            &[(ally, None)],
            exp
        ));

        // Deactivation keeps wars, war_state, and war_lifecycle_meta synchronized.
        deactivate_war(&mut snap, war_id, exp + 100);
        assert!(!snap.wars[&war_id].active);
        assert_eq!(snap.war_state.wars[&war_id].finish_at_unix, Some(exp + 100));
        assert_eq!(
            snap.war_lifecycle_meta[&war_id].finished_unix,
            Some(exp + 100)
        );
        assert!(!active_war_between(&snap, agg, def, exp + 100));
    }

    #[test]
    fn mutual_invite_and_gm_deactivate_keep_all_war_stores_synchronized() {
        let mut snap = WorldStateSnapshot::default();
        let agg = 98_120_001_i64;
        let def = 98_120_002_i64;
        let ally = 98_120_003_i64;
        for cid in [agg, def, ally] {
            let mut c = eve_db::CorporationRecord::default();
            c.corporation_id = cid;
            snap.corporations.insert(cid, c);
        }
        let t0 = 1_800_000_000_u64;
        snap.war_state.mutual_invites.insert((agg, def), t0);
        let MutualWarAcceptOutcome::Started { war_id: mwar_id } =
            accept_mutual_war_invite(&mut snap, agg, def, t0).expect("mutual accept")
        else {
            panic!("expected MutualWarAcceptOutcome::Started");
        };
        assert!(snap.wars[&mwar_id].mutual);
        assert_eq!(snap.war_lifecycle_meta[&mwar_id].declared_unix, Some(t0));
        assert_eq!(snap.war_lifecycle_meta[&mwar_id].started_unix, Some(t0));
        assert!(active_war_between(&snap, agg, def, t0));

        // set_declare_time keeps mutual started_unix equal to declared_unix (vs +24h for non-mutual).
        set_declare_time(&mut snap, mwar_id, t0 - 100).expect("set_declare_time mutual");
        assert_eq!(
            snap.war_lifecycle_meta[&mwar_id].started_unix,
            Some(t0 - 100)
        );
        set_declare_time(&mut snap, mwar_id, t0).expect("restore declared");

        // Stale mutual invite returns Ok(RejectedAlreadyAtWar) and drops the invite cleanly.
        snap.war_state.mutual_invites.insert((agg, def), t0 + 1);
        assert_eq!(
            accept_mutual_war_invite(&mut snap, agg, def, t0 + 1).expect("already at war outcome"),
            MutualWarAcceptOutcome::RejectedAlreadyAtWar
        );
        assert!(!snap.war_state.mutual_invites.contains_key(&(agg, def)));

        // Negotiation lookup validates id and kind cleanly inside domain::wars.
        let surr_neg = create_negotiation(
            &mut snap,
            mwar_id,
            PersistedNegKind::Surrender,
            def,
            IskCents::ZERO,
            "surr".into(),
            t0,
        )
        .expect("create surrender neg");
        assert_eq!(
            resolve_ally_negotiation(
                &mut snap,
                surr_neg,
                agg,
                AllyNegotiationAction::Accept,
                t0
            )
            .unwrap_err()
            .code,
            "NegotiationNotFound"
        );
        snap.war_state
            .negotiations
            .retain(|n| n.id != surr_neg);

        gm_join_defender(
            &mut snap,
            mwar_id,
            ally,
            WarAllyRecord {
                corporation_id: Some(ally),
                alliance_id: None,
            },
            t0,
        );
        assert_eq!(snap.war_lifecycle_meta[&mwar_id].allies.len(), 1);
        assert!(is_war_participant(&snap, mwar_id, ally, None, t0));

        gm_deactivate_defender(&mut snap, mwar_id, Some(ally)).expect("deactivate ally");
        assert!(snap.war_lifecycle_meta[&mwar_id].allies.is_empty());
        assert!(!snap.war_state.ally_expires_unix.contains_key(&(mwar_id, ally)));
        assert!(!is_war_participant(&snap, mwar_id, ally, None, t0));

        let retract_finish = retract_mutual_war(&mut snap, mwar_id, t0);
        assert_eq!(retract_finish, t0 + WAR_TIMER_SECS);
        assert_eq!(snap.war_lifecycle_meta[&mwar_id].retracted_unix, Some(t0));
        assert_eq!(
            snap.war_lifecycle_meta[&mwar_id].finished_unix,
            Some(retract_finish)
        );
        assert_eq!(
            snap.war_state.wars[&mwar_id].finish_at_unix,
            Some(retract_finish)
        );
        assert!(active_war_between(&snap, agg, def, retract_finish - 1));
        assert!(!active_war_between(&snap, agg, def, retract_finish));
    }

    #[test]
    fn resolve_killmail_war_id_handles_alliance_wars_and_ally_windows() {
        let mut snap = WorldStateSnapshot::default();
        let agg_ally = 99_200_001_i64;
        let def_ally = 99_200_002_i64;
        let agg_corp = 98_200_001_i64;
        let def_corp = 98_200_002_i64;
        let ally_alliance = 99_200_003_i64;
        let ally_member_corp = 98_200_003_i64;

        for (cid, aid) in [
            (agg_corp, agg_ally),
            (def_corp, def_ally),
            (ally_member_corp, ally_alliance),
        ] {
            let mut c = eve_db::CorporationRecord::default();
            c.corporation_id = cid;
            c.alliance_id = Some(aid);
            snap.corporations.insert(cid, c);
        }
        let war_id = 707_i64;
        let t0 = 1_760_000_000_u64;
        insert_war_record(&mut snap, war_id, agg_ally, def_ally, false, Some(880_007), t0);
        let eff = t0 + WAR_TIMER_SECS;
        let exp = eff + 1_800;
        gm_join_defender(
            &mut snap,
            war_id,
            ally_alliance,
            WarAllyRecord {
                corporation_id: None,
                alliance_id: Some(ally_alliance),
            },
            eff,
        );
        gm_extend_ally_contract(&mut snap, war_id, ally_alliance, exp, eff).expect("extend ally");

        // Aggressor alliance vs defender alliance (even without explicit hints on killmail).
        assert_eq!(
            resolve_killmail_war_id(&snap, def_corp, None, &[(agg_corp, None)], eff),
            Some(war_id)
        );
        // Active defender alliance ally in [eff, exp) -> matched; before eff or >= exp -> None.
        assert_eq!(
            resolve_killmail_war_id(
                &snap,
                ally_member_corp,
                Some(ally_alliance),
                &[(agg_corp, Some(agg_ally))],
                eff - 1,
            ),
            None
        );
        assert_eq!(
            resolve_killmail_war_id(
                &snap,
                ally_member_corp,
                Some(ally_alliance),
                &[(agg_corp, Some(agg_ally))],
                exp - 1,
            ),
            Some(war_id)
        );
        assert_eq!(
            resolve_killmail_war_id(
                &snap,
                ally_member_corp,
                Some(ally_alliance),
                &[(agg_corp, Some(agg_ally))],
                exp,
            ),
            None
        );
    }
}
