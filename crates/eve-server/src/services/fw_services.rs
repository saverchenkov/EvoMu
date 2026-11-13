//! `fw_services` — Faction Warfare Warzones, Victory Points, Advantage & HQ Systems (`§3.11`).

#![forbid(unsafe_code)]

use crate::router::NotificationTarget;
use crate::router::ServerContext;
pub use eve_db::{
    PersistedBattlefieldRecord as BattlefieldRecord, PersistedFwState as FwRuntimeState,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

/// SDE Solar System IDs for the 4 Empire Militia War HQs:
/// - Mehatoor (30002974) - 24th Imperial Crusade (Amarr)
/// - Onnamon (30045324) - State Protectorate (Caldari)
/// - Intaki (30003788) - Federal Defence Union (Gallente)
/// - Amo (30002055) - Tribal Liberation Force (Minmatar)
pub const HQ_SYSTEM_MEHATOOR: u32 = 30002974;
pub const HQ_SYSTEM_ONNAMON: u32 = 30045324;
pub const HQ_SYSTEM_INTAKI: u32 = 30003788;
pub const HQ_SYSTEM_AMO: u32 = 30002055;

pub const FACTION_CALDARI: i64 = 500001;
pub const FACTION_MINMATAR: i64 = 500002;
pub const FACTION_AMARR: i64 = 500003;
pub const FACTION_GALLENTE: i64 = 500004;

pub const STATUS_FRONTLINE: i64 = 1;
pub const STATUS_COMMAND: i64 = 2;
pub const STATUS_REARGUARD: i64 = 3;

/// V24.01 `fwwarzone/common/const.py` 0-indexed `ADJACENCY_STATES`:
/// `0 = Rearguard`, `1 = Command Operations`, `2 = Frontline`.
pub const ADJACENCY_STATE_REARGUARD: i64 = 0;
pub const ADJACENCY_STATE_COMMAND_OPERATIONS: i64 = 1;
pub const ADJACENCY_STATE_FRONTLINE: i64 = 2;

pub const fn frontline_status_to_adjacency_state(frontline_status: i64) -> i64 {
    match frontline_status {
        STATUS_FRONTLINE => ADJACENCY_STATE_FRONTLINE,
        STATUS_COMMAND => ADJACENCY_STATE_COMMAND_OPERATIONS,
        _ => ADJACENCY_STATE_REARGUARD,
    }
}

/// Returns the opposing empire militia faction ID for the given faction ID, if in a known FW war pair.
pub fn fw_opposing_faction(faction_id: i64) -> Option<i64> {
    match faction_id {
        FACTION_CALDARI => Some(FACTION_GALLENTE),
        FACTION_GALLENTE => Some(FACTION_CALDARI),
        FACTION_MINMATAR => Some(FACTION_AMARR),
        FACTION_AMARR => Some(FACTION_MINMATAR),
        _ => None,
    }
}

/// Returns true if two factions are in an opposing militia relationship (canonical empire war pairs or pirate insurgency pairs).
pub fn is_opposing_militia(f1: i64, f2: i64) -> bool {
    if fw_opposing_faction(f1) == Some(f2) {
        return true;
    }
    // Pirate insurgencies opposing pairs:
    // Guristas (500010) opposes Caldari (500001) and Gallente (500004)
    // Angel Cartel (500011) opposes Minmatar (500002) and Amarr (500003)
    matches!(
        (f1, f2),
        (500010, 500001 | 500004)
            | (500001 | 500004, 500010)
            | (500011, 500002 | 500003)
            | (500002 | 500003, 500011)
    )
}

/// Computes the frontline status (Frontline, Command, or Rearguard) for all warzone solar systems
/// using genuine stargate graph adjacency (`feat:fw.frontlines`, `R-FW-P1-6`).
///
/// Rules (from EVE Online Uprising FW system classification):
/// - Frontline (1): Any warzone system adjacent (via stargates) to an enemy-held warzone system.
/// - Command (2): Any non-frontline warzone system adjacent to a friendly frontline warzone system.
/// - Rearguard (3): All other systems in the warzone.
pub fn compute_all_frontline_statuses(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
) -> HashMap<u32, i64> {
    let mut neighbors: HashMap<u32, Vec<u32>> = HashMap::new();
    for j in ctx.sde.jumps.values() {
        if snap.fw_system_occupations.contains_key(&j.from_solar_system_id) {
            neighbors
                .entry(j.from_solar_system_id)
                .or_default()
                .push(j.to_solar_system_id);
        }
    }

    let mut frontlines = HashSet::new();
    for (&sys_id, occ) in &snap.fw_system_occupations {
        if let Some(enemy_fac) = fw_opposing_faction(occ.occupier_faction_id) {
            if let Some(adj) = neighbors.get(&sys_id) {
                if adj.iter().any(|&nbr| {
                    snap.fw_system_occupations
                        .get(&nbr)
                        .map(|o| o.occupier_faction_id == enemy_fac)
                        .unwrap_or(false)
                }) {
                    frontlines.insert(sys_id);
                }
            }
        }
    }

    let mut statuses = HashMap::with_capacity(snap.fw_system_occupations.len());
    for (&sys_id, occ) in &snap.fw_system_occupations {
        let status = if frontlines.contains(&sys_id) {
            STATUS_FRONTLINE
        } else if let Some(adj) = neighbors.get(&sys_id) {
            let is_command = adj.iter().any(|&nbr| {
                frontlines.contains(&nbr)
                    && snap
                        .fw_system_occupations
                        .get(&nbr)
                        .map(|o| o.occupier_faction_id == occ.occupier_faction_id)
                        .unwrap_or(false)
            });
            if is_command {
                STATUS_COMMAND
            } else {
                STATUS_REARGUARD
            }
        } else {
            STATUS_REARGUARD
        };
        statuses.insert(sys_id, status);
    }

    statuses
}

/// Computes the frontline status for a single solar system using the warzone stargate adjacency graph.
pub fn compute_system_frontline_status(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    sys_id: u32,
) -> i64 {
    let Some(occ) = snap.fw_system_occupations.get(&sys_id) else {
        return STATUS_REARGUARD;
    };
    let my_fac = occ.occupier_faction_id;
    let Some(enemy_fac) = fw_opposing_faction(my_fac) else {
        return STATUS_REARGUARD;
    };

    let direct_neighbors: Vec<u32> = ctx
        .sde
        .jumps
        .values()
        .filter(|j| j.from_solar_system_id == sys_id)
        .map(|j| j.to_solar_system_id)
        .collect();

    for &nbr in &direct_neighbors {
        if let Some(nbr_occ) = snap.fw_system_occupations.get(&nbr) {
            if nbr_occ.occupier_faction_id == enemy_fac {
                return STATUS_FRONTLINE;
            }
        }
    }

    for &nbr in &direct_neighbors {
        if let Some(nbr_occ) = snap.fw_system_occupations.get(&nbr) {
            if nbr_occ.occupier_faction_id == my_fac {
                let nbr_has_enemy = ctx
                    .sde
                    .jumps
                    .values()
                    .filter(|j| j.from_solar_system_id == nbr)
                    .any(|j| {
                        snap.fw_system_occupations
                            .get(&j.to_solar_system_id)
                            .map(|o| o.occupier_faction_id == enemy_fac)
                            .unwrap_or(false)
                    });
                if nbr_has_enemy {
                    return STATUS_COMMAND;
                }
            }
        }
    }

    STATUS_REARGUARD
}

pub fn with_fw_runtime<R>(ctx: &ServerContext, f: impl FnOnce(&mut FwRuntimeState) -> R) -> R {
    ctx.db.with_mut_snapshot(|snap| f(&mut snap.fw_runtime))
}

pub fn read_fw_runtime<R>(ctx: &ServerContext, f: impl FnOnce(&FwRuntimeState) -> R) -> R {
    ctx.db.with_snapshot(|snap| f(&snap.fw_runtime))
}

/// Derives character Faction Warfare militia rank from unmodified faction standing (`feat:fw.fw-ranks`).
/// Ranks 1-9 each require +0.75 standing (1: 0.75, 2: 1.50, ..., 9: 6.75+). 0 standing is unranked (0).
pub fn calculate_militia_rank(standing: f64) -> i64 {
    if standing < 0.75 {
        0
    } else {
        ((standing / 0.75).floor() as i64).clamp(1, 9)
    }
}

/// Updates advantage in a warzone system and scatters advantage update notifications (`feat:fw.advantage`).
pub fn update_system_advantage(ctx: &ServerContext, sys_id: u32, friendly_delta: f64, enemy_delta: f64) -> (f64, f64) {
    let (friendly, enemy) = with_fw_runtime(ctx, |rt| {
        let entry = rt.system_advantage.entry(sys_id).or_insert((0.0, 0.0));
        entry.0 = (entry.0 + friendly_delta).clamp(0.0, 100.0);
        entry.1 = (entry.1 + enemy_delta).clamp(0.0, 100.0);
        *entry
    });
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSolarsystemAdvantageStateUpdated_Local",
        "solarsystemid",
        vec![],
    );
    (friendly, enemy)
}

