//! Stateful object binding and moniker handles (`MachoBindObject`).

use crate::router::context::{BoundObjectEntry, ServerContext, NEXT_BOUND_ID};
use eve_machonet::SessionState;
use eve_proto::BridgeRpcError;
use std::sync::atomic::Ordering;

impl ServerContext {
    /// Registers a disconnect notification channel for an active TCP client session (`session_id`).
    pub fn register_session_closer(
        &self,
        session_id: u64,
        tx: tokio::sync::mpsc::UnboundedSender<(String, String)>,
    ) {
        self.session_closers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id, tx);
    }

    /// Removes the disconnect notification channel for `session_id`.
    pub fn unregister_session_closer(&self, session_id: u64) {
        self.session_closers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session_id);
    }

    /// Actively signals an open TCP session (`session_id`) to send `GPSTransportClosed` and close its socket.
    pub fn disconnect_session(&self, session_id: u64, reason_code: &str, reason: &str) -> bool {
        self.release_session_bindings(session_id);
        let tx = self
            .session_closers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session_id);
        if let Some(sender) = tx {
            sender
                .send((reason_code.to_string(), reason.to_string()))
                .is_ok()
        } else {
            false
        }
    }

    /// Releases all bound objects associated with `session_id` (called on disconnect or re-login).
    pub fn release_session_bindings(&self, session_id: u64) {
        self.bound_objects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, v| v.session_id != session_id);
    }

    /// Releases all bound objects associated with `user_id` (called on ban, kick, or session displacement).
    pub fn release_user_bindings(&self, user_id: u64) {
        self.bound_objects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, v| v.user_id != user_id);
    }

    /// Binds a stateful remote object scoped to `(session_id, user_id, character_id)`.
    pub fn bind_object(
        &self,
        session: &SessionState,
        service: &str,
        object_id: i64,
    ) -> Result<u64, BridgeRpcError> {
        let user_id = Self::require_auth(session)?;
        let char_id = session.character_id.ok_or_else(|| BridgeRpcError {
            code: "CharacterNotSelected".into(),
            message: "Cannot bind object before selecting a character".into(),
        })?;

        let authorized = match service {
            "beyonce" => session.solar_system_id.map(|s| s as i64) == Some(object_id),
            "dogmaIM" | "ship" => {
                session.ship_id == Some(object_id)
                    || session.station_id == Some(object_id)
                    || session.solar_system_id.map(|s| s as i64) == Some(object_id)
            }
            "invbroker" => {
                session.station_id == Some(object_id)
                    || session.structure_id == Some(object_id)
                    || session.ship_id == Some(object_id)
                    || (session.station_id.is_none()
                        && session.structure_id.is_none()
                        && session.solar_system_id.map(|s| s as i64) == Some(object_id))
            }
            "station" | "corpStationMgr" | "repairSvc" | "reprocessingSvc" | "insuranceSvc" => {
                session.station_id == Some(object_id) || session.structure_id == Some(object_id)
            }
            "skillMgr2" | "charMgr" | "jumpCloneSvc" => {
                Some(object_id) == session.character_id || object_id == 0
            }
            "agentMgr" | "keeper" | "planetMgr" | "posMgr" | "fleetObjectHandler"
            | "corpRegistry" | "allianceRegistry" | "warRegistry" | "warStatisticMgr"
            | "trademgr" | "voucher" => object_id > 0,
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
        self.bound_objects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                bound_id,
                BoundObjectEntry {
                    bound_id,
                    session_id: session.session_id,
                    user_id,
                    character_id: char_id,
                    service: service.to_string(),
                    object_id,
                },
            );
        Ok(bound_id)
    }

    /// Verifies that `bound_id` exists and belongs to the caller's `(session_id, user_id, character_id)`.
    pub fn verify_bound_object(
        &self,
        session: &SessionState,
        bound_id: u64,
    ) -> Result<BoundObjectEntry, BridgeRpcError> {
        let entry = self
            .bound_objects
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&bound_id)
            .cloned()
            .ok_or_else(|| BridgeRpcError {
                code: "InvalidBoundObject".into(),
                message: format!("Bound object {bound_id} does not exist"),
            })?;

        if entry.session_id != session.session_id
            || Some(entry.user_id) != session.user_id
            || Some(entry.character_id) != session.character_id
        {
            return Err(BridgeRpcError {
                code: "BoundObjectOwnershipViolation".into(),
                message: format!("Bound object {bound_id} is not owned by this session"),
            });
        }
        Ok(entry)
    }

    /// Validates container ownership, co-location, and 2,500m in-space loot range for `invbroker.GetContainerContents`.
    pub async fn get_container_contents(
        &self,
        session: &SessionState,
        bound_entry: Option<&BoundObjectEntry>,
        char_id: i64,
        container_id: i64,
    ) -> Result<eve_proto::PyRep, BridgeRpcError> {
        use eve_proto::{DBRowDescriptor, PyRep};

        let (owner_id, loc_id, group_id, category_id, rows) = self.db.with_snapshot(|snap| {
            let Some(container) = snap.items.get(&container_id) else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Container item {container_id} not found"),
                });
            };
            let (grp, cat) = self
                .sde
                .types
                .get(&container.type_id)
                .map(|t| (t.group_id, t.category_id))
                .unwrap_or((0, 0));
            let rows: Vec<Vec<PyRep>> = snap
                .items
                .values()
                .filter(|i| i.location_id == container_id)
                .map(|i| {
                    let (g, c) = self
                        .sde
                        .types
                        .get(&i.type_id)
                        .map(|t| (t.group_id as i64, t.category_id as i64))
                        .unwrap_or((0, 0));
                    let qty = if i.is_blueprint_copy {
                        -2
                    } else if i.singleton {
                        -1
                    } else {
                        i.quantity as i64
                    };
                    vec![
                        PyRep::Integer(i.item_id),
                        PyRep::Integer(i.type_id as i64),
                        PyRep::Integer(i.owner_id),
                        PyRep::Integer(i.location_id),
                        PyRep::Integer(i.flag as i64),
                        PyRep::Integer(qty),
                        PyRep::Integer(g),
                        PyRep::Integer(c),
                        PyRep::String(String::new()),
                    ]
                })
                .collect();
            Ok((container.owner_id, container.location_id, grp, cat, rows))
        })?;

        let deny = || BridgeRpcError {
            code: "CantDoThatWithSomeoneElsesStuff".into(),
            message: format!("Container {container_id} is not accessible by character {char_id}"),
        };

        if let Some(b) = bound_entry {
            if b.object_id != container_id && b.object_id != loc_id {
                return Err(deny());
            }
        }

        if owner_id == char_id {
            let co_located_docked_or_ship = session.ship_id == Some(container_id)
                || session.station_id == Some(loc_id)
                || session.structure_id == Some(loc_id)
                || session.ship_id == Some(loc_id);
            if !co_located_docked_or_ship {
                let Some(sys_id) = u32::try_from(loc_id).ok().filter(|s| {
                    session.station_id.is_none()
                        && session.structure_id.is_none()
                        && session.solar_system_id == Some(*s)
                        && self.sde.solar_systems.contains_key(s)
                }) else {
                    return Err(deny());
                };
                let Some(ship_id) = session.ship_id else {
                    return Err(deny());
                };
                let sys_handle = self.systems.get_or_spawn(sys_id);
                let ship_ball = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten();
                let cont_ball = sys_handle
                    .get_ball_snapshot(container_id)
                    .await
                    .ok()
                    .flatten();
                let (Some((_, sb)), Some((_, cb))) = (ship_ball, cont_ball) else {
                    return Err(deny());
                };
                if sb.position.distance(cb.position) > 2_500.0 {
                    return Err(deny());
                }
            }
        } else {
            let is_openable_container =
                category_id != 6 && matches!(group_id, 12 | 186 | 340 | 448 | 649);
            if !is_openable_container {
                return Err(deny());
            }
            let Some(sys_id) = u32::try_from(loc_id).ok().filter(|s| {
                session.station_id.is_none()
                    && session.structure_id.is_none()
                    && session.solar_system_id == Some(*s)
                    && self.sde.solar_systems.contains_key(s)
            }) else {
                return Err(deny());
            };
            let Some(ship_id) = session.ship_id else {
                return Err(deny());
            };
            let sys_handle = self.systems.get_or_spawn(sys_id);
            let ship_ball = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten();
            let cont_ball = sys_handle
                .get_ball_snapshot(container_id)
                .await
                .ok()
                .flatten();
            let (Some((_, sb)), Some((_, cb))) = (ship_ball, cont_ball) else {
                return Err(deny());
            };
            if sb.position.distance(cb.position) > 2_500.0 {
                return Err(deny());
            }
        }

        Ok(DBRowDescriptor::inv_item_header().build_crowset(rows))
    }
}
