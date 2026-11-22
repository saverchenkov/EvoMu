//! Session first-RPC legal-fixture census collector (`#1540`).
//!
//! Audits each session's initial world/session state via
//! [`eve_testkit::legal_fixture::check_legal_fixture`] on its first eligible RPC
//! inside a context, aggregates counts per [`ServerContext`] with zero globals
//! (`#1583`), and optionally appends one NDJSON summary line on teardown.

use std::collections::{BTreeMap, HashSet};
use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use eve_machonet::SessionState;
use eve_testkit::legal_fixture::{
    check_legal_fixture, FixtureSessionView, LegalFixtureExpectation, LegalFixtureViolation,
};
use serde::{Deserialize, Serialize};

use super::ServerContext;

const CENSUS_REPORT_ENV: &str = "EVOMU_FIXTURE_CENSUS_REPORT_PATH";
const MAX_CENSUS_SAMPLES: usize = 16;

/// Sampled violation captured during first-RPC fixture census inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureViolationSample {
    pub session_id: u64,
    pub character_id: Option<i64>,
    pub service: String,
    pub method: String,
    pub kind: String,
    pub detail: String,
}

/// Aggregated fixture census report for a single [`ServerContext`] lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureCensusSnapshot {
    pub test_name: String,
    pub sessions_checked: usize,
    pub violation_total: usize,
    pub violations_by_kind: BTreeMap<String, usize>,
    pub samples: Vec<FixtureViolationSample>,
}

#[derive(Debug)]
struct FixtureCensusState {
    test_name: String,
    checked_sessions: HashSet<u64>,
    sessions_checked: usize,
    violation_total: usize,
    violations_by_kind: BTreeMap<String, usize>,
    samples: Vec<FixtureViolationSample>,
    /// `None` reads `EVOMU_FIXTURE_CENSUS_REPORT_PATH`; `Some(None)` disables file writes.
    report_path_override: Option<Option<PathBuf>>,
    reported_to_file: bool,
}

impl FixtureCensusState {
    fn new(test_name: String) -> Self {
        Self {
            test_name,
            checked_sessions: HashSet::new(),
            sessions_checked: 0,
            violation_total: 0,
            violations_by_kind: BTreeMap::new(),
            samples: Vec::new(),
            report_path_override: None,
            reported_to_file: false,
        }
    }

    fn snapshot(&self) -> FixtureCensusSnapshot {
        FixtureCensusSnapshot {
            test_name: self.test_name.clone(),
            sessions_checked: self.sessions_checked,
            violation_total: self.violation_total,
            violations_by_kind: self.violations_by_kind.clone(),
            samples: self.samples.clone(),
        }
    }

    fn resolved_report_path(&self) -> Option<PathBuf> {
        match &self.report_path_override {
            Some(explicit) => explicit.clone(),
            None => std::env::var_os(CENSUS_REPORT_ENV)
                .map(PathBuf::from)
                .filter(|path| !path.as_os_str().is_empty()),
        }
    }
}

/// Per-`ServerContext` collector that runs at most one legal-fixture check per
/// session and flushes an optional NDJSON summary when the last `Arc` drops.
#[derive(Debug)]
pub struct FixtureCensusCollector {
    state: Mutex<FixtureCensusState>,
}

impl FixtureCensusCollector {
    /// Creates a new per-context collector capturing the current thread's test name.
    pub(crate) fn new() -> Self {
        let test_name = std::thread::current()
            .name()
            .filter(|name| !name.is_empty())
            .unwrap_or("unnamed")
            .to_string();
        Self { state: Mutex::new(FixtureCensusState::new(test_name)) }
    }