/// Adds victory points to a system from complex captures or enemy kills.
pub fn add_victory_points(ctx: &ServerContext, sys_id: u32, vp: i64) -> (i64, f64) {
    let res = ctx.db.with_mut_snapshot(|snap| {
        let occ = snap.fw_system_occupations.get_mut(&sys_id)?;
        let threshold_f = (occ.victory_points_threshold as f64).max(1.0);
        let threshold_i = occ.victory_points_threshold;
        let owner_fac = occ.owner_faction_id;
        let occupier_fac = occ.occupier_faction_id;
        let entry = snap
            .fw_runtime
            .system_victory_points
            .entry(sys_id)
            .or_insert(occ.victory_points);
        *entry = entry.saturating_add(vp).max(0);
        let new_vp = *entry;
        occ.victory_points = new_vp;
        let contested = ((new_vp as f64 / threshold_f) * 100.0).clamp(0.0, 100.0);
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.fw_contested_pct = contested;
        }
        Some((new_vp, contested, threshold_i, owner_fac, occupier_fac))
    });
    let Some((new_vp, contested, threshold_i, owner_fac, occupier_fac)) = res else {
        return (0, 0.0);
    };

    if contested >= 100.0 {
        set_facwar_bunker_vulnerable(ctx, sys_id, occupier_fac, true);
    }

    crate::services::notify_emitters_fleet_combat::emit_victory_point_state_updated(
        ctx,
        sys_id,
        new_vp,
        contested,
        threshold_i,
    );
    let front = ctx.db.with_snapshot(|snap| {
        compute_system_frontline_status(ctx, snap, sys_id)
    });
    let occ_state = PyRep::keyval([
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("ownerID", PyRep::Integer(owner_fac)),
        ("ownerFactionID", PyRep::Integer(owner_fac)),
        ("occupierID", PyRep::Integer(occupier_fac)),
        ("occupierFactionID", PyRep::Integer(occupier_fac)),
        (
            "adjacencyState",
            PyRep::Integer(frontline_status_to_adjacency_state(front)),
        ),
        ("frontlineStatus", PyRep::Integer(front)),
        ("contestedPercentage", PyRep::Real(contested)),
    ]);
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnWarzoneOccupationStateUpdated",
        "solarsystemid",
        vec![PyRep::Integer(i64::from(sys_id)), occ_state],
    );
    (new_vp, contested)
}

