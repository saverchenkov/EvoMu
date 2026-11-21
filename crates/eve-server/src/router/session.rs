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

    /// Canonical GM privilege bitmask across router and services (`ROLE_GML | ROLE_GMH | ROLE_GMS | ROLE_ADMIN | ROLE_WORLDMOD`).
    pub const ROLE_GM_CANONICAL_MASK: u64 = eve_machonet::service_roles::ROLE_GM_MASK;

    /// Canonical GM mask extended with `ROLE_PROGRAMMER` for alert telemetry management.
    pub const ROLE_GM_PROGRAMMER_MASK: u64 =
        Self::ROLE_GM_CANONICAL_MASK | eve_machonet::service_roles::ROLE_PROGRAMMER;

    /// Canonical GM mask extended with `ROLE_QA` for QA-permitted starter/testing options.
    pub const ROLE_GM_QA_MASK: u64 =
        Self::ROLE_GM_CANONICAL_MASK | eve_machonet::service_roles::ROLE_QA;

    /// Returns `true` when `role_bits` satisfies at least one bit in `required_mask`.
    pub fn has_gm_role_mask(role_bits: u64, required_mask: u64) -> bool {
        (role_bits & required_mask) != 0
    }

    /// Returns `true` when `session` holds at least one canonical GM privilege bit (`ROLE_GM_CANONICAL_MASK`).
    pub fn has_canonical_gm_role(session: &SessionState) -> bool {
        Self::has_gm_role_mask(session.role, Self::ROLE_GM_CANONICAL_MASK)
    }

    pub fn require_auth(session: &SessionState) -> Result<u64, BridgeRpcError> {
        session.user_id.ok_or_else(|| BridgeRpcError {
            code: "Unauthorized".into(),
            message: "Authentication required before calling this service".into(),
            client_error: None,
        })
    }

    pub fn require_gm_role(session: &SessionState) -> Result<u64, BridgeRpcError> {
        let uid = Self::require_auth(session)?;
        if !Self::has_canonical_gm_role(session) {
            return Err(BridgeRpcError {
                code: "CustomError".into(),
                message: "GM/Admin role required".into(),
                client_error: None,
            });
        }
        Ok(uid)
    }

    pub fn require_character(session: &SessionState) -> Result<(u64, i64), BridgeRpcError> {
        let uid = Self::require_auth(session)?;
        let cid = session.character_id.ok_or_else(|| BridgeRpcError {
            code: "CharacterNotSelected".into(),
            message: "Character selection required before calling this service".into(),
            client_error: None,
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
            client_error: None,
        })?;
        if entry.service != expected_service {
            return Err(BridgeRpcError {
                code: "InvalidBoundService".into(),
                message: format!(
                    "Expected bound service {expected_service}, got {}",
                    entry.service
                ),
                client_error: None,
            });
        }
        Ok(entry.object_id as SolarSystemId)
    }
}

#[cfg(test)]
mod f8_gm_mask_tests {
    use crate::router::ServerContext;
    use eve_machonet::{service_roles, SessionState};

    #[test]
    fn canonical_gm_mask_enforces_exact_privileges() {
        assert_eq!(ServerContext::ROLE_GM_CANONICAL_MASK, service_roles::ROLE_GM_MASK);
        assert_eq!(
            ServerContext::ROLE_GM_PROGRAMMER_MASK,
            service_roles::ROLE_GM_MASK | service_roles::ROLE_PROGRAMMER
        );
        assert_eq!(
            ServerContext::ROLE_GM_QA_MASK,
            service_roles::ROLE_GM_MASK | service_roles::ROLE_QA
        );

        let mut sess = SessionState::default();
        sess.user_id = Some(42);
        for blocked in [
            0,
            service_roles::ROLE_PLAYER_DEFAULT,
            service_roles::ROLE_PLAYER_DEFAULT | service_roles::ROLE_QA,
            service_roles::ROLE_PLAYER_DEFAULT | service_roles::ROLE_PROGRAMMER,
        ] {
            sess.role = blocked;
            assert!(!ServerContext::has_canonical_gm_role(&sess));
            assert!(ServerContext::require_gm_role(&sess).is_err());
        }
        for allowed in [
            service_roles::ROLE_GML,
            service_roles::ROLE_GMH,
            service_roles::ROLE_GMS,
            service_roles::ROLE_ADMIN,
            service_roles::ROLE_WORLDMOD,
        ] {
            sess.role = service_roles::ROLE_PLAYER_DEFAULT | allowed;
            assert!(ServerContext::has_canonical_gm_role(&sess));
            assert_eq!(ServerContext::require_gm_role(&sess).unwrap(), 42);
        }
        assert!(ServerContext::has_gm_role_mask(
            service_roles::ROLE_QA,
            ServerContext::ROLE_GM_QA_MASK
        ));
        assert!(ServerContext::has_gm_role_mask(
            service_roles::ROLE_PROGRAMMER,
            ServerContext::ROLE_GM_PROGRAMMER_MASK
        ));
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;