    /// Overrides (or disables with `None`) the NDJSON report destination for this context.
    pub(crate) fn set_report_path_override(&self, path: Option<PathBuf>) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.report_path_override = Some(path);
        guard.reported_to_file = false;
    }

    /// Returns a point-in-time snapshot of collected fixture violations.
    pub fn snapshot(&self) -> FixtureCensusSnapshot {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).snapshot()
    }

    fn claim_session_for_check(&self, session_id: u64) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .checked_sessions
            .insert(session_id)
    }

    fn record_check_outcome(
        &self,
        session_id: u64,
        character_id: Option<i64>,
        service: &str,
        method: &str,
        violations: &[LegalFixtureViolation],
    ) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.sessions_checked += 1;
        for violation in violations {
            let kind = violation_kind_name(violation);
            guard.violation_total += 1;
            *guard.violations_by_kind.entry(kind.to_string()).or_insert(0) += 1;
            if guard.samples.len() < MAX_CENSUS_SAMPLES {
                guard.samples.push(FixtureViolationSample {
                    session_id,
                    character_id,
                    service: service.to_string(),
                    method: method.to_string(),
                    kind: kind.to_string(),
                    detail: format!("{violation:?}"),
                });
            }
        }
    }
}

impl Drop for FixtureCensusCollector {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        if state.reported_to_file || state.sessions_checked == 0 {
            return;
        }
        let Some(path) = state.resolved_report_path() else {
            return;
        };
        let snapshot = state.snapshot();
        if append_ndjson_report(&path, &snapshot).is_ok() {
            state.reported_to_file = true;
        }
    }
}

fn append_ndjson_report(path: &Path, snapshot: &FixtureCensusSnapshot) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        create_dir_all(parent)?;
    }
    let line = serde_json::to_string(snapshot)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")
}

/// Pre-auth and character-select RPCs are deferred while `character_id` is `None` so
/// `Login -> SelectCharacterID` flows are checked on their first gameplay RPC instead
/// of firing a premature `UnauthenticatedSession` false positive.
fn should_check_session_on_rpc(session: &SessionState, service: &str, method: &str) -> bool {
    if session.character_id.is_some() {
        return true;
    }
    !matches!(
        service,
        "authentication"
            | "cache"
            | "objectCaching"
            | "multiLoginBlocker"
            | "serviceGatewayMgr"
            | "charUnboundMgr"
            | "character"
            | "paperDollServer"
            | "userSvc"
            | "photoUploadSvc"
            | "config"
    ) && !matches!(
        (service, method),
        (
            "machoNet",
            "GetTime"
                | "GetServiceInfo"
                | "NoBlock"
                | "GetGlobalConfig"
                | "ClientHasReleasedTheseObjects"
                | "MachoBindObject"
        ) | ("petitioner", "MayPetition")
    )
}

fn session_to_fixture_view(session: &SessionState, now_unix: u64) -> FixtureSessionView {
    FixtureSessionView {
        session_id: session.session_id,
        user_id: session.user_id,
        character_id: session.character_id,
        corporation_id: session.corporation_id,
        alliance_id: session.alliance_id,
        station_id: session.station_id,
        structure_id: session.structure_id,
        solar_system_id: session.solar_system_id,
        ship_id: session.ship_id,
        corp_role: session.corp_role,
        fleet_id: session.fleet_id,
        role: session.role,
        now_unix: Some(now_unix),
    }
}