/// Captures an FW solar system when it is 100% contested and vulnerable (`feat:fw.system-capture`).
/// Transfers occupation to `new_occupier_faction_id`, resets contested % / victory points to 0,
/// marks the bunker non-vulnerable, and updates frontline / command / rearguard statuses.
pub fn capture_warzone_system(
    ctx: &ServerContext,
    sys_id: u32,
    new_occupier_faction_id: i64,
) -> Result<(i64, i64, i64), BridgeRpcError> {
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let occ = snap.fw_system_occupations.get_mut(&sys_id).ok_or_else(|| {
            BridgeRpcError {
                code: "SystemNotWarzone".into(),
                message: format!("System {sys_id} is not an FW warzone system"),
                client_error: None,
            }
        })?;
        let current_c = snap.system_states.get(&sys_id).map(|s| s.fw_contested_pct).unwrap_or_else(|| {
            let threshold = (occ.victory_points_threshold as f64).max(1.0);
            let vp = snap.fw_runtime.system_victory_points.get(&sys_id).copied().unwrap_or(occ.victory_points);
            ((vp as f64 / threshold) * 100.0).clamp(0.0, 100.0)
        });
        if current_c < 100.0 {
            return Err(BridgeRpcError {
                code: "SystemNotVulnerable".into(),
                message: format!("System {sys_id} is only {current_c:.1}% contested; must be 100% to capture"),
                client_error: None,
            });
        }
        let old_occupier = occ.occupier_faction_id;
        occ.occupier_faction_id = new_occupier_faction_id;
        occ.victory_points = 0;
        let owner = occ.owner_faction_id;
        let threshold_i = occ.victory_points_threshold;
        snap.fw_runtime.system_victory_points.insert(sys_id, 0);
        snap.fw_runtime.system_advantage.insert(sys_id, (0.0, 0.0));
        snap.fw_runtime.ihub_tiers.insert(sys_id, 0);
        snap.fw_runtime.ihub_lp_donations.insert(sys_id, 0);
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.fw_contested_pct = 0.0;
        }
        Ok((old_occupier, new_occupier_faction_id, owner, threshold_i))
    })?;

    set_facwar_bunker_vulnerable(ctx, sys_id, 0, false);

    let front = ctx.db.with_snapshot(|snap| {
        compute_system_frontline_status(ctx, snap, sys_id)
    });
    let occ_state = PyRep::keyval([
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("ownerID", PyRep::Integer(res.2)),
        ("ownerFactionID", PyRep::Integer(res.2)),
        ("occupierID", PyRep::Integer(new_occupier_faction_id)),
        ("occupierFactionID", PyRep::Integer(new_occupier_faction_id)),
        (
            "adjacencyState",
            PyRep::Integer(frontline_status_to_adjacency_state(front)),
        ),
        ("frontlineStatus", PyRep::Integer(front)),
        ("contestedPercentage", PyRep::Real(0.0)),
    ]);
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnWarzoneOccupationStateUpdated",
        "solarsystemid",
        vec![PyRep::Integer(i64::from(sys_id)), occ_state],
    );
    let vp_state = PyRep::keyval([
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("victoryPoints", PyRep::Integer(0)),
        ("contestedPercentage", PyRep::Real(0.0)),
        ("threshold", PyRep::Integer(res.3)),
    ]);
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnVictoryPointStateUpdated",
        "solarsystemid",
        vec![PyRep::Integer(i64::from(sys_id)), vp_state],
    );

    Ok((res.0, res.1, res.2))
}

