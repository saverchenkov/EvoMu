//! Stateful object binding and moniker handles (`MachoBindObject`).

use crate::router::context::{BoundObjectEntry, ServerContext, NEXT_BOUND_ID};
use eve_machonet::SessionState;
use eve_proto::BridgeRpcError;
use std::sync::atomic::Ordering;

impl ServerContext {
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
}
