//! Tournament post-match return & referee home teleport helpers extracted from `tourney_service.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

use super::tourney_common::{
    check_referee_auth, extract_id, load_store, resolve_valid_home_station, save_store,
    TourneyPilot,
};

pub(super) async fn handle_return_players(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    now_unix: u64,
) -> Result<PyRep, BridgeRpcError> {
    let (_, caller_cid) = ServerContext::require_character(session)?;
    let (target_mid, which_team_opt) = if method == "ReturnAllPlayers" {
        (extract_id(args, 0), None)
    } else if args.len() >= 2 {
        (extract_id(args, 0), Some(extract_id(args, 1).unwrap_or(0)))
    } else {
        (None, Some(extract_id(args, 0).unwrap_or(0)))
    };
    let mut returned_transfers = Vec::new();
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let m = store.resolve_match_mut(target_mid)?;
        check_referee_auth(session, caller_cid, m)?;
        let pilots: Vec<TourneyPilot> = match which_team_opt {
            None => {
                m.returned_teams = vec![0, 1];
                m.red_players
                    .iter()
                    .chain(m.blue_players.iter())
                    .cloned()
                    .collect()
            }
            Some(wt) => {
                if !m.returned_teams.contains(&wt) {
                    m.returned_teams.push(wt);
                }
                if wt == 0 {
                    m.red_players.clone()
                } else {
                    m.blue_players.clone()
                }
            }
        };
        for p in &pilots {
            let (old_sys, old_st, raw_home, active_sid) = match snap.characters.get(&p.character_id)
            {
                Some(ch) => (
                    ch.solar_system_id,
                    ch.station_id,
                    ch.home_station_id,
                    if ch.active_ship_id > 0 {
                        ch.active_ship_id
                    } else {
                        p.ship_id
                    },
                ),
                None => continue,
            };
            let (home_st_id, home_sys) = resolve_valid_home_station(ctx, snap, raw_home, old_sys);
            if let Some(ch) = snap.characters.get_mut(&p.character_id) {
                ch.station_id = Some(home_st_id);
                ch.structure_id = None;
                ch.solar_system_id = home_sys;
            }
            let mut valid_ship_id = 0;
            if active_sid > 0 {
                if let Some(ship_it) = snap
                    .items
                    .get_mut(&active_sid)
                    .filter(|it| it.owner_id == p.character_id)
                {
                    ship_it.location_id = home_st_id;
                    ship_it.flag = eve_db::InventoryFlag::Hangar;
                    ship_it.version += 1;
                    valid_ship_id = active_sid;
                }
            }
            returned_transfers.push((
                p.character_id,
                valid_ship_id,
                old_sys,
                old_st,
                home_st_id,
                home_sys,
            ));
        }
        save_store(snap, &store, now_unix);
        Ok(PyRep::Bool(true))
    });
    if res.is_ok() {
        for (cid, ship_id, old_sys, old_st, home_st_id, home_sys) in returned_transfers {
            if ship_id > 0 {
                let _ = ctx.systems.get_or_spawn(old_sys).remove_ball(ship_id).await;
                if home_sys != old_sys {
                    let _ = ctx.systems.get_or_spawn(home_sys).remove_ball(ship_id).await;
                }
            }
            if session.character_id == Some(cid) {
                session.station_id = Some(home_st_id);
                session.structure_id = None;
                session.solar_system_id = Some(home_sys);
            }
            ctx.invalidate_bound_objects_for_session_transition(
                cid,
                home_sys,
                Some(home_st_id),
                "dock",
            );
            let sess_rep = PyRep::keyval([
                ("charid", PyRep::Integer(cid)),
                ("stationid", PyRep::Integer(home_st_id)),
                ("solarsystemid2", PyRep::Integer(i64::from(home_sys))),
                ("solarsystemid", PyRep::None),
            ]);
            ctx.notify(
                crate::router::NotificationTarget::Character(cid),
                "OnSessionChanged",
                "charid",
                vec![
                    PyRep::Bool(false),
                    sess_rep,
                    PyRep::Dict(vec![
                        (
                            PyRep::String("stationid".into()),
                            PyRep::Tuple(vec![
                                old_st.map(PyRep::Integer).unwrap_or(PyRep::None),
                                PyRep::Integer(home_st_id),
                            ]),
                        ),
                        (
                            PyRep::String("solarsystemid".into()),
                            PyRep::Tuple(vec![
                                PyRep::Integer(i64::from(old_sys)),
                                PyRep::Integer(i64::from(home_sys)),
                            ]),
                        ),
                    ]),
                ],
            );
        }
    }
    res
}

pub(super) async fn handle_teleport_ref_home(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, caller_cid) = ServerContext::require_character(session)?;
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
                if let Some(ship_it) = snap
                    .items
                    .get_mut(&ref_ship_id)
                    .filter(|it| it.owner_id == caller_cid)
                {
                    ship_it.location_id = home_st_id;
                    ship_it.flag = eve_db::InventoryFlag::Hangar;
                    ship_it.version += 1;
                    valid_ref_ship = ref_ship_id;
                }
            }
            transfer_info = Some((valid_ref_ship, old_sys, old_st, home_st_id, home_sys));
        }
        Ok(PyRep::Bool(true))
    });

    if res.is_ok() {
        if let Some((ref_ship_id, old_sys, old_st, home_st_id, home_sys)) = transfer_info {
            if ref_ship_id > 0 {
                let _ = ctx.systems.get_or_spawn(old_sys).remove_ball(ref_ship_id).await;
                if home_sys != old_sys {
                    let _ = ctx.systems.get_or_spawn(home_sys).remove_ball(ref_ship_id).await;
                }
            }
            session.station_id = Some(home_st_id);
            session.structure_id = None;
            session.solar_system_id = Some(home_sys);
            ctx.invalidate_bound_objects_for_session_transition(
                caller_cid,
                home_sys,
                Some(home_st_id),
                "dock",
            );
            let sess_rep = PyRep::keyval([
                ("charid", PyRep::Integer(caller_cid)),
                ("stationid", PyRep::Integer(home_st_id)),
                ("solarsystemid2", PyRep::Integer(i64::from(home_sys))),
                ("solarsystemid", PyRep::None),
            ]);
            ctx.notify(
                crate::router::NotificationTarget::Character(caller_cid),
                "OnSessionChanged",
                "charid",
                vec![
                    PyRep::Bool(false),
                    sess_rep,
                    PyRep::Dict(vec![
                        (
                            PyRep::String("stationid".into()),
                            PyRep::Tuple(vec![
                                old_st.map(PyRep::Integer).unwrap_or(PyRep::None),
                                PyRep::Integer(home_st_id),
                            ]),
                        ),
                        (
                            PyRep::String("solarsystemid".into()),
                            PyRep::Tuple(vec![
                                PyRep::Integer(i64::from(old_sys)),
                                PyRep::Integer(i64::from(home_sys)),
                            ]),
                        ),
                    ]),
                ],
            );
        }
    }
    res
}