/// Executes queued FW system captures whose Infrastructure Hubs were destroyed before daily downtime.
pub fn process_pending_fw_captures_on_downtime(ctx: &ServerContext) -> usize {
    let pending: Vec<(u32, i64)> = ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.pending_downtime_captures.drain().collect()
    });
    let mut captured = 0usize;
    for (sys_id, new_fac) in pending {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(o) = snap.fw_system_occupations.get_mut(&sys_id) {
                o.victory_points = o.victory_points_threshold;
                snap.fw_runtime
                    .system_victory_points
                    .insert(sys_id, o.victory_points_threshold);
            }
            if let Some(ss) = snap.system_states.get_mut(&sys_id) {
                ss.fw_contested_pct = 100.0;
            }
        });
        if capture_warzone_system(ctx, sys_id, new_fac).is_ok() {
            captured += 1;
            let new_corp = crate::services::fw_gameplay::militia_lp_corp_id(&ctx.sde, new_fac);
            ctx.db.with_mut_snapshot(|snap| {
                snap.fw_runtime.ihub_vulnerable.insert(sys_id, false);
                snap.fw_runtime.ihub_tiers.insert(sys_id, 0);
                snap.fw_runtime.ihub_lp_donations.insert(sys_id, 0);
                snap.insurgency_runtime
                    .surrender_ihub_vulnerable_unix
                    .remove(&sys_id);
                for st in snap.structures.values_mut() {
                    if st.solar_system_id == sys_id && st.type_id == crate::services::fw_gameplay::TYPE_FW_IHUB {
                        st.owner_corp_id = new_corp;
                        st.shield_hp = 5_000.0;
                        st.armor_hp = 5_000.0;
                        st.hull_hp = 8_000.0;
                        st.state = "ShieldVulnerable".into();
                    }
                }
            });
        }
    }
    captured
}

/// Transitions a Faction Warfare bunker's vulnerability state and emits `OnFacWarBunkerVulnerable` or `OnFacWarBunkerNonVulnerable`.
pub fn set_facwar_bunker_vulnerable(ctx: &ServerContext, sys_id: u32, attacker_id: i64, vulnerable: bool) {
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.ihub_vulnerable.insert(sys_id, vulnerable);
        if vulnerable {
            snap.fw_runtime.ihub_vulnerable_since_unix.insert(sys_id, now);
        } else {
            snap.fw_runtime.ihub_vulnerable_since_unix.remove(&sys_id);
            snap.fw_runtime.last_ihub_attacked_unix.remove(&sys_id);
        }
    });
    let event = if vulnerable {
        "OnFacWarBunkerVulnerable"
    } else {
        "OnFacWarBunkerNonVulnerable"
    };
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        event,
        "solarsystemid",
        vec![PyRep::Integer(attacker_id)],
    );
}

/// Completes a Faction Warfare dungeon, objective, and scoreboard in `sys_id`.
pub fn complete_fw_dungeon_and_objective(
    ctx: &ServerContext,
    sys_id: u32,
    faction_id: i64,
    was_conquered: bool,
    objective_mode: i64,
    scoreboard_item_id: i64,
) {
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnFacWarDungeonCompleted",
        "solarsystemid",
        vec![PyRep::Bool(was_conquered), PyRep::Integer(faction_id)],
    );
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnFwObjectiveCompletedInSolarSystem",
        "solarsystemid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Integer(faction_id),
            PyRep::Integer(objective_mode),
        ],
    );
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnFwScoreboardWon",
        "solarsystemid",
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Integer(scoreboard_item_id),
            PyRep::Integer(faction_id),
        ],
    );
}

/// Records an LP donation to a warzone solar system hub and emits `OnSolarSystemLPChange`.
pub fn donate_solar_system_lp(ctx: &ServerContext, sys_id: u32, old_points: i64, new_points: i64) {
    crate::services::notify_emitters_fleet_combat::emit_solar_system_lp_change(
        ctx,
        sys_id,
        old_points,
        new_points,
    );
}

/// Scales capture speed by advantage.
pub fn calculate_capture_speed_multiplier(friendly_adv: f64, enemy_adv: f64) -> f64 {
    (1.0 + (friendly_adv - enemy_adv) * 0.005).clamp(0.5, 2.0)
}

/// Adds a Battlefield site instance to a warzone system.
pub fn spawn_battlefield_site(ctx: &ServerContext, sys_id: u32, faction_id: i64, name: &str) -> i64 {
    with_fw_runtime(ctx, |rt| {
        let id = (rt.battlefield_instances.len() as i64) + 700_001;
        rt.battlefield_instances.push(BattlefieldRecord {
            instance_id: id,
            solar_system_id: sys_id,
            faction_id,
            name: name.to_string(),
            capture_progress: 0.0,
            completed: false,
        });
        id
    })
}

/// Scales FW LP reward based on solar system frontline status (`feat:fw.fw-lp`).
/// Per EVE Uprising: Frontline grants +50% LP (1.5x), Command Operations pays 100% base (1.0x),
/// and Rearguard applies -99% LP (0.01x).
pub fn calculate_fw_lp_reward(base_lp: i64, status: i64) -> i64 {
    let mult = match status {
        STATUS_FRONTLINE => 1.5,
        STATUS_COMMAND => 1.0,
        STATUS_REARGUARD => 0.01,
        _ => 1.0,
    };
    ((base_lp as f64) * mult).round() as i64
}

