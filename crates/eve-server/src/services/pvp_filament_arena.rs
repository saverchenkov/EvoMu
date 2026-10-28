//! `pvp_filament_arena` — Proving Grounds multi-team / FFA match resolution, spatial arena isolation,
//! multi-tick player-ship destruction hooks, and `AbyssalPVPEndGateActivation` extraction.

#![forbid(unsafe_code)]

use crate::router::cache_notifications::NotificationTarget;
use crate::router::ServerContext;
use crate::services::pve_abyssal_hazards::resolve_character_home_station_and_system;
use crate::services::pvp_filament_mgr::{
    build_character_statistics_for_event, build_leaderboard_rows_for_event, load_pvp_store,
    save_pvp_store, PvpArenaMatchInstance, PvpFilamentStore,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

pub fn effective_match_teams(store: &PvpFilamentStore, m: &PvpArenaMatchInstance) -> Vec<Vec<i64>> {
    if let Some(teams) = store.match_teams.get(&m.match_id) {
        if !teams.is_empty() {
            return teams.clone();
        }
    }
    vec![m.team_a.clone(), m.team_b.clone()]
}

pub fn match_contains_char(store: &PvpFilamentStore, m: &PvpArenaMatchInstance, cid: i64) -> bool {
    effective_match_teams(store, m)
        .iter()
        .any(|t| t.contains(&cid))
}

pub fn are_opposing_in_match(
    store: &PvpFilamentStore,
    m: &PvpArenaMatchInstance,
    char_a: i64,
    char_b: i64,
) -> bool {
    let teams = effective_match_teams(store, m);
    let ta = teams.iter().position(|t| t.contains(&char_a));
    let tb = teams.iter().position(|t| t.contains(&char_b));
    matches!((ta, tb), (Some(ia), Some(ib)) if ia != ib)
}

/// Applies a Proving Grounds kill inside `snap`, advancing team elimination and concluding the match
/// only when at most one team has surviving (non-eliminated) pilots.
pub fn record_proving_grounds_combat_result_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    winner_char_id: i64,
    loser_char_id: i64,
    damage_done: f64,
) -> Result<(i64, Vec<i64>), BridgeRpcError> {
    if winner_char_id == loser_char_id {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "Winner and loser must be distinct characters".into(),
        });
    }
    let mut store = load_pvp_store(snap);
    let active_mid = store
        .active_matches
        .values()
        .find(|m| m.active && are_opposing_in_match(&store, m, winner_char_id, loser_char_id))
        .map(|m| m.match_id);
    let both_queued =
        store.queue.contains_key(&winner_char_id) && store.queue.contains_key(&loser_char_id);

    if active_mid.is_none() && !both_queued {
        return Err(BridgeRpcError {
            code: "NotInPvpQueue".into(),
            message: "Both combatants must be queued in Proving Grounds or in an active match".into(),
        });
    }

    let mut affected_pilots = vec![winner_char_id, loser_char_id];
    let matched_event_id = if let Some(mid) = active_mid {
        let teams = store
            .active_matches
            .get(&mid)
            .map(|m| effective_match_teams(&store, m))
            .unwrap_or_default();
        let ev_id = store
            .active_matches
            .get(&mid)
            .map(|m| m.event_id)
            .unwrap_or(171);

        let elim = store.match_eliminated.entry(mid).or_default();
        elim.insert(loser_char_id);

        // Credit individual kill + damage to the killer immediately
        let wk = store.stats.entry(winner_char_id).or_insert((0, 0, 0, 0, 0.0));
        wk.3 += 1;
        wk.4 += damage_done;
        let ewk = store
            .event_stats
            .entry(ev_id)
            .or_default()
            .entry(winner_char_id)
            .or_insert((0, 0, 0, 0, 0.0));
        ewk.3 += 1;
        ewk.4 += damage_done;

        let surviving_team_indices: Vec<usize> = teams
            .iter()
            .enumerate()
            .filter_map(|(idx, t)| {
                t.iter()
                    .any(|&cid| !elim.contains(&cid))
                    .then_some(idx)
            })
            .collect();

        if surviving_team_indices.len() <= 1 {
            let win_team_idx = surviving_team_indices
                .first()
                .copied()
                .or_else(|| teams.iter().position(|t| t.contains(&winner_char_id)))
                .unwrap_or(0);
            if let Some(m) = store.active_matches.get_mut(&mid) {
                m.active = false;
                m.winner_char_id = Some(winner_char_id);
            }
            for (t_idx, team) in teams.iter().enumerate() {
                for &cid in team {
                    store.queue.remove(&cid);
                    if !affected_pilots.contains(&cid) {
                        affected_pilots.push(cid);
                    }
                    if t_idx == win_team_idx {
                        let st = store.stats.entry(cid).or_insert((0, 0, 0, 0, 0.0));
                        st.0 += 1;
                        st.2 += 100;
                        let est = store
                            .event_stats
                            .entry(ev_id)
                            .or_default()
                            .entry(cid)
                            .or_insert((0, 0, 0, 0, 0.0));
                        est.0 += 1;
                        est.2 += 100;
                    } else {
                        let st = store.stats.entry(cid).or_insert((0, 0, 0, 0, 0.0));
                        st.1 += 1;
                        st.2 += 25;
                        let est = store
                            .event_stats
                            .entry(ev_id)
                            .or_default()
                            .entry(cid)
                            .or_insert((0, 0, 0, 0, 0.0));
                        est.1 += 1;
                        est.2 += 25;
                    }
                }
            }
        }
        ev_id
    } else {
        store.queue.remove(&winner_char_id);
        store.queue.remove(&loser_char_id);
        let ev_id = store.events.last().map(|e| e.event_id).unwrap_or(171);

        let w = store.stats.entry(winner_char_id).or_insert((0, 0, 0, 0, 0.0));
        w.0 += 1;
        w.2 += 100;
        w.3 += 1;
        w.4 += damage_done;
        let l = store.stats.entry(loser_char_id).or_insert((0, 0, 0, 0, 0.0));
        l.1 += 1;
        l.2 += 25;

        let ev_stats = store.event_stats.entry(ev_id).or_default();
        let ew = ev_stats.entry(winner_char_id).or_insert((0, 0, 0, 0, 0.0));
        ew.0 += 1;
        ew.2 += 100;
        ew.3 += 1;
        ew.4 += damage_done;
        let el = ev_stats.entry(loser_char_id).or_insert((0, 0, 0, 0, 0.0));
        el.1 += 1;
        el.2 += 25;
        ev_id
    };

    save_pvp_store(snap, &store);
    Ok((matched_event_id, affected_pilots))
}

