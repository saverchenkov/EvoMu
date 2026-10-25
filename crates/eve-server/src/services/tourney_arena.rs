//! `tourney_arena` — Tournament arena setup, match pairings query, and referee/competitor teleportation.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::router::{NotificationTarget, ServerContext};
use crate::services::tourney_service::{
    check_referee_auth, extract_id, load_store, resolve_valid_home_station,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

macro_rules! require_cid {
    ($session:expr) => {
        match $session.character_id {
            Some(id) if id > 0 => id,
            _ => {
                return Some(Err(BridgeRpcError {
                    code: "AuthenticationRequired".into(),
                    message: "Character authentication required for tourney operation".into(),
                }))
            }
        }
    };
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "tourneyMgr" {
        return None;
    }

    match method {
        // parity: rpc:tourneyMgr.GetPotentialMatches
        "GetPotentialMatches" => {
            let _caller_cid = require_cid!(session);
            let tourney_id = extract_id(args, 0).unwrap_or(0);
            let matches_rep = ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let list: Vec<PyRep> = store
                    .matches
                    .values()
                    .filter(|m| tourney_id == 0 || m.tourney_id == tourney_id)
                    .map(|m| {
                        PyRep::Tuple(vec![
                            PyRep::Integer(m.match_id),
                            PyRep::String(m.red_team_name.clone()),
                            PyRep::String(m.blue_team_name.clone()),
                            PyRep::String(m.state.as_str().into()),
                        ])
                    })
                    .collect();
                PyRep::List(list)
            });
            Some(Ok(matches_rep))
        }

        // parity: rpc:tourneyMgr.GetTourneySetup
        "GetTourneySetup" => {
            let _caller_cid = require_cid!(session);
            let (tourneys_rep, systems_rep, rules_rep) = ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                let mut tourneys = vec![
                    (
                        PyRep::Integer(1),
                        PyRep::String("Alliance Tournament".into()),
                    ),
                    (
                        PyRep::Integer(2),
                        PyRep::String("Open Arena Championship".into()),
                    ),
                ];
                for m in store.matches.values() {
                    if !tourneys.iter().any(|(k, _)| k.as_i64() == Some(m.tourney_id)) {
                        tourneys.push((
                            PyRep::Integer(m.tourney_id),
                            PyRep::String(format!("Tournament {}", m.tourney_id)),
                        ));
                    }
                }

                let cur_sys = session.solar_system_id.unwrap_or(30000142) as i64;
                let mut systems = vec![
                    (
                        PyRep::Integer(cur_sys),
                        PyRep::Tuple(vec![
                            PyRep::Integer(cur_sys),
                            PyRep::String("Local Tournament Grid".into()),
                        ]),
                    ),
                    (
                        PyRep::Integer(30000142),
                        PyRep::Tuple(vec![
                            PyRep::Integer(30000142),
                            PyRep::String("Jita Arena".into()),
                        ]),
                    ),
                ];
                for m in store.matches.values() {
                    if !systems.iter().any(|(k, _)| k.as_i64() == Some(m.solar_system_id)) {
                        systems.push((
                            PyRep::Integer(m.solar_system_id),
                            PyRep::Tuple(vec![
                                PyRep::Integer(m.solar_system_id),
                                PyRep::String(format!("Arena System {}", m.solar_system_id)),
                            ]),
                        ));
                    }
                }

                let rules = PyRep::List(vec![
                    PyRep::Integer(0),
                    PyRep::Integer(1),
                    PyRep::Integer(2),
                ]);

                (PyRep::Dict(tourneys), PyRep::Dict(systems), rules)
            });
            Some(Ok(PyRep::Tuple(vec![tourneys_rep, systems_rep, rules_rep])))
        }

        // parity: rpc:tourneyMgr.TeleportToArena
        "TeleportToArena" => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);
            let mut arena_sys = session.solar_system_id.unwrap_or(30000142) as i64;

            let auth_res = ctx.db.with_snapshot(|snap| {
                let store = load_store(snap);
                if let Some(m) = store.resolve_match(target_mid) {
                    check_referee_auth(session, caller_cid, m)?;
                    arena_sys = m.solar_system_id;
                    Ok(())
                } else if session.has_gm_role() {
                    if let Some(active_m) = store.resolve_match(None) {
                        arena_sys = active_m.solar_system_id;
                    }
                    Ok(())
                } else {
                    Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Caller is not an authorized tournament referee or GM".into(),
                    })
                }
            });
            if let Err(e) = auth_res {
                return Some(Err(e));
            }

            let mut old_sys = 0u32;
            let mut old_st = None;
            let mut ship_id = 0i64;
            let mut ship_type_id = 0u32;

            let update_res = ctx.db.try_with_mut_snapshot(|snap| {
                let ch = snap.characters.get_mut(&caller_cid).ok_or_else(|| BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: "Character not found".into(),
                })?;
                old_sys = ch.solar_system_id;
                old_st = ch.station_id;
                ch.solar_system_id = arena_sys as u32;
                ch.station_id = None;
                ch.structure_id = None;
                ship_id = ch.active_ship_id;

                if ship_id > 0 {
                    if let Some(ship_it) = snap.items.get_mut(&ship_id).filter(|it| it.owner_id == caller_cid) {
                        ship_it.location_id = arena_sys;
                        ship_it.flag = eve_db::InventoryFlag::None;
                        ship_it.version += 1;
                        ship_type_id = ship_it.type_id;
                    }
                }
                Ok(())
            });
            if let Err(e) = update_res {
                return Some(Err(e));
            }

            session.solar_system_id = Some(arena_sys as u32);
            session.station_id = None;
            session.structure_id = None;

            let sys_handle = ctx.systems.get_or_spawn(arena_sys as u32);
            if ship_id > 0 && ship_type_id > 0 {
                let ref_pos = DVec3::new(0.0, 10_000.0, 0.0);
                let (mass, radius, max_vel, inertia) = ctx.sde.types.get(&ship_type_id).map(|t| {
                    (if t.mass > 0.0 { t.mass } else { 1_200_000.0 }, if t.radius > 0.0 { t.radius } else { 80.0 }, t.default_attributes.get(&37).copied().unwrap_or(300.0), t.default_attributes.get(&70).copied().unwrap_or(2.5))
                }).unwrap_or((1_200_000.0, 80.0, 300.0, 2.5));
                let cs = ctx.build_ship_combat_state(caller_cid, ship_id, ship_type_id);
                let _ = sys_handle.spawn_ball_with_combat(
                    eve_destiny::Ball { id: ship_id, type_id: ship_type_id, position: ref_pos, velocity: DVec3::ZERO, max_velocity: max_vel, inertia, mass, radius, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop },
                    cs,
                ).await;
            }

            if old_sys != arena_sys as u32 || old_st.is_some() {
                let sess_rep = PyRep::keyval([
                    ("charid", PyRep::Integer(caller_cid)),
                    ("stationid", PyRep::None),
                    ("solarsystemid2", PyRep::Integer(arena_sys)),
                    ("solarsystemid", PyRep::Integer(arena_sys)),
                ]);
                ctx.notify(
                    NotificationTarget::Character(caller_cid),
                    "OnSessionChanged",
                    "charid",
                    vec![
                        PyRep::Bool(false),
                        sess_rep,
                        PyRep::Dict(vec![
                            (
                                PyRep::String("stationid".into()),
                                PyRep::Tuple(vec![old_st.map(PyRep::Integer).unwrap_or(PyRep::None), PyRep::None]),
                            ),
                            (
                                PyRep::String("solarsystemid".into()),
                                PyRep::Tuple(vec![PyRep::Integer(i64::from(old_sys)), PyRep::Integer(arena_sys)]),
                            ),
                        ]),
                    ],
                );
            }

            Some(Ok(PyRep::Bool(true)))
        }

        // parity: rpc:tourneyMgr.TeleportToStation
        "TeleportToStation" => {
            let caller_cid = require_cid!(session);
            let target_mid = extract_id(args, 0);
            let mut transfer_info = None;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let store = load_store(snap);
                if let Some(m) = store.resolve_match(target_mid) {
                    check_referee_auth(session, caller_cid, m)?;
                } else if !session.has_gm_role() {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Caller is not an authorized tournament referee or GM".into(),
                    });
                }

                if let Some((old_sys, old_st, raw_home, ref_ship_id)) = snap
                    .characters
                    .get(&caller_cid)
                    .map(|ch| (ch.solar_system_id, ch.station_id, ch.home_station_id, ch.active_ship_id))
                {
                    let (home_st_id, home_sys) = resolve_valid_home_station(ctx, snap, raw_home, old_sys);
                    if let Some(ch) = snap.characters.get_mut(&caller_cid) {
                        ch.station_id = Some(home_st_id);
                        ch.structure_id = None;
                        ch.solar_system_id = home_sys;
                    }
                    let mut valid_ref_ship = 0;
                    if ref_ship_id > 0 {
                        if let Some(ship_it) = snap.items.get_mut(&ref_ship_id).filter(|it| it.owner_id == caller_cid) {
                            ship_it.location_id = home_st_id;
                            ship_it.flag = eve_db::InventoryFlag::Hangar;
                            ship_it.version += 1;
                            valid_ref_ship = ship_it.item_id;
                        }
                    }
                    transfer_info = Some((old_sys, old_st, home_sys, home_st_id, valid_ref_ship));
                }
                Ok(PyRep::Bool(true))
            });

            if res.is_ok() {
                if let Some((old_sys, old_st, home_sys, home_st_id, ref_ship)) = transfer_info {
                    session.solar_system_id = Some(home_sys);
                    session.station_id = Some(home_st_id);
                    session.structure_id = None;
                    if ref_ship > 0 {
                        let sys_handle = ctx.systems.get_or_spawn(old_sys);
                        let _ = sys_handle.remove_ball(ref_ship).await;
                    }
                    let sess_rep = PyRep::keyval([
                        ("charid", PyRep::Integer(caller_cid)),
                        ("stationid", PyRep::Integer(home_st_id)),
                        ("solarsystemid2", PyRep::Integer(home_sys as i64)),
                        ("solarsystemid", PyRep::Integer(home_sys as i64)),
                    ]);
                    ctx.notify(
                        NotificationTarget::Character(caller_cid),
                        "OnSessionChanged",
                        "charid",
                        vec![
                            PyRep::Bool(false),
                            sess_rep,
                            PyRep::Dict(vec![
                                (
                                    PyRep::String("stationid".into()),
                                    PyRep::Tuple(vec![old_st.map(PyRep::Integer).unwrap_or(PyRep::None), PyRep::Integer(home_st_id)]),
                                ),
                                (
                                    PyRep::String("solarsystemid".into()),
                                    PyRep::Tuple(vec![PyRep::Integer(i64::from(old_sys)), PyRep::Integer(i64::from(home_sys))]),
                                ),
                            ]),
                        ],
                    );
                }
            }

            Some(res)
        }

        _ => None,
    }
}