/// Prunes FW stat events older than 7 days from `stat_events` and folds their kills & VPs
/// into per-entity cumulative historical totals (`historical_*_totals`).
pub fn prune_and_fold_fw_stat_events_in_snap(snap: &mut eve_db::WorldStateSnapshot, now_unix: u64) {
    let mut i = 0;
    while i < snap.fw_runtime.stat_events.len() {
        let age = now_unix.saturating_sub(snap.fw_runtime.stat_events[i].timestamp_unix);
        if age > 7 * 86_400 {
            let ev = snap.fw_runtime.stat_events.swap_remove(i);
            let ce = snap.fw_runtime.historical_char_totals.entry(ev.character_id).or_default();
            ce.0 += ev.kills;
            ce.1 += ev.victory_points;
            if ev.corporation_id > 0 {
                let co = snap.fw_runtime.historical_corp_totals.entry(ev.corporation_id).or_default();
                co.0 += ev.kills;
                co.1 += ev.victory_points;
            }
            if ev.faction_id > 0 {
                let fa = snap.fw_runtime.historical_fac_totals.entry(ev.faction_id).or_default();
                fa.0 += ev.kills;
                fa.1 += ev.victory_points;
            }
            if let Some(aid) = ev.alliance_id.filter(|&a| a > 0) {
                let al = snap.fw_runtime.historical_ally_totals.entry(aid).or_default();
                al.0 += ev.kills;
                al.1 += ev.victory_points;
            }
        } else {
            i += 1;
        }
    }
}

/// Records a timestamped FW kill/VP event and updates rolling `fw_character_stats` windows.
pub fn record_fw_stat_event_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    now_unix: u64,
    char_id: i64,
    faction_opt: Option<i64>,
    kills: i64,
    victory_points: i64,
) {
    prune_and_fold_fw_stat_events_in_snap(snap, now_unix);
    let (corp_id, alliance_id, char_fac) = snap
        .characters
        .get(&char_id)
        .map(|c| {
            let ally = c
                .alliance_id
                .or_else(|| snap.corporations.get(&c.corporation_id).and_then(|co| co.alliance_id));
            (c.corporation_id, ally, c.war_faction_id)
        })
        .unwrap_or((0, None, None));
    let fid_opt = faction_opt.or(char_fac);
    if let Some(fid) = fid_opt {
        if kills > 0 || victory_points > 0 {
            snap.fw_runtime.stat_events.push(eve_db::PersistedFwStatEvent {
                timestamp_unix: now_unix,
                character_id: char_id,
                corporation_id: corp_id,
                alliance_id,
                faction_id: fid,
                kills,
                victory_points,
            });
        }
    }

    let rec = snap.fw_character_stats.entry(char_id).or_insert_with(|| {
        eve_db::snapshot_ext::FwCharacterStatsRecord {
            character_id: char_id,
            faction_id: fid_opt,
            enlisted_on_unix: fid_opt.map(|_| now_unix),
            current_rank: 1,
            highest_rank: 1,
            kills: Default::default(),
            victory_points: Default::default(),
        }
    });
    if fid_opt.is_some() {
        rec.faction_id = fid_opt;
    }
    let (hist_k, hist_v) = snap.fw_runtime.historical_char_totals.get(&char_id).copied().unwrap_or((0, 0));
    let (mut ky, mut klw, mut kt, mut vy, mut vlw, mut vt) = (0, 0, hist_k, 0, 0, hist_v);
    for ev in snap.fw_runtime.stat_events.iter().filter(|e| e.character_id == char_id) {
        let age = now_unix.saturating_sub(ev.timestamp_unix);
        kt += ev.kills;
        vt += ev.victory_points;
        if age < 7 * 86_400 {
            klw += ev.kills;
            vlw += ev.victory_points;
        }
        if age < 86_400 {
            ky += ev.kills;
            vy += ev.victory_points;
        }
    }
    rec.kills.yesterday = ky;
    rec.kills.last_week = klw;
    rec.kills.total = kt;
    rec.victory_points.yesterday = vy;
    rec.victory_points.last_week = vlw;
    rec.victory_points.total = vt;
}

/// Computes raw FW PvP kill LP before frontline status scaling (`3.25 * net_isk / 10_000`, clamped `0..=50_000`).
pub fn compute_fw_kill_base_lp(net_loss_isk: f64) -> i64 {
    (3.25 * net_loss_isk.max(0.0) / 10_000.0)
        .clamp(0.0, 50_000.0)
        .round() as i64
}

/// Resolves a character's effective militia faction (direct enlistment, character record, or corporation militia).
pub fn effective_char_militia_in_snap(snap: &eve_db::WorldStateSnapshot, char_id: i64) -> Option<i64> {
    snap.fw_runtime
        .direct_enlisted_chars
        .get(&char_id)
        .copied()
        .or_else(|| snap.characters.get(&char_id).and_then(|c| c.war_faction_id))
        .or_else(|| {
            let cid = snap.characters.get(&char_id).map(|c| c.corporation_id)?;
            snap.fw_runtime.corp_war_factions.get(&cid).copied()
        })
        .filter(|&f| f > 0)
}

