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

    fn is_controlling_structure(
        ship_id: Option<i64>,
        station_id: Option<i64>,
        structure_id: Option<i64>,
    ) -> bool {
        station_id.is_none() && structure_id.is_some() && structure_id == ship_id
    }

    fn not_in_space_error() -> BridgeRpcError {
        let msg = "Action is only permitted while in space".to_string();
        BridgeRpcError::with_client_error(
            "NotInSpace",
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        )
    }

    fn is_db_character_docked(&self, char_id: i64, allow_structure_control: bool) -> bool {
        self.db.with_snapshot(|snap| {
            snap.characters.get(&char_id).is_some_and(|ch| {
                ch.station_id.is_some()
                    || ch.structure_id.is_some_and(|st_id| {
                        !(allow_structure_control && ch.active_ship_id == st_id)
                    })
            })
        })
    }

    /// Evaluates whether `session` is docked under the requested DB/structure-control policy.
    ///
    /// Asymmetry between `strict_db_dock` and `allow_structure_control`:
    /// - When `strict_db_dock` is `true`, both DB `station_id` and DB `structure_id` are checked
    ///   (treating `ch.structure_id == Some(ch.active_ship_id)` as docked only when
    ///   `allow_structure_control` is `false`).
    /// - When `strict_db_dock` is `false`, DB `station_id` is skipped so undocked sessions with
    ///   stale DB station fields pass, while `!allow_structure_control` still checks DB
    ///   `ch.structure_id` so structure-control sessions fail closed for ship-only in-space actions.
    fn is_session_docked_with_policy(
        &self,
        session: &SessionState,
        strict_db_dock: bool,
        allow_structure_control: bool,
    ) -> bool {
        if session.station_id.is_some() {
            return true;
        }
        if session.structure_id.is_some()
            && !(allow_structure_control
                && Self::is_controlling_structure(
                    session.ship_id,
                    session.station_id,
                    session.structure_id,
                ))
        {
            return true;
        }
        if let Some(cid) = session.character_id {
            if strict_db_dock {
                return self.is_db_character_docked(cid, allow_structure_control);
            }
            // Non-strict callers skip DB `station_id` checks, but when structure control is
            // disallowed (`!allow_structure_control`), DB `ch.structure_id` still blocks it.
            if !allow_structure_control
                && self.db.with_snapshot(|snap| {
                    snap.characters
                        .get(&cid)
                        .is_some_and(|ch| ch.structure_id.is_some())
                })
            {
                return true;
            }
        }
        false
    }

    /// Returns `true` if `session` indicates the pilot is docked in a station or Upwell structure.
    ///
    /// Active Upwell structure control (`station_id.is_none() && structure_id == ship_id`) counts as
    /// in space rather than docked.
    pub fn is_session_docked(&self, session: &SessionState) -> bool {
        self.is_session_docked_with_policy(session, false, true)
    }

    /// Strictly checks both `session` and persisted character DB fields (`ch.station_id` or
    /// `ch.structure_id != Some(ch.active_ship_id)`) for docked placement even when `session.solar_system_id`
    /// is populated, while permitting active Upwell structure control (`station_id.is_none() && structure_id == ship_id`
    /// and DB `ch.structure_id == Some(ch.active_ship_id)`).
    pub fn is_session_docked_strict(&self, session: &SessionState) -> bool {
        self.is_session_docked_with_policy(session, true, true)
    }

    /// Strictly checks `session` and persisted DB docking with explicit Upwell structure-control policy.
    pub fn is_session_docked_strict_policy(
        &self,
        session: &SessionState,
        allow_structure_control: bool,
    ) -> bool {
        self.is_session_docked_with_policy(session, true, allow_structure_control)
    }

    /// Enforces [`RpcSceneClass::InSpace`] placement on `session`, requiring a selected character,
    /// non-docked session/DB state, and a valid non-zero solar system ID.
    pub fn require_in_space_scene(&self, session: &SessionState) -> Result<(), BridgeRpcError> {
        let _ = Self::require_character(session)?;
        if self.is_session_docked_strict(session)
            || session.solar_system_id.is_none_or(|id| id == 0)
        {
            return Err(Self::not_in_space_error());
        }
        Ok(())
    }

    /// Enforces [`RpcSceneClass::Docked`] placement on `session`, requiring a selected character and
    /// returning the docked station/structure ID or rejecting undocked callers with `"MustBeDocked"`.
    ///
    /// Precondition: when `session.solar_system_id.is_some()`, DB docking fallback is skipped so
    /// undocked in-space sessions with stale DB `station_id`/`structure_id` fields fail closed.
    pub fn require_docked_scene(&self, session: &SessionState) -> Result<i64, BridgeRpcError> {
        let _ = Self::require_character(session)?;
        let session_docked = if Self::is_controlling_structure(
            session.ship_id,
            session.station_id,
            session.structure_id,
        ) {
            None
        } else {
            session.station_id.or(session.structure_id)
        };
        let docked_id = session_docked.or_else(|| {
            // Skip DB fallback when already in space (`solar_system_id.is_some()`) so stale DB
            // docking fields cannot spoof docked placement.
            if session.solar_system_id.is_some() {
                return None;
            }
            session.character_id.and_then(|cid| {
                self.db.with_snapshot(|snap| {
                    snap.characters.get(&cid).and_then(|ch| {
                        ch.station_id
                            .or_else(|| ch.structure_id.filter(|&st_id| ch.active_ship_id != st_id))
                    })
                })
            })
        });
        docked_id.ok_or_else(|| BridgeRpcError::with_client_error(
            "MustBeDocked",
            "Action is only permitted while docked",
            eve_proto::ClientError::user(eve_proto::UserErrorKey::MustBeDocked),
        ))
    }

    /// Canonical undocked + active solar-system guard (`F10` scene-guard consolidation).
    pub fn require_undocked_space_system(
        &self,
        session: &SessionState,
        docked_code: &str,
        docked_msg: &str,
        strict_db_dock: bool,
        allow_structure_control: bool,
    ) -> Result<(i64, SolarSystemId), BridgeRpcError> {
        let (_, char_id) = Self::require_character(session)?;
        if self.is_session_docked_with_policy(session, strict_db_dock, allow_structure_control) {
            return Err(BridgeRpcError::with_client_error(
                docked_code,
                docked_msg,
                eve_proto::ClientError::CustomInfo(docked_msg.to_string()),
            ));
        }
        let sys_id = session.solar_system_id.filter(|&id| id > 0).ok_or_else(|| {
            let msg = "Session has no active solar system".to_string();
            BridgeRpcError::with_client_error(
                "NoSolarSystem",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            )
        })?;
        Ok((char_id, sys_id))
    }

    /// Canonical undocked + active solar-system + active ship guard (`F10` scene-guard consolidation).
    pub fn require_undocked_space_ship(
        &self,
        session: &SessionState,
        docked_code: &str,
        docked_msg: &str,
        strict_db_dock: bool,
        allow_structure_control: bool,
    ) -> Result<(i64, SolarSystemId, i64), BridgeRpcError> {
        let (char_id, sys_id) = self.require_undocked_space_system(
            session,
            docked_code,
            docked_msg,
            strict_db_dock,
            allow_structure_control,
        )?;
        let ship_id = session.ship_id.filter(|&id| id > 0).ok_or_else(|| {
            let msg = "Session has no active ship".to_string();
            BridgeRpcError::with_client_error(
                "NoActiveShip",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            )
        })?;
        Ok((char_id, sys_id, ship_id))
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