/// Maps a [`LegalFixtureViolation`] variant to its canonical name for aggregation.
pub fn violation_kind_name(violation: &LegalFixtureViolation) -> &'static str {
    use LegalFixtureViolation as V;
    match violation {
        V::UnauthenticatedSession { .. } => "UnauthenticatedSession",
        V::MissingOrBannedAccount { .. } => "MissingOrBannedAccount",
        V::MissingCharacter { .. } => "MissingCharacter",
        V::CharacterAccountMismatch { .. } => "CharacterAccountMismatch",
        V::NonOmegaCharacter { .. } => "NonOmegaCharacter",
        V::MissingCorporationId { .. } => "MissingCorporationId",
        V::CharacterCorporationMismatch { .. } => "CharacterCorporationMismatch",
        V::NpcCorpUsedAsPlayerCorp { .. } => "NpcCorpUsedAsPlayerCorp",
        V::NpcCorpCannotGrantPlayerRoles { .. } => "NpcCorpCannotGrantPlayerRoles",
        V::MissingCorporation { .. } => "MissingCorporation",
        V::CharacterNotCorpMember { .. } => "CharacterNotCorpMember",
        V::MissingCorpRoles { .. } => "MissingCorpRoles",
        V::PhantomSessionCorpRoles { .. } => "PhantomSessionCorpRoles",
        V::InvalidSessionLocation { .. } => "InvalidSessionLocation",
        V::ExpectedInSpaceButDocked { .. } => "ExpectedInSpaceButDocked",
        V::ExpectedDockedButInSpace { .. } => "ExpectedDockedButInSpace",
        V::DockLocationMismatch { .. } => "DockLocationMismatch",
        V::MissingActiveShip { .. } => "MissingActiveShip",
        V::InvalidActiveShip { .. } => "InvalidActiveShip",
        V::ShipLocationMismatch { .. } => "ShipLocationMismatch",
        V::ShipMissingFromSpaceBallpark { .. } => "ShipMissingFromSpaceBallpark",
        V::UnownedFittedItem { .. } => "UnownedFittedItem",
        V::DuplicateSlotModule { .. } => "DuplicateSlotModule",
        V::InvalidFittedModule { .. } => "InvalidFittedModule",
        V::MissingRequiredSkill { .. } => "MissingRequiredSkill",
        V::OrphanLoadedCharge { .. } => "OrphanLoadedCharge",
        V::IncompatibleLoadedCharge { .. } => "IncompatibleLoadedCharge",
        V::InvalidLoadedChargeQuantity { .. } => "InvalidLoadedChargeQuantity",
        V::ChargeExceedsModuleCapacity { .. } => "ChargeExceedsModuleCapacity",
        V::FleetMembershipMismatch { .. } => "FleetMembershipMismatch",
    }
}