pub fn notify_pvp_stats_for_pilots(ctx: &ServerContext, event_id: i64, pilots: &[i64]) {
    let (pilot_stats, leaderboard) = ctx.db.with_snapshot(|snap| {
        let store = load_pvp_store(snap);
        let pstats: Vec<(i64, PyRep)> = pilots
            .iter()
            .map(|&cid| (cid, build_character_statistics_for_event(&store, event_id, cid)))
            .collect();
        let lb = build_leaderboard_rows_for_event(&store, event_id);
        (pstats, lb)
    });
    let lb_notif = PyRep::keyval([
        ("eventID", PyRep::Integer(event_id)),
        ("event_id", PyRep::Integer(event_id)),
        ("entries", PyRep::List(leaderboard)),
    ]);
    for (cid, stats_rep) in pilot_stats {
        let stat_notif = PyRep::keyval([
            ("eventID", PyRep::Integer(event_id)),
            ("event_id", PyRep::Integer(event_id)),
            ("statistics", stats_rep),
        ]);
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnPVPFilamentsCharacterStatistics",
            "charid",
            vec![stat_notif],
        );
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnPVPFilamentsLeaderboard",
            "charid",
            vec![lb_notif.clone()],
        );
    }
}



#[allow(clippy::type_complexity)]
pub async fn spawn_matched_arena_instance(
    ctx: &ServerContext,
    session: &mut SessionState,
    mid: i64,
    end_gate_id: i64,
    arena_offset: [f64; 3],
    teams: Vec<Vec<i64>>,
    participants: Vec<(i64, i64, u32)>,
) {
    session.solar_system_id = Some(32_000_001);
    let arena = ctx.systems.get_or_spawn(32_000_001);
    let center = DVec3::from_array(arena_offset);
    if arena.get_ball_snapshot(end_gate_id).await.ok().flatten().is_none() {
        let gate_ball = Ball {
            id: end_gate_id,
            type_id: 47459,
            position: center,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let _ = arena.spawn_ball_with_combat(gate_ball, Default::default()).await;
    }

    let get_ball_physics = |ship_type_id: u32| -> (f64, f64, f64, f64) {
        ctx.sde
            .types
            .get(&ship_type_id)
            .map(|st| {
                let mv = st
                    .default_attributes
                    .get(&eve_sde::attr_ids::MAX_VELOCITY)
                    .copied()
                    .unwrap_or(250.0);
                let ag = st
                    .default_attributes
                    .get(&eve_sde::attr_ids::AGILITY)
                    .copied()
                    .unwrap_or(0.5);
                (st.mass, st.radius, ag, mv)
            })
            .unwrap_or((1_000_000.0, 50.0, 0.5, 250.0))
    };

    let num_teams = teams.len().max(2);
    for (pid, p_ship_id, p_origin_sys) in participants {
        crate::services::map_service::record_solar_system_visit(ctx, pid, 32_000_001);
        let sess_rep = PyRep::keyval([
            ("charid", PyRep::Integer(pid)),
            ("solarsystemid2", PyRep::Integer(32_000_001)),
            ("solarsystemid", PyRep::Integer(32_000_001)),
            ("matchID", PyRep::Integer(mid)),
        ]);
        ctx.notify(
            NotificationTarget::Character(pid),
            "OnSessionChanged",
            "charid",
            vec![
                PyRep::Bool(false),
                sess_rep,
                PyRep::Dict(vec![(
                    PyRep::String("solarsystemid".into()),
                    PyRep::Tuple(vec![
                        PyRep::Integer(p_origin_sys as i64),
                        PyRep::Integer(32_000_001),
                    ]),
                )]),
            ],
        );
        crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(
            ctx,
            pid,
            p_ship_id,
            p_origin_sys,
        );
        ctx.systems.get_or_spawn(p_origin_sys).try_remove_ball(p_ship_id);

        let p_ship_type = ctx
            .db
            .with_snapshot(|s| s.items.get(&p_ship_id).map(|i| i.type_id).unwrap_or(621));
        let (mass, radius, inertia, max_vel) = get_ball_physics(p_ship_type);
        let cs = ctx.build_ship_combat_state(pid, p_ship_id, p_ship_type);

        let (t_idx, m_idx) = teams
            .iter()
            .enumerate()
            .find_map(|(ti, t)| t.iter().position(|&c| c == pid).map(|mi| (ti, mi)))
            .unwrap_or((0, 0));
        let rel_pos = if num_teams == 2 {
            let x = if t_idx == 0 { -10_000.0 } else { 10_000.0 };
            DVec3::new(x, (m_idx as f64) * 2_000.0, 0.0)
        } else {
            let theta = std::f64::consts::TAU * (t_idx as f64) / (num_teams as f64);
            DVec3::new(
                10_000.0 * theta.cos(),
                10_000.0 * theta.sin() + (m_idx as f64) * 2_000.0,
                0.0,
            )
        };
        let ball = Ball {
            id: p_ship_id,
            type_id: p_ship_type,
            position: center + rel_pos,
            velocity: DVec3::ZERO,
            max_velocity: max_vel,
            inertia,
            mass,
            radius,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let _ = arena.spawn_ball_with_combat(ball, cs).await;
    }
}

pub async fn handle_pvp_end_gate_activation(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    args: &[PyRep],
    now_unix: u64,
) -> Result<PyRep, BridgeRpcError> {
    let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);

    let (is_in_arena, active_ship_id) = ctx.db.with_snapshot(|snap| {
        let c = snap.characters.get(&char_id);
        let sys = session
            .solar_system_id
            .or_else(|| c.map(|c| c.solar_system_id))
            .unwrap_or(0);
        let ship = session
            .ship_id
            .or_else(|| c.map(|c| c.active_ship_id))
            .filter(|&s| s > 0);
        (sys == 32_000_001, ship)
    });

    if !is_in_arena {
        return Err(BridgeRpcError::user_error("NotInArena"));
    }

    let match_precheck = ctx.db.with_snapshot(|snap| {
        let store = load_pvp_store(snap);
        for m in store.active_matches.values() {
            if m.arena_solar_system_id == 32_000_001
                && match_contains_char(&store, m, char_id)
                && !m.exited_pilots.contains(&char_id)
            {
                return Some((
                    m.match_id,
                    m.end_gate_id,
                    m.winner_char_id,
                    m.active,
                    m.started_unix,
                ));
            }
        }
        None
    });

    let Some((mid, end_gate_id, winner_char_id, m_active, m_started_unix)) = match_precheck else {
        return Err(BridgeRpcError::user_error("NoActiveMatch"));
    };

    let match_timed_out = now_unix >= m_started_unix.saturating_add(900);
    if winner_char_id.is_none() && m_active && !match_timed_out {
        return Err(BridgeRpcError::user_error("MatchNotConcluded"));
    }

    let target_gate_id = if end_gate_id > 0 { end_gate_id } else { 70_001 };
    if gate_id > 0 && gate_id != target_gate_id && gate_id != 70_001 {
        return Err(BridgeRpcError::user_error("TooFarFromGate"));
    }

    let arena = ctx.systems.get_or_spawn(32_000_001);
    let Some((_, gb)) = arena.get_ball_snapshot(target_gate_id).await.ok().flatten() else {
        return Err(BridgeRpcError::user_error("TooFarFromGate"));
    };
    let Some(sid) = active_ship_id else {
        return Err(BridgeRpcError::user_error("TooFarFromGate"));
    };
    let Some((_, sb)) = arena.get_ball_snapshot(sid).await.ok().flatten() else {
        return Err(BridgeRpcError::user_error("TooFarFromGate"));
    };
    if sb.position.distance(gb.position) > 50_000.0 {
        return Err(BridgeRpcError::user_error("TooFarFromGate"));
    }

    let match_info = ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.queue.remove(&char_id);
        let fallback_sys = resolve_character_home_station_and_system(snap, &ctx.sde, char_id).1;
        let opos = store
            .origin_positions
            .get(&char_id)
            .copied()
            .unwrap_or([0.0, 0.0, 0.0]);
        let teams = store
            .active_matches
            .get(&mid)
            .map(|m| effective_match_teams(&store, m))
            .unwrap_or_default();
        let elim = store.match_eliminated.get(&mid).cloned().unwrap_or_default();

        let mut matched_entry = None;
        let mut cleanup_gate_id = None;
        if let Some(m) = store.active_matches.get_mut(&mid) {
            if !m.exited_pilots.contains(&char_id) {
                let osys = m.origin_systems.get(&char_id).copied().unwrap_or(fallback_sys);
                m.exited_pilots.insert(char_id);
                let all_done = teams
                    .iter()
                    .flatten()
                    .all(|cid| m.exited_pilots.contains(cid) || elim.contains(cid));
                if all_done {
                    m.active = false;
                    cleanup_gate_id = Some(target_gate_id);
                }
                matched_entry = Some((m.match_id, osys, opos));
            }
        }
        if cleanup_gate_id.is_some() {
            store.match_arena_offsets.remove(&mid);
        }
        let Some((mid, osys, opos)) = matched_entry else {
            save_pvp_store(snap, &store);
            return None;
        };
        let active_ship_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.active_ship_id)
            .filter(|&s| s > 0);
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = osys;
        }
        if let Some(sid) = active_ship_id {
            if let Some(sh) = snap.items.get_mut(&sid) {
                sh.location_id = osys as i64;
            }
            snap.item_positions.insert(sid, opos);
        }
        save_pvp_store(snap, &store);
        Some((mid, osys, opos, active_ship_id, cleanup_gate_id))
    });

    let Some((_mid, osys, opos, active_ship_id, cleanup_gate_id)) = match_info else {
        return Err(BridgeRpcError::user_error("NoActiveMatch"));
    };

    session.solar_system_id = Some(osys);
    crate::services::map_service::record_solar_system_visit(ctx, char_id, osys);
    let cur_ship = active_ship_id.or(session.ship_id).unwrap_or(char_id);
    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, cur_ship, 32_000_001);
    arena.try_remove_ball(cur_ship);
    if let Some(gid) = cleanup_gate_id.filter(|&g| g > 70_001) {
        arena.try_remove_ball(gid);
    }

    let origin_sys_handle = ctx.systems.get_or_spawn(osys);
    let ship_type = ctx
        .db
        .with_snapshot(|s| s.items.get(&cur_ship).map(|i| i.type_id).unwrap_or(621));
    let (mass, radius, inertia, max_vel) = ctx
        .sde
        .types
        .get(&ship_type)
        .map(|st| {
            let mv = st
                .default_attributes
                .get(&eve_sde::attr_ids::MAX_VELOCITY)
                .copied()
                .unwrap_or(250.0);
            let ag = st
                .default_attributes
                .get(&eve_sde::attr_ids::AGILITY)
                .copied()
                .unwrap_or(0.5);
            (st.mass, st.radius, ag, mv)
        })
        .unwrap_or((1_000_000.0, 50.0, 0.5, 250.0));
    let cs = ctx.build_ship_combat_state(char_id, cur_ship, ship_type);
    let ball = Ball {
        id: cur_ship,
        type_id: ship_type,
        position: DVec3::from_array(opos),
        velocity: DVec3::ZERO,
        max_velocity: max_vel,
        inertia,
        mass,
        radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = origin_sys_handle.spawn_ball_with_combat(ball, cs).await;

    let sess_rep = PyRep::keyval([
        ("charid", PyRep::Integer(char_id)),
        ("solarsystemid2", PyRep::Integer(osys as i64)),
        ("solarsystemid", PyRep::Integer(osys as i64)),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSessionChanged",
        "charid",
        vec![
            PyRep::Bool(false),
            sess_rep,
            PyRep::Dict(vec![(
                PyRep::String("solarsystemid".into()),
                PyRep::Tuple(vec![
                    PyRep::Integer(32_000_001),
                    PyRep::Integer(osys as i64),
                ]),
            )]),
        ],
    );

    Ok(PyRep::keyval([
        ("gateID", PyRep::Integer(if gate_id > 0 { gate_id } else { target_gate_id })),
        ("activated", PyRep::Bool(true)),
        ("originSystemID", PyRep::Integer(osys as i64)),
    ]))
}