/// Awards FW LP for an enemy kill in a warzone solar system, scaled by system status (`feat:fw.fw-lp`).
/// Per EVE FW rules, kill LP uses `3.25 * (hull - insurance + fit + cargo) / 10_000` (floored at 0,
/// capped at 50,000 before suppression bonuses) and does NOT advance solar system contested VP.
pub fn award_fw_kill_lp(
    ctx: &ServerContext,
    killer_char_id: i64,
    sys_id: u32,
    victim_net_value_isk: f64,
) -> i64 {
    let (status, killer_fac_opt) = ctx.db.with_snapshot(|snap| {
        (
            compute_system_frontline_status(ctx, snap, sys_id),
            effective_char_militia_in_snap(snap, killer_char_id),
        )
    });
    let base_lp = compute_fw_kill_base_lp(victim_net_value_isk);
    let status_lp = calculate_fw_lp_reward(base_lp, status).clamp(0, 50_000);
    let supp_lp_mult = killer_fac_opt
        .map(|fid| crate::services::security_mgr::insurgency_suppression_empire_lp_multiplier(ctx, sys_id, fid))
        .unwrap_or(1.0);
    let final_lp = ((status_lp as f64) * supp_lp_mult).round() as i64;

    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let fid_opt = effective_char_militia_in_snap(snap, killer_char_id);
        if let (Some(fid), Some(ch)) = (fid_opt, snap.characters.get_mut(&killer_char_id)) {
            if final_lp > 0 {
                let militia_corp = ctx
                    .sde
                    .factions
                    .get(&(fid as u32))
                    .and_then(|f| f.militia_corporation_id)
                    .map(|c| c as i64)
                    .unwrap_or(fid);
                *ch.loyalty_points.entry(militia_corp).or_insert(0) += final_lp;
            }
        }
        if victim_net_value_isk > 0.0 {
            record_fw_stat_event_in_snap(snap, now_u, killer_char_id, fid_opt, 1, 0);
        }
    });

    final_lp
}

