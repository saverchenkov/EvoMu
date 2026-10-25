//! Session authentication and character validation guards.

use crate::router::context::{BoundObjectEntry, ServerContext};
use eve_machonet::SessionState;
use eve_proto::BridgeRpcError;
use eve_sde::SolarSystemId;

impl ServerContext {
    /// Returns true if cluster graceful shutdown is currently in progress.
    pub fn is_shutting_down(&self) -> bool {
        self.cluster_state
            .read()
            .map(|s| s.shutdown_in_progress)
            .unwrap_or(false)
    }

    pub fn require_auth(session: &SessionState) -> Result<u64, BridgeRpcError> {
        session.user_id.ok_or_else(|| BridgeRpcError {
            code: "Unauthorized".into(),
            message: "Authentication required before calling this service".into(),
        })
    }

    pub fn require_gm_role(session: &SessionState) -> Result<u64, BridgeRpcError> {
        let uid = Self::require_auth(session)?;
        if !session.has_gm_role() {
            return Err(BridgeRpcError {
                code: "CustomError".into(),
                message: "GM/Admin role required".into(),
            });
        }
        Ok(uid)
    }

    pub fn require_character(session: &SessionState) -> Result<(u64, i64), BridgeRpcError> {
        let uid = Self::require_auth(session)?;
        let cid = session.character_id.ok_or_else(|| BridgeRpcError {
            code: "CharacterNotSelected".into(),
            message: "Character selection required before calling this service".into(),
        })?;
        Ok((uid, cid))
    }

    pub fn require_bound_system(
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
}