impl ServerContext {
    /// Checks `session`'s initial fixture state once per context on its first eligible RPC.
    ///
    /// Runs *before* `sync_session(session)` so initial session/DB mismatches (such as a spoofed
    /// `session.corporation_id`) are recorded before `sync_character_session` overwrites them.
    pub(crate) fn record_fixture_census_on_first_rpc(
        &self,
        session: &SessionState,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
    ) {
        let bound_service = bound_id.and_then(|bid| self.owned_bound_service_for_metrics(session, bid));
        let effective_service = bound_service.as_deref().unwrap_or(service);
        if !should_check_session_on_rpc(session, effective_service, method) {
            return;
        }
        if !self.fixture_census.claim_session_for_check(session.session_id) {
            return;
        }
        let view = session_to_fixture_view(session, self.now_unix());
        let expectation = LegalFixtureExpectation::default();
        let violations = self
            .db
            .with_snapshot(|snap| check_legal_fixture(snap, &self.sde, &view, &expectation))
            .err()
            .unwrap_or_default();
        self.fixture_census.record_check_outcome(
            session.session_id,
            session.character_id,
            effective_service,
            method,
            &violations,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::TestContext;
    use eve_machonet::service_roles;
    use eve_proto::{BridgeRpcError, PyRep};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn test_ctx() -> TestContext {
        TestContext::with_sde_and_repository(
            Arc::new(eve_sde::SdeRegistry::with_canonical_seed()),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
    }

    async fn dispatch_no_kw(
        ctx: &TestContext,
        session: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(session, service, None, method, args, &HashMap::new(), None)
            .await
    }

    fn seeded_test_character_session(
        ctx: &TestContext,
        session_id: u64,
        account_name: &str,
        char_name: &str,
    ) -> SessionState {
        let user_id = ctx.db.provision_player_account(account_name, "pass");
        let created = ctx
            .db
            .create_character(user_id, char_name, 1, 1, 11, 17, &ctx.sde)
            .expect("create test character");
        let mut session = SessionState::new(session_id);
        session.user_id = Some(user_id);
        session.character_id = Some(created.character_id);
        session.corporation_id = Some(created.corporation_id);
        session.station_id = created.station_id;
        session.structure_id = created.structure_id;
        session.solar_system_id = Some(created.solar_system_id);
        session.ship_id = Some(created.active_ship_id);
        session.role = service_roles::ROLE_PLAYER_DEFAULT;
        session
    }

    #[tokio::test]
    async fn first_rpc_census_records_violation_before_sync_and_is_idempotent() {
        let ctx = test_ctx();
        ctx.set_fixture_census_report_path(None);
        let mut session = seeded_test_character_session(&ctx, 11, "census_u1", "Census Pilot One");
        // Diverge corporation_id in memory so only a pre-sync check observes the mismatch.
        session.corporation_id = Some(98_000_999);

        let res = dispatch_no_kw(&ctx, &mut session, "beyonce", "GetFormations", &[]).await;
        assert!(res.is_ok(), "census must never fail the dispatched RPC: {res:?}");

        let snap1 = ctx.fixture_census_snapshot();
        assert_eq!(snap1.sessions_checked, 1);
        assert_eq!(snap1.violations_by_kind.get("CharacterCorporationMismatch"), Some(&1));

        // Second RPC on the same session must not re-run the census check.
        assert!(dispatch_no_kw(&ctx, &mut session, "beyonce", "GetFormations", &[]).await.is_ok());
        assert_eq!(ctx.fixture_census_snapshot(), snap1);
    }

    #[tokio::test]
    async fn collector_aggregates_violations_across_sessions_and_flushes_ndjson_on_drop() {
        let report_path = std::env::temp_dir().join(format!(
            "evomu_fixture_census_test_{}_{:?}.ndjson",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&report_path);

        {
            let ctx = test_ctx().with_fixture_census_report_path(Some(report_path.clone()));
            let mut unauth = SessionState::new(21);
            let _ = dispatch_no_kw(&ctx, &mut unauth, "ship", "GetShipID", &[]).await;

            let mut s2 = seeded_test_character_session(&ctx, 22, "census_u2", "Census Pilot Two");
            s2.corporation_id = Some(98_000_888);
            let _ = dispatch_no_kw(&ctx, &mut s2, "beyonce", "GetFormations", &[]).await;

            let snap = ctx.fixture_census_snapshot();
            assert_eq!(snap.sessions_checked, 2);
            assert_eq!(snap.violations_by_kind.get("UnauthenticatedSession"), Some(&1));
            assert_eq!(snap.violations_by_kind.get("CharacterCorporationMismatch"), Some(&1));
            assert!(snap.violation_total >= 2);
        }

        let content = std::fs::read_to_string(&report_path).expect("ndjson report written on drop");
        let _ = std::fs::remove_file(&report_path);
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1, "expected one NDJSON record on context drop");
        let parsed: FixtureCensusSnapshot = serde_json::from_str(lines[0]).expect("valid NDJSON");
        assert_eq!(parsed.sessions_checked, 2);
        assert_eq!(parsed.violations_by_kind.get("UnauthenticatedSession"), Some(&1));
        assert_eq!(parsed.violations_by_kind.get("CharacterCorporationMismatch"), Some(&1));
    }

    #[tokio::test]
    async fn login_and_character_select_do_not_trigger_false_positive_before_gameplay() {
        let ctx = test_ctx();
        ctx.set_fixture_census_report_path(None);
        let user_id = ctx.db.provision_player_account("census_login_u", "pw");
        let created = ctx
            .db
            .create_character(user_id, "Census Clean Pilot", 1, 1, 11, 17, &ctx.sde)
            .expect("create clean character");

        let mut session = SessionState::new(33);
        let login = dispatch_no_kw(
            &ctx,
            &mut session,
            "authentication",
            "Login",
            &[PyRep::String("census_login_u".into()), PyRep::String("pw".into())],
        )
        .await;
        assert!(login.is_ok(), "login failed: {login:?}");

        let select = dispatch_no_kw(
            &ctx,
            &mut session,
            "charUnboundMgr",
            "SelectCharacterID",
            &[PyRep::Integer(created.character_id)],
        )
        .await;
        assert!(select.is_ok(), "SelectCharacterID failed: {select:?}");

        assert!(dispatch_no_kw(&ctx, &mut session, "beyonce", "GetFormations", &[]).await.is_ok());
        let snap = ctx.fixture_census_snapshot();
        assert_eq!(snap.sessions_checked, 1);
        assert_eq!(
            snap.violation_total, 0,
            "clean Login -> SelectCharacterID -> gameplay had violations: {:?}",
            snap.samples
        );
    }
}