/// Splits FW PvP kill LP equally across all opposing-militia attackers on a warzone killmail (`feat:fw.fw-lp`).
/// Empty Capsules (`type_id == 670`) and zero-net-loss hulls pay `0 LP`, and PvP kills never push system VP.
pub fn award_fw_kill_lp_split(
    ctx: &ServerContext,
    attacker_char_ids: &[i64],
    victim_char_id: i64,
    sys_id: u32,
    victim_ship_type_id: u32,
    net_loss_isk: f64,
) -> i64 {
    if victim_ship_type_id == 670 {
        return 0;
    }
    let (status, eligible) = ctx.db.with_snapshot(|snap| {
        if !snap.fw_system_occupations.contains_key(&sys_id) {
            return (STATUS_COMMAND, Vec::new());
        }
        let Some(v_fac) = effective_char_militia_in_snap(snap, victim_char_id) else {
            return (STATUS_COMMAND, Vec::new());
        };
        let mut seen = Vec::new();
        for &aid in attacker_char_ids {
            if aid > 0 && aid != victim_char_id && !seen.iter().any(|&(id, _)| id == aid) {
                if let Some(a_fac) = effective_char_militia_in_snap(snap, aid) {
                    if is_opposing_militia(a_fac, v_fac) {
                        seen.push((aid, a_fac));
                    }
                }
            }
        }
        (compute_system_frontline_status(ctx, snap, sys_id), seen)
    });
    if eligible.is_empty() {
        return 0;
    }
    let base_lp = compute_fw_kill_base_lp(net_loss_isk);
    let status_lp = calculate_fw_lp_reward(base_lp, status).clamp(0, 50_000);
    let count = eligible.len() as f64;
    let now_u = ctx.now_unix();
    let mut total_paid = 0i64;
    let shares: Vec<(i64, i64, i64)> = eligible
        .into_iter()
        .map(|(aid, afac)| {
            let supp = crate::services::security_mgr::insurgency_suppression_empire_lp_multiplier(ctx, sys_id, afac);
            let share = (((status_lp as f64) * supp) / count).round() as i64;
            (aid, afac, share)
        })
        .collect();
    ctx.db.with_mut_snapshot(|snap| {
        for (aid, afac, share) in shares {
            if share > 0 {
                if let Some(ch) = snap.characters.get_mut(&aid) {
                    let militia_corp = ctx
                        .sde
                        .factions
                        .get(&(afac as u32))
                        .and_then(|f| f.militia_corporation_id)
                        .map(|c| c as i64)
                        .unwrap_or(afac);
                    *ch.loyalty_points.entry(militia_corp).or_insert(0) += share;
                    total_paid += share;
                }
            }
            record_fw_stat_event_in_snap(snap, now_u, aid, Some(afac), 1, 0);
        }
    });
    total_paid
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("fwWarzoneSolarsystem", "GetHQSystemIDs") => handle_get_hq_system_ids(ctx),

        ("fwWarzoneSolarsystem", "GetAllWarzones") => handle_get_all_warzones(ctx),

        ("fwWarzoneSolarsystem", "GetAllWarzonesOccupationStates")
        | ("fwWarzoneSolarsystem", "GetAllWarzonesOccupationStatesUncached") => {
            ctx.db.with_mut_snapshot(|snap| {
                if snap.fw_system_occupations.is_empty() {
                    snap.backfill_fw_system_occupations();
                }
            });
            let states_by_warzone = ctx.db.with_snapshot(|snap| {
                let frontlines = compute_all_frontline_statuses(ctx, snap);
                let mut wz1: Vec<(u32, PyRep)> = Vec::new();
                let mut wz2: Vec<(u32, PyRep)> = Vec::new();
                for (&sys_id, occ) in &snap.fw_system_occupations {
                    let c = snap
                        .system_states
                        .get(&sys_id)
                        .map(|s| s.fw_contested_pct)
                        .unwrap_or_else(|| {
                            let threshold = (occ.victory_points_threshold as f64).max(1.0);
                            let vp = snap
                                .fw_runtime
                                .system_victory_points
                                .get(&sys_id)
                                .copied()
                                .unwrap_or(occ.victory_points);
                            ((vp as f64 / threshold) * 100.0).clamp(0.0, 100.0)
                        });
                    let front = frontlines.get(&sys_id).copied().unwrap_or(STATUS_REARGUARD);
                    let adj_state = frontline_status_to_adjacency_state(front);
                    let kv = PyRep::keyval([
                        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                        ("ownerID", PyRep::Integer(occ.owner_faction_id)),
                        ("ownerFactionID", PyRep::Integer(occ.owner_faction_id)),
                        ("occupierID", PyRep::Integer(occ.occupier_faction_id)),
                        ("occupierFactionID", PyRep::Integer(occ.occupier_faction_id)),
                        ("adjacencyState", PyRep::Integer(adj_state)),
                        ("frontlineStatus", PyRep::Integer(front)),
                        ("contestedPercentage", PyRep::Real(c)),
                        ("dateTaken", PyRep::Integer(0)),
                    ]);
                    if matches!(occ.owner_faction_id, FACTION_AMARR | FACTION_MINMATAR) {
                        wz2.push((sys_id, kv));
                    } else {
                        wz1.push((sys_id, kv));
                    }
                }
                wz1.sort_by_key(|(sid, _)| *sid);
                wz2.sort_by_key(|(sid, _)| *sid);
                PyRep::Dict(vec![
                    (
                        PyRep::Integer(1),
                        PyRep::Dict(
                            wz1.into_iter()
                                .map(|(sid, kv)| (PyRep::Integer(i64::from(sid)), kv))
                                .collect(),
                        ),
                    ),
                    (
                        PyRep::Integer(2),
                        PyRep::Dict(
                            wz2.into_iter()
                                .map(|(sid, kv)| (PyRep::Integer(i64::from(sid)), kv))
                                .collect(),
                        ),
                    ),
                ])
            });
            Some(Ok(states_by_warzone))
        }

        ("fwWarzoneSolarsystem", "GetLocalOccupationState") => {
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                    client_error: None,
                }));
            };
            let occ_state = ctx.db.with_snapshot(|snap| {
                let occ = snap.fw_system_occupations.get(&sys_id)?;
                let c = snap
                    .system_states
                    .get(&sys_id)
                    .map(|s| s.fw_contested_pct)
                    .unwrap_or_else(|| {
                        let threshold = (occ.victory_points_threshold as f64).max(1.0);
                        let vp = snap
                            .fw_runtime
                            .system_victory_points
                            .get(&sys_id)
                            .copied()
                            .unwrap_or(occ.victory_points);
                        ((vp as f64 / threshold) * 100.0).clamp(0.0, 100.0)
                    });
                let front = compute_system_frontline_status(ctx, snap, sys_id);
                let adj_state = frontline_status_to_adjacency_state(front);
                Some(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                    ("occupierID", PyRep::Integer(occ.occupier_faction_id)),
                    ("occupierFactionID", PyRep::Integer(occ.occupier_faction_id)),
                    ("ownerID", PyRep::Integer(occ.owner_faction_id)),
                    ("ownerFactionID", PyRep::Integer(occ.owner_faction_id)),
                    ("adjacencyState", PyRep::Integer(adj_state)),
                    ("frontlineStatus", PyRep::Integer(front)),
                    ("contestedPercentage", PyRep::Real(c)),
                    ("dateTaken", PyRep::Integer(0)),
                ]))
            }).unwrap_or(PyRep::None);
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Integer(i64::from(sys_id)),
                occ_state,
            ])))
        }

        ("fwVictoryPointMgr", "GetLocalVictoryPointState") => {
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                    client_error: None,
                }));
            };
            let opt_state = ctx.db.with_snapshot(|snap| {
                let occ = snap.fw_system_occupations.get(&sys_id)?;
                let threshold = occ.victory_points_threshold;
                let vp = snap
                    .fw_runtime
                    .system_victory_points
                    .get(&sys_id)
                    .copied()
                    .unwrap_or(occ.victory_points);
                let c = snap
                    .system_states
                    .get(&sys_id)
                    .map(|s| s.fw_contested_pct)
                    .unwrap_or_else(|| {
                        ((vp as f64 / (threshold as f64).max(1.0)) * 100.0).clamp(0.0, 100.0)
                    });
                Some((vp, c, threshold))
            });
            let Some((vp, contested, threshold)) = opt_state else {
                return Some(Ok(PyRep::None));
            };
            let contested_fraction = (contested / 100.0).clamp(0.0, 1.0);
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                ("victoryPoints", PyRep::Integer(vp)),
                ("score", PyRep::Integer(vp)),
                ("contestedPercentage", PyRep::Real(contested)),
                ("contestedFraction", PyRep::Real(contested_fraction)),
                ("threshold", PyRep::Integer(threshold)),
                ("isContested", PyRep::Bool(vp > 0)),
                ("isVulnerable", PyRep::Bool(vp >= threshold)),
            ])))
        }

        ("fwVictoryPointMgr", "GetAllVictoryPointStates") => {
            let res = ctx.db.with_snapshot(|snap| {
                let mut map = Vec::new();
                for (&sys_id, occ) in &snap.fw_system_occupations {
                    let threshold = occ.victory_points_threshold;
                    let vp = snap
                        .fw_runtime
                        .system_victory_points
                        .get(&sys_id)
                        .copied()
                        .unwrap_or(occ.victory_points);
                    let c = snap
                        .system_states
                        .get(&sys_id)
                        .map(|s| s.fw_contested_pct)
                        .unwrap_or_else(|| {
                            ((vp as f64 / (threshold as f64).max(1.0)) * 100.0).clamp(0.0, 100.0)
                        });
                    let contested_fraction = (c / 100.0).clamp(0.0, 1.0);
                    map.push((
                        PyRep::Integer(i64::from(sys_id)),
                        PyRep::keyval([
                            ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                            ("victoryPoints", PyRep::Integer(vp)),
                            ("score", PyRep::Integer(vp)),
                            ("contestedPercentage", PyRep::Real(c)),
                            ("contestedFraction", PyRep::Real(contested_fraction)),
                            ("threshold", PyRep::Integer(threshold)),
                            ("isContested", PyRep::Bool(vp > 0)),
                            ("isVulnerable", PyRep::Bool(vp >= threshold)),
                        ]),
                    ));
                }
                PyRep::Dict(map)
            });
            Some(Ok(res))
        }

        ("dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances") => {
            handle_get_fw_battlefield_instances(ctx, session, args)
        }

        _ => None,
    }
}

