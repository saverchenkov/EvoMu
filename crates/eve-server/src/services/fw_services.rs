//! `fw_services` — Faction Warfare Warzones, Victory Points, Advantage & HQ Systems (`§3.11`).

#![forbid(unsafe_code)]

use crate::router::NotificationTarget;
use crate::router::ServerContext;
pub use eve_db::{
    PersistedBattlefieldRecord as BattlefieldRecord, PersistedFwState as FwRuntimeState,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

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

pub fn with_fw_runtime<R>(ctx: &ServerContext, f: impl FnOnce(&mut FwRuntimeState) -> R) -> R {
    ctx.db.with_mut_snapshot(|snap| f(&mut snap.fw_runtime))
}

pub fn read_fw_runtime<R>(ctx: &ServerContext, f: impl FnOnce(&FwRuntimeState) -> R) -> R {
    ctx.db.with_snapshot(|snap| f(&snap.fw_runtime))
}

/// Broadcasts `OnSolarsystemAdvantageStateUpdated_Local` notification.
pub fn emit_advantage_updated(ctx: &ServerContext, sys_id: u32, friendly: f64, enemy: f64) {
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
}

/// Updates advantage in a warzone system and notifies clients.
pub fn update_system_advantage(ctx: &ServerContext, sys_id: u32, friendly_delta: f64, enemy_delta: f64) {
    let (friendly, enemy) = with_fw_runtime(ctx, |rt| {
        let entry = rt.system_advantage.entry(sys_id).or_insert((0.0, 0.0));
        entry.0 = (entry.0 + friendly_delta).clamp(0.0, 100.0);
        entry.1 = (entry.1 + enemy_delta).clamp(0.0, 100.0);
        *entry
    });
    emit_advantage_updated(ctx, sys_id, friendly, enemy);
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

    with_fw_runtime(ctx, |rt| {
        let entry = rt.system_victory_points.entry(sys_id).or_insert(0);
        *entry = new_vp;
    });
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
    let front = if contested > 75.0 {
        STATUS_FRONTLINE
    } else if contested > 25.0 {
        STATUS_COMMAND
    } else {
        STATUS_REARGUARD
    };
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

/// Transitions a Faction Warfare bunker's vulnerability state and emits `OnFacWarBunkerVulnerable` or `OnFacWarBunkerNonVulnerable`.
pub fn set_facwar_bunker_vulnerable(ctx: &ServerContext, sys_id: u32, attacker_id: i64, vulnerable: bool) {
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
pub fn calculate_fw_lp_reward(base_lp: i64, status: i64) -> i64 {
    let mult = match status {
        STATUS_FRONTLINE => 1.5,
        STATUS_COMMAND => 1.0,
        STATUS_REARGUARD => 0.75,
        _ => 1.0,
    };
    ((base_lp as f64) * mult).round() as i64
}

/// Awards FW LP for an enemy kill in a warzone solar system, scaled by system status (`feat:fw.fw-lp`).
pub fn award_fw_kill_lp(
    ctx: &ServerContext,
    killer_char_id: i64,
    sys_id: u32,
    victim_ship_value_isk: f64,
) -> i64 {
    let contested_pct = ctx.db.with_snapshot(|snap| {
        snap.system_states.get(&sys_id).map(|s| s.fw_contested_pct).unwrap_or(0.0)
    });
    let status = if contested_pct > 75.0 {
        STATUS_FRONTLINE
    } else if contested_pct > 25.0 {
        STATUS_COMMAND
    } else {
        STATUS_REARGUARD
    };
    let base_lp = (victim_ship_value_isk * 0.0001).clamp(100.0, 50_000.0) as i64;
    let final_lp = calculate_fw_lp_reward(base_lp, status);

    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut fid = FACTION_CALDARI;
        if let Some(ch) = snap.characters.get_mut(&killer_char_id) {
            fid = ch.war_faction_id.unwrap_or(FACTION_CALDARI);
            let militia_corp = ctx
                .sde
                .factions
                .get(&(fid as u32))
                .and_then(|f| f.militia_corporation_id)
                .map(|c| c as i64)
                .unwrap_or(fid);
            *ch.loyalty_points.entry(militia_corp).or_insert(0) += final_lp;
            *ch.loyalty_points.entry(fid).or_insert(0) += final_lp;
        }
        let rec = snap.fw_character_stats.entry(killer_char_id).or_insert_with(|| {
            eve_db::snapshot_ext::FwCharacterStatsRecord {
                character_id: killer_char_id,
                faction_id: Some(fid),
                enlisted_on_unix: Some(now_u),
                current_rank: 1,
                highest_rank: 1,
                kills: Default::default(),
                victory_points: Default::default(),
            }
        });
        rec.kills.yesterday += 1;
        rec.kills.last_week += 1;
        rec.kills.total += 1;
        rec.victory_points.yesterday += 20;
        rec.victory_points.last_week += 20;
        rec.victory_points.total += 20;
    });

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
            let states = ctx.db.with_snapshot(|snap| {
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
                    let front = if c > 75.0 {
                        STATUS_FRONTLINE
                    } else if c > 25.0 {
                        STATUS_COMMAND
                    } else {
                        STATUS_REARGUARD
                    };
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
                let front = if c > 75.0 {
                    STATUS_FRONTLINE
                } else if c > 25.0 {
                    STATUS_COMMAND
                } else {
                    STATUS_REARGUARD
                };
                Some(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                    ("ownerFactionID", PyRep::Integer(occ.owner_faction_id)),
                    ("occupierFactionID", PyRep::Integer(occ.occupier_faction_id)),
                    ("frontlineStatus", PyRep::Integer(front)),
                    ("contestedPercentage", PyRep::Real(c)),
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
                    .filter(|b| sys_id.map(|s| b.solar_system_id == s).unwrap_or(true))
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
