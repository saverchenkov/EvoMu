//! `ship_flight_ops` — Supplemental handlers for `ship` machoNet RPCs.
//!
//! Provides handlers for safe logoff timers, scooping (drones, cargo, mobile depots,
//! infrastructure holds), ship dirt timestamps, and session check verification.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

/// Dispatches supplemental `ship` RPC methods.
pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "ship" {
        return None;
    }

    match method {
        "SafeLogoff" => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // If already docked, logoff is immediate (0 delay)
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Ok(PyRep::Integer(0)));
            }
            // Standard safe logoff in space requires a 30-second timer
            Some(Ok(PyRep::Integer(30)))
        }

        "AbortSafeLogoff" => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Bool(true)))
        }

        "Drop" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let active_ship_id = session.ship_id.unwrap_or(0);
            if active_ship_id <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Drop requires an active ship in space".into(),
                }));
            }
            let res = ctx.db.with_mut_snapshot(|snap| {
                if let Some(item) = snap.items.get_mut(&item_id) {
                    if item.owner_id == char_id {
                        item.location_id = active_ship_id;
                        return true;
                    }
                }
                false
            });
            Some(Ok(PyRep::Bool(res)))
        }

        "Scoop" => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Bool(true)))
        }

        "ScoopDrone" => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Bool(true)))
        }

        "ScoopToInfrastructureHold" => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Bool(true)))
        }

        "ScoopToMobileDepotHold" => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Bool(true)))
        }

        "GetDirtTimestamp" => {
            let ship_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.ship_id)
                .unwrap_or(0);
            let dirt_unix = ctx.db.with_snapshot(|snap| {
                snap.items
                    .get(&ship_id)
                    .map(|_| 1609459200u64) // 2021-01-01
                    .unwrap_or(1609459200u64)
            });
            let filetime = (dirt_unix.saturating_add(11_644_473_600)) as i64 * 10_000_000;
            Some(Ok(PyRep::Integer(filetime)))
        }

        "ResetDirtTimestamp" => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let filetime = (now_unix.saturating_add(11_644_473_600)) as i64 * 10_000_000;
            Some(Ok(PyRep::Integer(filetime)))
        }

        "SetSessionCheck" => {
            Some(Ok(PyRep::None))
        }

        "Undock" => {
            let (user_id, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (sys_id, ship_id) = if session.station_id.is_some() || session.structure_id.is_some() {
                match ctx.db.undock_character(user_id, char_id) {
                    Ok((_st_id, ch, ship)) => {
                        session.station_id = None;
                        session.structure_id = None;
                        (ch.solar_system_id, ship.item_id)
                    }
                    Err(e) => {
                        return Some(Err(BridgeRpcError {
                            code: "UndockFailed".into(),
                            message: e.to_string(),
                        }));
                    }
                }
            } else {
                (
                    session.solar_system_id.unwrap_or(30000142),
                    session.ship_id.unwrap_or(0),
                )
            };
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("shipID", PyRep::Integer(ship_id)),
            ])))
        }

        "clear_memoized" => {
            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}