fn handle_get_hq_system_ids(ctx: &ServerContext) -> Option<Result<PyRep, BridgeRpcError>> {
    let hqs = [
        (FACTION_CALDARI, HQ_SYSTEM_ONNAMON),
        (FACTION_MINMATAR, HQ_SYSTEM_AMO),
        (FACTION_AMARR, HQ_SYSTEM_MEHATOOR),
        (FACTION_GALLENTE, HQ_SYSTEM_INTAKI),
    ]
    .into_iter()
    .filter(|(_, sys_id)| {
        ctx.sde.solar_systems.is_empty() || ctx.sde.solar_systems.contains_key(sys_id)
    })
    .map(|(faction_id, sys_id)| (PyRep::Integer(faction_id), PyRep::Integer(i64::from(sys_id))))
    .collect();
    Some(Ok(PyRep::Dict(hqs)))
}

fn handle_get_all_warzones(_ctx: &ServerContext) -> Option<Result<PyRep, BridgeRpcError>> {
    Some(Ok(PyRep::Dict(vec![
        (
            PyRep::Integer(1),
            PyRep::List(vec![
                PyRep::Integer(FACTION_CALDARI),
                PyRep::Integer(FACTION_GALLENTE),
            ]),
        ),
        (
            PyRep::Integer(2),
            PyRep::List(vec![
                PyRep::Integer(FACTION_AMARR),
                PyRep::Integer(FACTION_MINMATAR),
            ]),
        ),
    ])))
}

fn handle_get_fw_battlefield_instances(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let sys_id = args
        .first()
        .and_then(|a| a.as_i64())
        .and_then(|v| u32::try_from(v).ok())
        .or(session.solar_system_id);
    let instances = read_fw_runtime(ctx, |rt| {
        rt.battlefield_instances
            .iter()
            .filter(|b| !b.completed && sys_id.map(|s| b.solar_system_id == s).unwrap_or(true))
            .map(|b| {
                PyRep::keyval([
                    ("instanceID", PyRep::Integer(b.instance_id)),
                    ("solarSystemID", PyRep::Integer(i64::from(b.solar_system_id))),
                    ("factionID", PyRep::Integer(b.faction_id)),
                    ("name", PyRep::String(b.name.clone())),
                    ("captureProgress", PyRep::Real(b.capture_progress)),
                    ("completed", PyRep::Bool(b.completed)),
                ])
            })
            .collect::<Vec<PyRep>>()
    });
    Some(Ok(PyRep::List(instances)))
}

#[cfg(test)] #[path = "fw_services_tests.rs"] mod fw_services_tests;

