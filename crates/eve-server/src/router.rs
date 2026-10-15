//! `router` — Authenticated Asynchronous `machoNet` & Client Bridge RPC Service Router.

use eve_db::{InventoryFlag, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tracing::{info, warn};

use crate::system_actor::SystemRegistry;

static NEXT_BOUND_ID: AtomicU64 = AtomicU64::new(10_000);

#[derive(Debug, Clone)]
pub struct BoundObjectEntry {
    pub bound_id: u64,
    pub session_id: u64,
    pub character_id: i64,
    pub service: String,
    pub object_id: i64,
}

pub struct ServerContext {
    pub sde: Arc<SdeRegistry>,
    pub db: Arc<WorldRepository>,
    pub systems: Arc<SystemRegistry>,
    pub bound_objects: RwLock<HashMap<u64, BoundObjectEntry>>,
}

impl Default for ServerContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerContext {
    pub fn new() -> Self {
        Self {
            sde: Arc::new(SdeRegistry::with_canonical_seed()),
            db: Arc::new(WorldRepository::with_demo_seed()),
            systems: Arc::new(SystemRegistry::new()),
            bound_objects: RwLock::new(HashMap::new()),
        }
    }

    /// Releases all bound objects associated with `session_id` (called on disconnect or re-login).
    pub fn release_session_bindings(&self, session_id: u64) {
        self.bound_objects
            .write()
            .unwrap()
            .retain(|_, v| v.session_id != session_id);
    }

    /// Binds a stateful remote object (`beyonce`, `dogmaIM`, `invbroker`) scoped to `(session_id, character_id)`,
    /// validating both the service allowlist and target `object_id` ownership/location.
    pub fn bind_object(
        &self,
        session: &SessionState,
        service: &str,
        object_id: i64,
    ) -> Result<u64, BridgeRpcError> {
        let char_id = session.character_id.ok_or_else(|| BridgeRpcError {
            code: "CharacterNotSelected".into(),
            message: "Cannot bind object before selecting a character".into(),
        })?;

        let authorized = match service {
            "beyonce" => session.solar_system_id.map(|s| s as i64) == Some(object_id),
            "dogmaIM" => session.ship_id == Some(object_id),
            "invbroker" => {
                session.station_id == Some(object_id) || session.ship_id == Some(object_id)
            }
            _ => false,
        };

        if !authorized {
            return Err(BridgeRpcError {
                code: "BindAuthorizationDenied".into(),
                message: format!(
                    "Service '{service}' cannot be bound to object_id {object_id} for current session state"
                ),
            });
        }

        let bound_id = NEXT_BOUND_ID.fetch_add(1, Ordering::Relaxed);
        self.bound_objects.write().unwrap().insert(
            bound_id,
            BoundObjectEntry {
                bound_id,
                session_id: session.session_id,
                character_id: char_id,
                service: service.to_string(),
                object_id,
            },
        );
        Ok(bound_id)
    }

    /// Verifies that `bound_id` exists and belongs to the caller's `(session_id, character_id)`.
    pub fn verify_bound_object(
        &self,
        session: &SessionState,
        bound_id: u64,
    ) -> Result<BoundObjectEntry, BridgeRpcError> {
        let entry = self
            .bound_objects
            .read()
            .unwrap()
            .get(&bound_id)
            .cloned()
            .ok_or_else(|| BridgeRpcError {
                code: "InvalidBoundObject".into(),
                message: format!("Bound object {bound_id} does not exist"),
            })?;

        if entry.session_id != session.session_id
            || Some(entry.character_id) != session.character_id
        {
            return Err(BridgeRpcError {
                code: "BoundObjectOwnershipViolation".into(),
                message: format!("Bound object {bound_id} is not owned by this session"),
            });
        }
        Ok(entry)
    }

    fn require_auth(session: &SessionState) -> Result<u64, BridgeRpcError> {
        session.user_id.ok_or_else(|| BridgeRpcError {
            code: "Unauthorized".into(),
            message: "Authentication required before calling this service".into(),
        })
    }

    fn require_character(session: &SessionState) -> Result<(u64, i64), BridgeRpcError> {
        let uid = Self::require_auth(session)?;
        let cid = session.character_id.ok_or_else(|| BridgeRpcError {
            code: "CharacterNotSelected".into(),
            message: "Character selection required before calling this service".into(),
        })?;
        Ok((uid, cid))
    }

    fn require_bound_system(
        bound_entry: Option<&BoundObjectEntry>,
        expected_service: &str,
    ) -> Result<SolarSystemId, BridgeRpcError> {
        let entry = bound_entry.ok_or_else(|| BridgeRpcError {
            code: "BoundObjectRequired".into(),
            message: format!("Calls to {expected_service} require a bound object handle"),
        })?;
        if entry.service != expected_service {
            return Err(BridgeRpcError {
                code: "InvalidBoundService".into(),
                message: format!(
                    "Expected bound service {expected_service}, got {}",
                    entry.service
                ),
            });
        }
        Ok(entry.object_id as SolarSystemId)
    }

    /// Dispatches an incoming RPC call `(service, bound_id, method, args, kwargs)` to the appropriate
    /// Rust handler, enforcing authentication and ownership checks.
    #[allow(clippy::too_many_arguments)]
    pub async fn dispatch_rpc(
        &self,
        session: &mut SessionState,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
        caller_trace: Option<&str>,
    ) -> Result<PyRep, BridgeRpcError> {
        info!(
            session_id = session.session_id,
            user_id = ?session.user_id,
            char_id = ?session.character_id,
            service,
            ?bound_id,
            method,
            "Dispatching RPC call"
        );

        let bound_entry = if let Some(bid) = bound_id {
            Some(self.verify_bound_object(session, bid)?)
        } else {
            None
        };

        let effective_service = bound_entry
            .as_ref()
            .map(|b| b.service.as_str())
            .unwrap_or(service);
        if let Some(ref b) = bound_entry {
            info!(
                bound_id = b.bound_id,
                object_id = b.object_id,
                "Resolved bound object target"
            );
        }

        match (effective_service, method) {
            // 1. Public Handshake / Bootstrap Services
            ("machoNet", "GetInitVals") => Ok(PyRep::Tuple(vec![
                PyRep::Dict(vec![
                    (
                        PyRep::String("stationSvc".into()),
                        PyRep::String("station".into()),
                    ),
                    (
                        PyRep::String("charUnboundMgr".into()),
                        PyRep::String("character".into()),
                    ),
                    (
                        PyRep::String("dogmaIM".into()),
                        PyRep::String("dogma".into()),
                    ),
                ]),
                PyRep::Dict(vec![]),
            ])),

            // 2. Authentication Service (`auth.Login`)
            ("auth", "Login") | ("authentication", "Login") => {
                let username = args.first().and_then(|a| a.as_str()).unwrap_or("");
                let token = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
                let acc = self
                    .db
                    .authenticate(username, token)
                    .map_err(|e| BridgeRpcError {
                        code: "AuthenticationFailed".into(),
                        message: e.to_string(),
                    })?;

                // Reset any previous character/location state and bindings on re-authentication
                self.release_session_bindings(session.session_id);
                session.user_id = Some(acc.user_id);
                session.role = acc.role;
                session.character_id = None;
                session.corporation_id = None;
                session.alliance_id = None;
                session.station_id = None;
                session.solar_system_id = None;
                session.constellation_id = None;
                session.region_id = None;
                session.ship_id = None;

                Ok(PyRep::keyval([
                    ("userID", PyRep::Integer(acc.user_id as i64)),
                    ("role", PyRep::Integer(acc.role as i64)),
                ]))
            }

            // 3. Character Selection Services
            ("charUnboundMgr", "GetCharactersToSelect") => {
                let user_id = Self::require_auth(session)?;
                let chars = self.db.get_characters_for_user(user_id);
                let rows = chars
                    .into_iter()
                    .map(|c| {
                        vec![
                            PyRep::Integer(c.character_id),
                            PyRep::String(c.name),
                            PyRep::Integer(c.corporation_id),
                            PyRep::Integer(c.station_id.unwrap_or(0)),
                            PyRep::Integer(c.solar_system_id as i64),
                            PyRep::Real(c.balance.to_wire_f64()),
                            PyRep::Real(c.skill_points),
                        ]
                    })
                    .collect();

                Ok(PyRep::rowset(
                    vec![
                        "characterID",
                        "characterName",
                        "corporationID",
                        "stationID",
                        "solarSystemID",
                        "balance",
                        "skillPoints",
                    ],
                    rows,
                ))
            }

            ("charUnboundMgr", "SelectCharacterID") => {
                let user_id = Self::require_auth(session)?;
                let char_id = match args.first() {
                    Some(PyRep::Integer(id)) => *id,
                    _ => {
                        return Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: "SelectCharacterID requires integer characterID".into(),
                        })
                    }
                };

                let c = self
                    .db
                    .select_character_for_user(user_id, char_id)
                    .map_err(|e| BridgeRpcError {
                        code: "CharacterSelectionDenied".into(),
                        message: e.to_string(),
                    })?;

                self.release_session_bindings(session.session_id);
                session.character_id = Some(c.character_id);
                session.corporation_id = Some(c.corporation_id);
                session.station_id = c.station_id;
                session.solar_system_id = Some(c.solar_system_id);
                session.constellation_id = Some(c.constellation_id);
                session.region_id = Some(c.region_id);
                session.ship_id = Some(c.active_ship_id);

                Ok(PyRep::Bool(true))
            }

            // 4. Authorized Object Binding (`MachoBindObject`)
            (_, "MachoBindObject") => {
                let _ = Self::require_character(session)?;
                let obj_id = match args.first() {
                    Some(PyRep::Integer(id)) => *id,
                    _ => {
                        return Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: "MachoBindObject requires an explicit integer objectID".into(),
                        })
                    }
                };
                let bid = self.bind_object(session, effective_service, obj_id)?;
                Ok(PyRep::Tuple(vec![PyRep::Integer(bid as i64), PyRep::None]))
            }

            // 5. Station & Solar System Lookup + Atomic Undocking
            ("stationSvc", "GetSolarSystem") => {
                let _ = Self::require_auth(session)?;
                let sys_id = match args.first() {
                    Some(PyRep::Integer(id)) => *id as u32,
                    _ => session.solar_system_id.ok_or_else(|| BridgeRpcError {
                        code: "NoSolarSystem".into(),
                        message: "No solarSystemID argument or active session solar system".into(),
                    })?,
                };
                if let Some(sys) = self.sde.solar_systems.get(&sys_id) {
                    Ok(PyRep::keyval([
                        ("solarSystemID", PyRep::Integer(sys.solar_system_id as i64)),
                        ("solarSystemName", PyRep::String(sys.name.clone())),
                        (
                            "constellationID",
                            PyRep::Integer(sys.constellation_id as i64),
                        ),
                        ("regionID", PyRep::Integer(sys.region_id as i64)),
                        ("security", PyRep::Real(sys.security_status)),
                    ]))
                } else {
                    Ok(PyRep::None)
                }
            }

            ("stationSvc", "Undock") => {
                let (user_id, char_id) = Self::require_character(session)?;
                let (station_id, ch, ship) =
                    self.db
                        .undock_character(user_id, char_id)
                        .map_err(|e| BridgeRpcError {
                            code: "UndockFailed".into(),
                            message: e.to_string(),
                        })?;

                // Look up the station's real 3D coordinates in the SDE (plus undock exit offset)
                let spawn_pos = self
                    .sde
                    .solar_systems
                    .get(&ch.solar_system_id)
                    .and_then(|sys| sys.stations.iter().find(|s| s.station_id == station_id))
                    .map(|st| st.position + DVec3::new(1_500.0, 0.0, 0.0))
                    .unwrap_or_else(|| DVec3::new(100_000.0, 0.0, 0.0));

                let ship_type = self.sde.types.get(&ship.type_id);
                let max_vel = ship_type
                    .and_then(|t| t.default_attributes.get(&attr_ids::MAX_VELOCITY))
                    .copied()
                    .unwrap_or(365.0);
                let inertia = ship_type
                    .and_then(|t| t.default_attributes.get(&attr_ids::AGILITY))
                    .copied()
                    .unwrap_or(3.45);
                let mass = ship_type.map(|t| t.mass).unwrap_or(1_067_000.0);
                let radius = ship_type.map(|t| t.radius).unwrap_or(37.0);

                let ball = Ball {
                    id: ship.item_id,
                    type_id: ship.type_id,
                    position: spawn_pos,
                    velocity: DVec3::new(max_vel * 0.5, 0.0, 0.0),
                    max_velocity: max_vel,
                    inertia,
                    mass,
                    radius,
                    speed_fraction: 1.0,
                    mode: MovementMode::GotoDirection {
                        direction: DVec3::X,
                    },
                };

                let sys_handle = self.systems.get_or_spawn(ch.solar_system_id);
                let tick = match sys_handle.spawn_ball(ball).await {
                    Ok(t) => t,
                    Err(e) => {
                        // Compensating rollback so DB never diverges from actor state
                        self.db.rollback_undock(char_id, station_id, ship.item_id);
                        return Err(BridgeRpcError {
                            code: "SystemActorError".into(),
                            message: e,
                        });
                    }
                };

                session.station_id = None;
                let beyonce_bound_id =
                    self.bind_object(session, "beyonce", ch.solar_system_id as i64)?;
                Ok(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(ch.solar_system_id as i64)),
                    ("shipID", PyRep::Integer(ship.item_id)),
                    ("beyonceBoundID", PyRep::Integer(beyonce_bound_id as i64)),
                    ("spawnTick", PyRep::Integer(tick as i64)),
                ]))
            }

            // 6. Space Flight & Destiny Simulation (`beyonce` — requires bound object handle)
            ("beyonce", "CmdGotoDirection") => {
                let (_, _char_id) = Self::require_character(session)?;
                let sys_id = Self::require_bound_system(bound_entry.as_ref(), "beyonce")?;
                let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                })?;
                let dx = extract_f64(args.first()).unwrap_or(1.0);
                let dy = extract_f64(args.get(1)).unwrap_or(0.0);
                let dz = extract_f64(args.get(2)).unwrap_or(0.0);

                let handle = self.systems.get_or_spawn(sys_id);
                let tick = handle
                    .set_movement_mode(
                        ship_id,
                        MovementMode::GotoDirection {
                            direction: DVec3::new(dx, dy, dz),
                        },
                    )
                    .await
                    .map_err(|e| BridgeRpcError {
                        code: "DestinyCommandFailed".into(),
                        message: e,
                    })?;
                Ok(PyRep::Integer(tick as i64))
            }

            ("beyonce", "CmdStop") => {
                let (_, _char_id) = Self::require_character(session)?;
                let sys_id = Self::require_bound_system(bound_entry.as_ref(), "beyonce")?;
                let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                })?;
                let handle = self.systems.get_or_spawn(sys_id);
                let tick = handle
                    .set_movement_mode(ship_id, MovementMode::Stop)
                    .await
                    .map_err(|e| BridgeRpcError {
                        code: "DestinyCommandFailed".into(),
                        message: e,
                    })?;
                Ok(PyRep::Integer(tick as i64))
            }

            ("beyonce", "CmdWarpToPoint") => {
                let (_, _char_id) = Self::require_character(session)?;
                let sys_id = Self::require_bound_system(bound_entry.as_ref(), "beyonce")?;
                let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                })?;
                let x = extract_f64(args.first()).unwrap_or(0.0);
                let y = extract_f64(args.get(1)).unwrap_or(0.0);
                let z = extract_f64(args.get(2)).unwrap_or(0.0);

                let handle = self.systems.get_or_spawn(sys_id);
                let (_, current_ball) = handle
                    .get_ball_snapshot(ship_id)
                    .await
                    .map_err(|e| BridgeRpcError {
                        code: "DestinyCommandFailed".into(),
                        message: e,
                    })?
                    .ok_or_else(|| BridgeRpcError {
                        code: "ShipNotInSpace".into(),
                        message: "Active ship is not spawned in Ballpark".into(),
                    })?;

                let warp_speed = self
                    .sde
                    .types
                    .get(&current_ball.type_id)
                    .and_then(|t| t.default_attributes.get(&attr_ids::WARP_SPEED_MULTIPLIER))
                    .copied()
                    .unwrap_or(3.0);

                let mode =
                    MovementMode::new_warp(current_ball.position, DVec3::new(x, y, z), warp_speed);
                let tick =
                    handle
                        .set_movement_mode(ship_id, mode)
                        .await
                        .map_err(|e| BridgeRpcError {
                            code: "DestinyCommandFailed".into(),
                            message: e,
                        })?;
                Ok(PyRep::Integer(tick as i64))
            }

            ("beyonce", "StepSimulation") => {
                let (_, _char_id) = Self::require_character(session)?;
                let sys_id = Self::require_bound_system(bound_entry.as_ref(), "beyonce")?;
                let dt = extract_f64(args.first()).unwrap_or(1.0);
                let handle = self.systems.get_or_spawn(sys_id);
                let bcast = handle.step_manual(dt).await.map_err(|e| BridgeRpcError {
                    code: "DestinyStepFailed".into(),
                    message: e,
                })?;
                Ok(PyRep::Integer(bcast.tick as i64))
            }

            ("beyonce", "GetBallSnapshot") => {
                let (_, _char_id) = Self::require_character(session)?;
                let sys_id = Self::require_bound_system(bound_entry.as_ref(), "beyonce")?;
                let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                })?;
                let handle = self.systems.get_or_spawn(sys_id);
                if let Some((tick, ball)) =
                    handle
                        .get_ball_snapshot(ship_id)
                        .await
                        .map_err(|e| BridgeRpcError {
                            code: "DestinyQueryFailed".into(),
                            message: e,
                        })?
                {
                    Ok(PyRep::keyval([
                        ("tick", PyRep::Integer(tick as i64)),
                        ("ballID", PyRep::Integer(ball.id)),
                        ("x", PyRep::Real(ball.position.x)),
                        ("y", PyRep::Real(ball.position.y)),
                        ("z", PyRep::Real(ball.position.z)),
                        ("vx", PyRep::Real(ball.velocity.x)),
                        ("vy", PyRep::Real(ball.velocity.y)),
                        ("vz", PyRep::Real(ball.velocity.z)),
                    ]))
                } else {
                    Ok(PyRep::None)
                }
            }

            // 7. Transactional Inventory Operations (`invbroker`)
            ("invbroker", "ListItems") => {
                let (_, char_id) = Self::require_character(session)?;
                let items = self.db.get_items_for_character(char_id);
                let rows = items
                    .into_iter()
                    .map(|i| {
                        vec![
                            PyRep::Integer(i.item_id),
                            PyRep::Integer(i.type_id as i64),
                            PyRep::Integer(i.owner_id),
                            PyRep::Integer(i.location_id),
                            PyRep::Integer(i.flag as i64),
                            PyRep::Integer(i.quantity as i64),
                            PyRep::Integer(i.version as i64),
                        ]
                    })
                    .collect();
                Ok(PyRep::rowset(
                    vec![
                        "itemID",
                        "typeID",
                        "ownerID",
                        "locationID",
                        "flagID",
                        "quantity",
                        "version",
                    ],
                    rows,
                ))
            }

            ("invbroker", "MoveItem") => {
                let (_, char_id) = Self::require_character(session)?;
                let item_id = match args.first() {
                    Some(PyRep::Integer(id)) => *id,
                    _ => {
                        return Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: "MoveItem requires itemID".into(),
                        })
                    }
                };
                let new_loc = match args.get(1) {
                    Some(PyRep::Integer(loc)) => *loc,
                    _ => {
                        return Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: "MoveItem requires destination locationID".into(),
                        })
                    }
                };
                let expected_version = match kwargs.get("expected_version") {
                    Some(PyRep::Integer(v)) => Some(*v as u64),
                    _ => None,
                };
                let idem_key = kwargs.get("idempotency_key").and_then(|v| v.as_str());

                let updated = self
                    .db
                    .move_item_transactional(
                        char_id,
                        item_id,
                        new_loc,
                        InventoryFlag::Cargo,
                        expected_version,
                        idem_key,
                    )
                    .map_err(|e| BridgeRpcError {
                        code: "InventoryMoveFailed".into(),
                        message: e.to_string(),
                    })?;

                Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(updated.item_id)),
                    ("locationID", PyRep::Integer(updated.location_id)),
                    ("flagID", PyRep::Integer(updated.flag as i64)),
                    ("version", PyRep::Integer(updated.version as i64)),
                ]))
            }

            ("config", "GetMultiOwnersEx") | ("config", "GetMultiLocationsEx") => {
                Ok(PyRep::Tuple(vec![PyRep::List(vec![]), PyRep::List(vec![])]))
            }

            _ => {
                warn!(
                    service = effective_service,
                    method,
                    ?args,
                    ?kwargs,
                    caller = caller_trace.unwrap_or("unknown"),
                    "Unimplemented RPC intercepted — returning explicit UnimplementedRPC error"
                );
                Err(BridgeRpcError {
                    code: "UnimplementedRPC".into(),
                    message: format!("{effective_service}.{method} is not yet implemented"),
                })
            }
        }
    }
}

