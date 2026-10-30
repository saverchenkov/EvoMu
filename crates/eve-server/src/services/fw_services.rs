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
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Real(friendly),
            PyRep::Real(enemy),
        ],
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

    let vp_state = PyRep::keyval([
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("victoryPoints", PyRep::Integer(new_vp)),
        ("contestedPercentage", PyRep::Real(contested)),
        ("threshold", PyRep::Integer(threshold_i)),
    ]);
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnVictoryPointStateUpdated",
        "solarsystemid",
        vec![PyRep::Integer(i64::from(sys_id)), vp_state],
    );
    let front = ctx.db.with_snapshot(|snap| {
        compute_system_frontline_status(ctx, snap, sys_id)
    });
    let occ_state = PyRep::keyval([
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("ownerFactionID", PyRep::Integer(owner_fac)),
        ("occupierFactionID", PyRep::Integer(occupier_fac)),
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
        ("ownerFactionID", PyRep::Integer(res.2)),
        ("occupierFactionID", PyRep::Integer(new_occupier_faction_id)),
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
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSolarSystemLPChange",
        "solarsystemid",
        vec![PyRep::Integer(old_points), PyRep::Integer(new_points)],
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
/// Frontline pays 100% (1.0x), Command Operation pays 75% (0.75x), Rearguard pays 50% (0.50x).
pub fn calculate_fw_lp_reward(base_lp: i64, status: i64) -> i64 {
    let mult = match status {
        STATUS_FRONTLINE => 1.0,
        STATUS_COMMAND => 0.75,
        STATUS_REARGUARD => 0.50,
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

/// Awards FW LP for an enemy kill in a warzone solar system, scaled by system status (`feat:fw.fw-lp`).
pub fn award_fw_kill_lp(
    ctx: &ServerContext,
    killer_char_id: i64,
    sys_id: u32,
    victim_ship_value_isk: f64,
) -> i64 {
    let (status, killer_fac_opt) = ctx.db.with_snapshot(|snap| {
        (
            compute_system_frontline_status(ctx, snap, sys_id),
            snap.characters.get(&killer_char_id).and_then(|c| c.war_faction_id),
        )
    });
    let base_lp = (victim_ship_value_isk * 0.0001).clamp(100.0, 50_000.0) as i64;
    let supp_lp_mult = killer_fac_opt
        .map(|fid| crate::services::security_mgr::insurgency_suppression_empire_lp_multiplier(ctx, sys_id, fid))
        .unwrap_or(1.0);
    let final_lp = ((calculate_fw_lp_reward(base_lp, status) as f64) * supp_lp_mult).round() as i64;

    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut fid_opt = None;
        if let Some(ch) = snap.characters.get_mut(&killer_char_id) {
            fid_opt = ch.war_faction_id;
            if let Some(fid) = fid_opt {
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
        record_fw_stat_event_in_snap(snap, now_u, killer_char_id, fid_opt, 1, 20);
    });

    add_victory_points(ctx, sys_id, 20);

    final_lp
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
        ("fwWarzoneSolarsystem", "GetHQSystemIDs") => {
            let hqs = vec![
                PyRep::Integer(i64::from(HQ_SYSTEM_MEHATOOR)),
                PyRep::Integer(i64::from(HQ_SYSTEM_ONNAMON)),
                PyRep::Integer(i64::from(HQ_SYSTEM_INTAKI)),
                PyRep::Integer(i64::from(HQ_SYSTEM_AMO)),
            ];
            Some(Ok(PyRep::List(hqs)))
        }

        ("fwWarzoneSolarsystem", "GetAllWarzones") => {
            let wz1 = PyRep::keyval([
                ("warzoneID", PyRep::Integer(1)),
                ("name", PyRep::String("Caldari - Gallente Warzone".into())),
                ("factionOneID", PyRep::Integer(FACTION_CALDARI)),
                ("factionTwoID", PyRep::Integer(FACTION_GALLENTE)),
            ]);
            let wz2 = PyRep::keyval([
                ("warzoneID", PyRep::Integer(2)),
                ("name", PyRep::String("Amarr - Minmatar Warzone".into())),
                ("factionOneID", PyRep::Integer(FACTION_AMARR)),
                ("factionTwoID", PyRep::Integer(FACTION_MINMATAR)),
            ]);
            Some(Ok(PyRep::List(vec![wz1, wz2])))
        }

        ("fwWarzoneSolarsystem", "GetAllWarzonesOccupationStates")
        | ("fwWarzoneSolarsystem", "GetAllWarzonesOccupationStatesUncached") => {
            ctx.db.with_mut_snapshot(|snap| {
                if snap.fw_system_occupations.is_empty() {
                    snap.backfill_fw_system_occupations();
                }
            });
            let states = ctx.db.with_snapshot(|snap| {
                let frontlines = compute_all_frontline_statuses(ctx, snap);
                let mut list = Vec::new();
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
                    list.push(PyRep::keyval([
                        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                        ("ownerFactionID", PyRep::Integer(occ.owner_faction_id)),
                        ("occupierFactionID", PyRep::Integer(occ.occupier_faction_id)),
                        ("frontlineStatus", PyRep::Integer(front)),
                        ("contestedPercentage", PyRep::Real(c)),
                    ]));
                }
                list
            });
            Some(Ok(PyRep::List(states)))
        }

        ("fwWarzoneSolarsystem", "GetLocalOccupationState") => {
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
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
                Some(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                    ("occupierID", PyRep::Integer(occ.occupier_faction_id)),
                    ("occupierFactionID", PyRep::Integer(occ.occupier_faction_id)),
                    ("ownerID", PyRep::Integer(occ.owner_faction_id)),
                    ("ownerFactionID", PyRep::Integer(occ.owner_faction_id)),
                    ("adjacencyState", PyRep::Integer(front)),
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
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                ("victoryPoints", PyRep::Integer(vp)),
                ("contestedPercentage", PyRep::Real(contested)),
                ("threshold", PyRep::Integer(threshold)),
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
                    map.push((
                        PyRep::Integer(i64::from(sys_id)),
                        PyRep::keyval([
                            ("victoryPoints", PyRep::Integer(vp)),
                            ("contestedPercentage", PyRep::Real(c)),
                            ("threshold", PyRep::Integer(threshold)),
                        ]),
                    ));
                }
                PyRep::Dict(map)
            });
            Some(Ok(res))
        }

        ("dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances") => {
            let sys_id = args.first().and_then(|a| a.as_i64()).and_then(|v| u32::try_from(v).ok()).or(session.solar_system_id);
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

        _ => None,
    }
}