fn extract_f64(rep: Option<&PyRep>) -> Option<f64> {
    match rep {
        Some(PyRep::Real(v)) => Some(*v),
        Some(PyRep::Integer(v)) => Some(*v as f64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_authentication_and_ownership_enforcement() {
        let ctx = ServerContext::new();
        let mut session = SessionState::new(1);

        let unauth_res = ctx
            .dispatch_rpc(
                &mut session,
                "charUnboundMgr",
                None,
                "GetCharactersToSelect",
                &[],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(unauth_res.unwrap_err().code, "Unauthorized");

        let unauth_sel = ctx
            .dispatch_rpc(
                &mut session,
                "charUnboundMgr",
                None,
                "SelectCharacterID",
                &[PyRep::Integer(90_000_001)],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(unauth_sel.unwrap_err().code, "Unauthorized");

        let login_res = ctx
            .dispatch_rpc(
                &mut session,
                "auth",
                None,
                "Login",
                &[
                    PyRep::String("demo".into()),
                    PyRep::String("demo-secret".into()),
                ],
                &HashMap::new(),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(login_res, PyRep::KeyVal(_)));
        assert_eq!(session.user_id, Some(1));

        let cross_account_sel = ctx
            .dispatch_rpc(
                &mut session,
                "charUnboundMgr",
                None,
                "SelectCharacterID",
                &[PyRep::Integer(90_000_002)],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(
            cross_account_sel.unwrap_err().code,
            "CharacterSelectionDenied"
        );

        let valid_sel = ctx
            .dispatch_rpc(
                &mut session,
                "charUnboundMgr",
                None,
                "SelectCharacterID",
                &[PyRep::Integer(90_000_001)],
                &HashMap::new(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(valid_sel, PyRep::Bool(true));
        assert_eq!(session.character_id, Some(90_000_001));

        // Unauthorized object bind (trying to bind beyonce to a system the character isn't in) MUST fail
        let bad_bind = ctx
            .dispatch_rpc(
                &mut session,
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(30_000_999)],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(bad_bind.unwrap_err().code, "BindAuthorizationDenied");
    }

    #[tokio::test]
    async fn test_vertical_slice_undock_double_undock_prevention_and_movement() {
        let ctx = ServerContext::new();
        let mut session = SessionState::new(10);

        ctx.dispatch_rpc(
            &mut session,
            "auth",
            None,
            "Login",
            &[
                PyRep::String("demo".into()),
                PyRep::String("demo-secret".into()),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();

        ctx.dispatch_rpc(
            &mut session,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();

        let undock_res = ctx
            .dispatch_rpc(
                &mut session,
                "stationSvc",
                None,
                "Undock",
                &[],
                &HashMap::new(),
                None,
            )
            .await
            .unwrap();

        // Calling Undock a second time while already in space MUST fail cleanly!
        let double_undock = ctx
            .dispatch_rpc(
                &mut session,
                "stationSvc",
                None,
                "Undock",
                &[],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(double_undock.unwrap_err().code, "UndockFailed");

        let PyRep::KeyVal(kv) = undock_res else {
            panic!("Expected KeyVal from Undock");
        };
        let Some(PyRep::Integer(bound_id)) = kv.get("beyonceBoundID") else {
            panic!("Expected beyonceBoundID");
        };

        // Calling beyonce WITHOUT bound_id MUST be rejected!
        let unbound_call = ctx
            .dispatch_rpc(
                &mut session,
                "beyonce",
                None,
                "CmdGotoDirection",
                &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(unbound_call.unwrap_err().code, "BoundObjectRequired");

        // Issue flight command via bound beyonce object and advance the solar system 1 tick
        ctx.dispatch_rpc(
            &mut session,
            "beyonce",
            Some(*bound_id as u64),
            "CmdGotoDirection",
            &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();

        let sys_handle = ctx.systems.get_or_spawn(30_000_142);
        let tick_broadcast = sys_handle.step_manual(1.0).await.unwrap();
        assert!(tick_broadcast.tick >= 2);

        let snap = ctx
            .dispatch_rpc(
                &mut session,
                "beyonce",
                Some(*bound_id as u64),
                "GetBallSnapshot",
                &[],
                &HashMap::new(),
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(snap_kv) = snap else {
            panic!("Expected KeyVal snapshot");
        };
        let Some(PyRep::Real(vy)) = snap_kv.get("vy") else {
            panic!("Expected vy");
        };
        assert!(
            *vy > 0.0,
            "Ship should have positive Y velocity after stepping!"
        );
    }
}
