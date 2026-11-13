//! In-process Wire-Path Client Harness for Integration Tests (`C-2`, `#926`).
//!
//! Ensures integration tests observe the exact client-transport pipeline:
//! 1. Calls pass through the server's socket-loop handler (`dispatch_rpc(..., Some("machoNet:26000"))`,
//!    reply normalization, bound-object wire encoding, notification suppression and SDE normalization,
//!    `SessionChangeNotification` enrichment, `ErrorResponse` mapping, and `blue.marshal` framing).
//! 2. Every RPC reply and emitted notification passing through [`ClientHarness`] is automatically
//!    validated against the strict `#912` (`C-1`) expectations in [`crate::client_contract`]; an
//!    unratcheted wrong reply or notification shape fails the calling test immediately.
//! 3. Raw `ctx.dispatch_rpc(..., None)` call sites in `crates/eve-server/tests/it/` are tracked on
//!    a strictly falling ratchet ([`RawDispatchRatchet`]).

use crate::client_contract::{
    validate_notification_contract, validate_rpc_reply_for_call, ClientContractRatchet,
    LoadedClientContracts, SemanticLookupContext,
};
use crate::shadow_client::{ShadowClient, ShadowDivergence};
use eve_proto::PyRep;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

/// Mapped V24 client `ErrorResponse` returned when an RPC fails on the wire path (`C-2`).
#[derive(Debug, Clone, PartialEq)]
pub struct ClientErrorResponse {
    pub code: String,
    pub message: String,
    /// Exact payload tuple written in the wire `ErrorResponse` packet by
    /// `macho_listener::map_call_req_error_body`.
    pub wire_body: PyRep,
}

/// Decoded payload of an outbound frame emitted by the server socket loop (`C-2`).
#[derive(Debug, Clone, PartialEq)]
pub enum ClientWireFrameKind {
    CallRsp {
        call_id: u64,
        reply: PyRep,
    },
    ErrorResponse {
        call_id: u64,
        body: PyRep,
    },
    SessionChange {
        packet: PyRep,
    },
    DoDestinyUpdate {
        epoch: u32,
        stamp: u32,
        has_set_state: bool,
        updates: PyRep,
        wait_for_bubble: bool,
    },
    Notification {
        event: String,
        args: Vec<PyRep>,
    },
}

/// Single ordered outbound frame received by [`ClientHarness`], pairing its typed representation
/// with the exact serialized `MachoPacket` `PyRep` written onto the wire.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientWireFrame {
    pub kind: ClientWireFrameKind,
    pub wire_packet: PyRep,
}

/// Raw transport exchange returned by [`ClientWireTransport::send_wire_call`].
#[derive(Debug, Clone)]
pub struct ClientWireExchange {
    pub rpc_result: Result<PyRep, ClientErrorResponse>,
    pub frames: Vec<ClientWireFrame>,
    pub marshal_errors: Vec<String>,
}

/// Validation policy applied by [`ClientHarness`] to every RPC reply and emitted notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContractValidationMode {
    /// Fails the calling test on any `#912` reply or notification violation not explicitly covered
    /// by `client_contract_ratchet.json` (`default`).
    #[default]
    Ratcheted,
    /// Fails the calling test on ANY `#912` reply or notification violation, ignoring ratchet allowlists.
    Strict,
    /// Collects all violations in [`ClientCallOutput::violations`] without panicking.
    ReportOnly,
}

/// Full structured output returned by [`ClientHarness::call_exchange`].
#[derive(Debug, Clone)]
pub struct ClientCallOutput {
    pub result: Result<PyRep, ClientErrorResponse>,
    pub frames: Vec<ClientWireFrame>,
    pub notifications: Vec<(String, Vec<PyRep>)>,
    pub session_change_packets: Vec<PyRep>,
    pub destiny_batches: Vec<PyRep>,
    pub violations: Vec<String>,
}

/// Backend transport executing a single `CallReq` through the server socket-loop wire path.
pub trait ClientWireTransport {
    fn send_wire_call<'a>(
        &'a mut self,
        service: &'a str,
        bound_id: Option<u64>,
        method: &'a str,
        args: &'a [PyRep],
        kwargs: &'a HashMap<String, PyRep>,
    ) -> Pin<Box<dyn Future<Output = ClientWireExchange> + Send + 'a>>;

    /// Optional SDE + WorldRepository lookup context used to run full semantic validation
    /// (`evetypes`, `cfg.eveowners`, `cfg.evelocations`, FILETIME) on replies and notifications.
    fn semantic_lookup(&self) -> Option<SemanticLookupContext<'_>> {
        None
    }
}

/// Wire-path client harness wrapping a [`ClientWireTransport`] and enforcing strict `#912` (`C-1`)
/// reply and notification contracts on every call (`C-2`, `#926`).
pub struct ClientHarness<T> {
    transport: T,
    mode: ContractValidationMode,
    contracts: &'static LoadedClientContracts,
    buffered_notifications: Vec<(String, Vec<PyRep>)>,
    buffered_frames: Vec<ClientWireFrame>,
    recorded_commits: Vec<(String, u64)>,
    multi_commit_ratchet: Option<MultiCommitRpcRatchet>,
    shadow: Option<ShadowClient>,
    shadow_enforce: bool,
    bound_inv_containers: HashMap<u64, i64>,
}

impl<T: ClientWireTransport> ClientHarness<T> {
    /// Creates a new harness over `transport` using [`ContractValidationMode::Ratcheted`].
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            mode: ContractValidationMode::Ratcheted,
            contracts: ClientContractRatchet::load_default_cached(),
            buffered_notifications: Vec::new(),
            buffered_frames: Vec::new(),
            recorded_commits: Vec::new(),
            multi_commit_ratchet: None,
            shadow: None,
            shadow_enforce: false,
            bound_inv_containers: HashMap::new(),
        }
    }

    /// Enables or disables automatic [`ShadowClient`] convergence checks after every wire call (`G-05`).
    pub fn with_shadow_enforcement(mut self, enabled: bool) -> Self {
        self.shadow_enforce = enabled;
        self
    }

    /// Toggles automatic [`ShadowClient`] convergence enforcement after every wire call.
    pub fn set_shadow_enforcement(&mut self, enabled: bool) {
        self.shadow_enforce = enabled;
    }

    /// Attaches a fresh wire-only [`ShadowClient`] for `char_id` (seeded with SDE metadata if available).
    pub fn attach_shadow(&mut self, char_id: i64) {
        let mut sc = ShadowClient::new(char_id);
        if let Some(lookup) = self.transport.semantic_lookup() {
            sc.seed_sde(lookup.sde);
        }
        self.shadow = Some(sc);
    }

    /// Returns a shared reference to the attached [`ShadowClient`], if initialized.
    pub fn shadow(&self) -> Option<&ShadowClient> {
        self.shadow.as_ref()
    }

    /// Returns a mutable reference to the attached [`ShadowClient`], if initialized.
    pub fn shadow_mut(&mut self) -> Option<&mut ShadowClient> {
        self.shadow.as_mut()
    }

    /// Feeds out-of-band server simulation tick notifications into the attached [`ShadowClient`]
    /// and verifies convergence when shadow enforcement is enabled (`G-05`).
    pub fn apply_shadow_tick_notifications<'a, I>(&mut self, notifications: I)
    where
        I: IntoIterator<Item = (&'a str, &'a [PyRep])>,
    {
        if let Some(ref mut shadow) = self.shadow {
            shadow.apply_notifications(notifications);
        }
        if self.shadow_enforce {
            self.assert_shadow_converged();
        }
    }

    /// Asserts that the attached [`ShadowClient`] has no recorded divergences and matches world state.
    pub fn assert_shadow_converged(&self) {
        let Some(ref shadow) = self.shadow else {
            return;
        };
        assert!(
            shadow.recorded_divergences.is_empty(),
            "ShadowClient recorded divergences for char {}: {:?}",
            shadow.char_id,
            shadow.recorded_divergences
        );
        if let Some(lookup) = self.transport.semantic_lookup() {
            shadow.assert_converged_with_world(lookup.db, Some(lookup.sde));
        }
    }

    /// Returns all currently detected [`ShadowDivergence`]s (recorded + world diff).
    pub fn shadow_divergences(&self) -> Vec<ShadowDivergence> {
        let Some(ref shadow) = self.shadow else {
            return Vec::new();
        };
        let mut out = shadow.recorded_divergences.clone();
        if let Some(lookup) = self.transport.semantic_lookup() {
            out.extend(shadow.compare_all_against_world(lookup.db, Some(lookup.sde)));
        }
        out
    }

    /// Enables live enforcement of a falling [`MultiCommitRpcRatchet`] after every RPC call.
    pub fn with_multi_commit_ratchet(mut self, ratchet: MultiCommitRpcRatchet) -> Self {
        self.multi_commit_ratchet = Some(ratchet);
        self
    }

    /// Returns all `(service.method, commits)` pairs observed on this harness in dispatch order.
    pub fn recorded_commits(&self) -> &[(String, u64)] {
        &self.recorded_commits
    }

    /// Clears recorded `(service.method, commits)` entries.
    pub fn clear_recorded_commits(&mut self) {
        self.recorded_commits.clear();
    }

    /// Validates all recorded `(service.method, commits)` entries against `ratchet`.
    pub fn check_multi_commit_ratchet(&self, ratchet: &MultiCommitRpcRatchet) -> Result<(), String> {
        ratchet.assert_ratchet(&self.recorded_commits)
    }

    /// Configures how contract violations are handled (`Ratcheted`, `Strict`, or `ReportOnly`).
    pub fn with_mode(mut self, mode: ContractValidationMode) -> Self {
        self.mode = mode;
        self
    }

    /// Returns a shared reference to the underlying transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Returns a mutable reference to the underlying transport.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Consumes the harness and returns the underlying transport.
    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Drains all notifications received across calls on this harness.
    pub fn drain_notifications(&mut self) -> Vec<(String, Vec<PyRep>)> {
        std::mem::take(&mut self.buffered_notifications)
    }

    /// Drains all wire frames received across calls on this harness.
    pub fn drain_frames(&mut self) -> Vec<ClientWireFrame> {
        std::mem::take(&mut self.buffered_frames)
    }

    /// Dispatches `service.method` over the full wire transport path, validates the reply and every
    /// emitted notification against `#912` client expectations, and returns all emitted wire artifacts.
    ///
    /// Panics if an unratcheted contract violation or marshal round-trip failure occurs (unless
    /// [`ContractValidationMode::ReportOnly`] is set).
    pub async fn call_exchange(
        &mut self,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
    ) -> ClientCallOutput {
        let commits_before = self
            .transport
            .semantic_lookup()
            .map(|l| l.db.tx_commit_count());
        let exchange = self
            .transport
            .send_wire_call(service, bound_id, method, args, kwargs)
            .await;
        let lookup = self.transport.semantic_lookup();
        let tx_commits = match (commits_before, lookup.as_ref().map(|l| l.db.tx_commit_count())) {
            (Some(before), Some(after)) => after.saturating_sub(before),
            _ => 0,
        };
        let rpc_name = format!("{service}.{method}");
        self.recorded_commits.push((rpc_name.clone(), tx_commits));
        if let Some(ref ratchet) = self.multi_commit_ratchet {
            ratchet
                .assert_call(&rpc_name, tx_commits)
                .unwrap_or_else(|e| panic!("{e}"));
        }

        let mut violations = Vec::new();
        for err in &exchange.marshal_errors {
            violations.push(format!("wire:marshal: {err}"));
        }

        if let Ok(ref reply) = exchange.rpc_result {
            let outcome = validate_rpc_reply_for_call(
                lookup.as_ref(),
                &self.contracts.expectations,
                service,
                method,
                args,
                reply,
            );
            for v in outcome.violations {
                let keep = match self.mode {
                    ContractValidationMode::Ratcheted => {
                        !self.contracts.is_shape_violation_allowed(&v)
                    }
                    ContractValidationMode::Strict | ContractValidationMode::ReportOnly => true,
                };
                if keep {
                    violations.push(v);
                }
            }
        }

        let notify_map = self
            .contracts
            .expectations
            .get("notify")
            .and_then(Value::as_object);
        let mut notifications = Vec::new();
        let mut session_change_packets = Vec::new();
        let mut destiny_batches = Vec::new();
        for frame in &exchange.frames {
            match &frame.kind {
                ClientWireFrameKind::Notification { event, args: n_args } => {
                    if let Some(map) = notify_map {
                        for v in validate_notification_contract(
                            lookup.as_ref(),
                            map,
                            event,
                            n_args,
                        ) {
                            let keep = match self.mode {
                                ContractValidationMode::Ratcheted => {
                                    !self.contracts.is_notify_violation_allowed(&v)
                                }
                                ContractValidationMode::Strict
                                | ContractValidationMode::ReportOnly => true,
                            };
                            if keep {
                                violations.push(v);
                            }
                        }
                    }
                    notifications.push((event.clone(), n_args.clone()));
                }
                ClientWireFrameKind::SessionChange { packet } => {
                    session_change_packets.push(packet.clone());
                }
                ClientWireFrameKind::DoDestinyUpdate { updates, .. } => {
                    destiny_batches.push(updates.clone());
                }
                ClientWireFrameKind::CallRsp { .. }
                | ClientWireFrameKind::ErrorResponse { .. } => {}
            }
        }

        self.buffered_notifications.extend(notifications.iter().cloned());
        self.buffered_frames.extend(exchange.frames.iter().cloned());

        if service == "charUnboundMgr"
            && method == "SelectCharacterID"
            && exchange.rpc_result.is_ok()
        {
            if let Some(cid) = args.first().and_then(|v| v.as_i64()) {
                let mut sc = ShadowClient::new(cid);
                if let Some(ref l) = lookup {
                    sc.seed_sde(l.sde);
                }
                self.shadow = Some(sc);
            }
        }
        if service == "invbroker" && method == "MachoBindObject" {
            if let Ok(ref reply) = exchange.rpc_result {
                if let Some(bid) = extract_bound_id_from_wire_rep(reply) {
                    if let Some(cid) = args.first().and_then(|v| v.as_i64()) {
                        self.bound_inv_containers.insert(bid, cid);
                    }
                }
            }
        }
        if let Some(ref mut shadow) = self.shadow {
            if let Some(ref l) = lookup {
                if shadow.module_states.known_effect_names.is_empty() {
                    shadow.seed_sde(l.sde);
                }
            }
            for frame in &exchange.frames {
                match &frame.kind {
                    ClientWireFrameKind::Notification { event, args: n_args } => {
                        shadow.apply_notification(event, n_args);
                    }
                    ClientWireFrameKind::SessionChange { packet } => {
                        shadow.apply_notification(
                            "SessionChangeNotification",
                            std::slice::from_ref(packet),
                        );
                    }
                    ClientWireFrameKind::DoDestinyUpdate { updates, .. } => {
                        shadow.apply_notification(
                            "DoDestinyUpdate",
                            std::slice::from_ref(updates),
                        );
                    }
                    ClientWireFrameKind::CallRsp { .. }
                    | ClientWireFrameKind::ErrorResponse { .. } => {}
                }
            }
            if let Ok(ref reply) = exchange.rpc_result {
                if service == "dogmaIM" && method == "GetAllInfo" {
                    shadow.sync_from_get_all_info(reply);
                } else if service == "account" && method == "GetCashBalance" && args.is_empty() {
                    shadow.sync_from_wallet_balance(reply);
                } else if service == "invbroker" && method == "List" {
                    let container_id = bound_id
                        .and_then(|bid| self.bound_inv_containers.get(&bid).copied())
                        .or_else(|| {
                            let rows = match reply {
                                PyRep::ObjectEx { list, .. } | PyRep::List(list) => list.as_slice(),
                                _ => &[],
                            };
                            rows.iter()
                                .find_map(crate::shadow_client::ShadowItem::from_pyrep)
                                .map(|i| i.location_id)
                        });
                    if let Some(cid) = container_id {
                        shadow.sync_from_inventory_list(cid, reply);
                    }
                }
            }
            if self.shadow_enforce {
                for div in &shadow.recorded_divergences {
                    violations.push(format!("shadow:recorded:{div}"));
                }
                if let Some(ref l) = lookup {
                    for div in shadow.compare_all_against_world(l.db, Some(l.sde)) {
                        violations.push(format!("shadow:world_diff:{div}"));
                    }
                }
            }
        }

        if !violations.is_empty() && self.mode != ContractValidationMode::ReportOnly {
            panic!(
                "ClientHarness wire contract violation on {service}.{method}:\n  - {}",
                violations.join("\n  - ")
            );
        }

        ClientCallOutput {
            result: exchange.rpc_result,
            frames: exchange.frames,
            notifications,
            session_change_packets,
            destiny_batches,
            violations,
        }
    }

    /// Dispatches `service.method`, validates the reply and notifications, and returns `Result`.
    pub async fn call(
        &mut self,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
    ) -> Result<PyRep, ClientErrorResponse> {
        self.call_exchange(service, bound_id, method, args, kwargs)
            .await
            .result
    }

    /// Dispatches `service.method`, validates the reply and notifications, and unwraps `Ok(PyRep)`.
    pub async fn call_ok(
        &mut self,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
    ) -> PyRep {
        self.call_exchange(service, bound_id, method, args, kwargs)
            .await
            .result
            .unwrap_or_else(|e| panic!("{service}.{method} failed: {}: {}", e.code, e.message))
    }
}

/// Extracts the server `bound_id` from either the V24 client wire bound-object wrapper
/// (`SubStruct(SubStream((b"N=<node>:<bound_id>", ref_id)))` or `(bound_rep, sub_result)`),
/// a legacy plain `Integer`, or a `{boundID: ...}` dictionary/KeyVal (`C-2`).
pub fn extract_bound_id_from_wire_rep(rep: &PyRep) -> Option<u64> {
    match rep {
        PyRep::Integer(id) if *id >= 0 => Some(*id as u64),
        PyRep::SubStruct(inner) | PyRep::SubStream(inner) => extract_bound_id_from_wire_rep(inner),
        PyRep::Tuple(items) => {
            if items.len() == 2 {
                if let Some(oid_str) = items[0].as_str() {
                    if let Some((_, tail)) = oid_str.rsplit_once(':') {
                        if let Ok(bid) = tail.parse::<u64>() {
                            return Some(bid);
                        }
                    }
                }
            }
            items.first().and_then(extract_bound_id_from_wire_rep)
        }
        _ => rep
            .dict_get("boundID")
            .and_then(PyRep::as_i64)
            .and_then(|id| u64::try_from(id).ok()),
    }
}

/// Scan result counting raw `ctx.dispatch_rpc(..., None)` call sites in integration tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawDispatchScanReport {
    pub raw_none_calls: usize,
    pub files_with_raw_none: usize,
    pub total_dispatch_rpc_calls: usize,
    pub macho_net_calls: usize,
    pub per_file_raw_none: HashMap<String, usize>,
}

/// Falling ratchet limiting RPCs whose execution commits more than one database transaction
/// (`RFC-2 §3.2`, `#915`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiCommitRpcRatchet {
    pub version: u32,
    pub allowed_multi_commit_rpcs: BTreeMap<String, u64>,
}

impl MultiCommitRpcRatchet {
    /// Returns the repository path to `crates/eve-testkit/ratchets/multi_commit_rpc_ratchet.json`.
    pub fn default_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ratchets")
            .join("multi_commit_rpc_ratchet.json")
    }

    /// Loads `multi_commit_rpc_ratchet.json` from its default path.
    pub fn load_default() -> Result<Self, String> {
        Self::load_from_path(&Self::default_path())
    }

    /// Loads a [`MultiCommitRpcRatchet`] from `path`.
    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))
    }

    /// Validates a single `(service.method, commits)` observation against the falling ratchet.
    pub fn assert_call(&self, rpc_name: &str, commits: u64) -> Result<(), String> {
        if commits <= 1 {
            return Ok(());
        }
        let allowed = self
            .allowed_multi_commit_rpcs
            .get(rpc_name)
            .copied()
            .unwrap_or(1);
        if commits > allowed {
            return Err(format!(
                "Multi-commit RPC ratchet violation on {rpc_name}: committed {commits} transactions (max allowed: {allowed}). Consolidate handler writes into a single WorldRepository::tx."
            ));
        }
        Ok(())
    }

    /// Validates all recorded `(service.method, commits)` pairs against this ratchet.
    pub fn assert_ratchet(&self, recorded: &[(String, u64)]) -> Result<(), String> {
        for (rpc_name, commits) in recorded {
            self.assert_call(rpc_name, *commits)?;
        }
        Ok(())
    }
}

/// Falling ratchet limiting direct unrewritten `ctx.dispatch_rpc(..., None)` calls under
/// `crates/eve-server/tests/it/` (`C-2`, `#926`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawDispatchRatchet {
    pub version: u32,
    pub max_raw_dispatch_calls: usize,
    pub max_files_with_raw_dispatch: usize,
}

impl RawDispatchRatchet {
    /// Returns the repository path to `crates/eve-testkit/ratchets/raw_dispatch_ratchet.json`.
    pub fn default_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ratchets")
            .join("raw_dispatch_ratchet.json")
    }

    /// Returns the repository path to `crates/eve-server/tests/it`.
    pub fn default_it_tests_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../eve-server/tests/it")
    }

    /// Loads `raw_dispatch_ratchet.json` from its default path.
    pub fn load_default() -> Result<Self, String> {
        Self::load_from_path(&Self::default_path())
    }

    /// Loads a [`RawDispatchRatchet`] from `path`.
    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))
    }

    /// Scans `crates/eve-server/tests/it` and asserts that raw `None` calls have not grown above
    /// the ratchet ceiling.
    pub fn verify_default(&self) -> Result<RawDispatchScanReport, String> {
        let report = scan_raw_dispatch_calls_in_dir(&Self::default_it_tests_dir())?;
        self.assert_ratchet(&report)?;
        Ok(report)
    }

    /// Enforces that both total `dispatch_rpc(..., None)` calls and distinct files remain at or
    /// below the falling ratchet ceiling.
    pub fn assert_ratchet(&self, report: &RawDispatchScanReport) -> Result<(), String> {
        if report.raw_none_calls > self.max_raw_dispatch_calls {
            return Err(format!(
                "Raw ctx.dispatch_rpc(..., None) count regressed above falling ratchet: found {}, max_raw_dispatch_calls={}. Route new/modified calls through ClientHarness instead of None.",
                report.raw_none_calls, self.max_raw_dispatch_calls
            ));
        }
        if report.files_with_raw_none > self.max_files_with_raw_dispatch {
            return Err(format!(
                "Files containing ctx.dispatch_rpc(..., None) regressed above ratchet: found {}, max_files_with_raw_dispatch={}.",
                report.files_with_raw_none, self.max_files_with_raw_dispatch
            ));
        }
        Ok(())
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Replaces comments, strings, raw strings, and char literals with spaces while preserving
/// newlines and all syntactic punctuation (`(`, `)`, `,`, `.dispatch_rpc`) outside literals.
pub(crate) fn strip_rust_comments_and_literals(src: &str) -> String {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;

    while i < n {
        if b[i] == b'/' && i + 1 < n && b[i + 1] == b'/' {
            while i < n && b[i] != b'\n' {
                out.push(b' ');
                i += 1;
            }
        } else if b[i] == b'/' && i + 1 < n && b[i + 1] == b'*' {
            let mut depth = 1usize;
            out.push(b' ');
            out.push(b' ');
            i += 2;
            while i < n && depth > 0 {
                if i + 1 < n && b[i] == b'/' && b[i + 1] == b'*' {
                    depth += 1;
                    out.push(b' ');
                    out.push(b' ');
                    i += 2;
                } else if i + 1 < n && b[i] == b'*' && b[i + 1] == b'/' {
                    depth -= 1;
                    out.push(b' ');
                    out.push(b' ');
                    i += 2;
                } else {
                    out.push(if b[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
            }
        } else if b[i] == b'r'
            && (i == 0
                || !is_ident_byte(b[i - 1])
                || (b[i - 1] == b'b' && (i == 1 || !is_ident_byte(b[i - 2]))))
        {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while j < n && b[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < n && b[j] == b'"' {
                for _ in i..=j {
                    out.push(b' ');
                }
                i = j + 1;
                while i < n {
                    if b[i] == b'"'
                        && i + hashes < n
                        && b[i + 1..=i + hashes].iter().all(|&c| c == b'#')
                    {
                        for _ in 0..=hashes {
                            out.push(b' ');
                        }
                        i += 1 + hashes;
                        break;
                    }
                    out.push(if b[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
            } else {
                out.push(b[i]);
                i += 1;
            }
        } else if b[i] == b'"' {
            out.push(b' ');
            i += 1;
            while i < n {
                let ch = b[i];
                out.push(if ch == b'\n' { b'\n' } else { b' ' });
                i += 1;
                if ch == b'\\' && i < n {
                    let esc = b[i];
                    out.push(if esc == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                    continue;
                }
                if ch == b'"' {
                    break;
                }
            }
        } else if b[i] == b'\'' {
            if i + 2 < n && b[i + 1] == b'\\' {
                let mut k = i + 2;
                let limit = (i + 10).min(n);
                while k < limit && b[k] != b'\'' && b[k] != b'\n' {
                    k += 1;
                }
                if k < limit && b[k] == b'\'' {
                    for _ in i..=k {
                        out.push(b' ');
                    }
                    i = k + 1;
                    continue;
                }
            } else if i + 2 < n && b[i + 2] == b'\'' && b[i + 1] != b'\n' {
                out.extend_from_slice(b"   ");
                i += 3;
                continue;
            }
            out.push(b[i]);
            i += 1;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }

    String::from_utf8(out).expect("ASCII comment/literal masking preserves UTF-8")
}

/// Recursively collects `.rs` file paths under `dir`.
fn collect_rust_files_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("failed to read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("failed reading dir entry: {e}"))?;
        let p = entry.path();
        if p.is_dir() {
            collect_rust_files_recursive(&p, out)?;
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(p);
        }
    }
    Ok(())
}

/// Recursively scans `.rs` files in `dir` for `.dispatch_rpc(..., <caller_trace>)` invocations.
pub fn scan_raw_dispatch_calls_in_dir(dir: &Path) -> Result<RawDispatchScanReport, String> {
    let mut paths = Vec::new();
    collect_rust_files_recursive(dir, &mut paths)?;
    paths.sort();

    let mut report = RawDispatchScanReport::default();
    let needle = ".dispatch_rpc";

    for path in paths {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let clean = strip_rust_comments_and_literals(&raw);
        let bytes = clean.as_bytes();
        let mut idx = 0usize;
        let mut file_none = 0usize;

        while let Some(rel) = clean[idx..].find(needle) {
            let after_needle = idx + rel + needle.len();
            let mut open = after_needle;
            while open < bytes.len() && bytes[open].is_ascii_whitespace() {
                open += 1;
            }
            if open >= bytes.len() || bytes[open] != b'(' {
                idx = after_needle;
                continue;
            }
            report.total_dispatch_rpc_calls += 1;
            let mut pos = open + 1;
            let mut depth = 1usize;
            let mut last_comma = pos;
            let mut prev_comma = pos;
            while pos < bytes.len() && depth > 0 {
                match bytes[pos] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    b',' if depth == 1 => {
                        prev_comma = last_comma;
                        last_comma = pos + 1;
                    }
                    _ => {}
                }
                pos += 1;
            }
            let tail = clean[last_comma..pos].trim();
            let last_arg = if tail.is_empty() && last_comma > open + 1 {
                clean[prev_comma..last_comma - 1].trim()
            } else {
                tail
            };
            if last_arg == "None" {
                file_none += 1;
                report.raw_none_calls += 1;
            } else if raw[last_comma..pos].contains("machoNet:26000") {
                report.macho_net_calls += 1;
            }
            idx = pos.saturating_add(1);
        }

        if file_none > 0 {
            report.files_with_raw_none += 1;
            let rel_key = path
                .strip_prefix(dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            report.per_file_raw_none.insert(rel_key, file_none);
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockTransport {
        next_exchange: Box<dyn FnMut(&str, &str, &[PyRep]) -> ClientWireExchange + Send>,
    }

    impl ClientWireTransport for MockTransport {
        fn send_wire_call<'a>(
            &'a mut self,
            service: &'a str,
            _bound_id: Option<u64>,
            method: &'a str,
            args: &'a [PyRep],
            _kwargs: &'a HashMap<String, PyRep>,
        ) -> Pin<Box<dyn Future<Output = ClientWireExchange> + Send + 'a>> {
            let ex = (self.next_exchange)(service, method, args);
            Box::pin(async move { ex })
        }
    }

    #[test]
    fn extract_bound_id_handles_wire_substruct_tuples_and_legacy_forms() {
        let substruct = PyRep::SubStruct(Box::new(PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::ascii_bytes("N=26000:42"),
            PyRep::Integer(1_335_000_000_000_000_00),
        ])))));
        assert_eq!(extract_bound_id_from_wire_rep(&substruct), Some(42));
        assert_eq!(
            extract_bound_id_from_wire_rep(&PyRep::Tuple(vec![substruct, PyRep::None])),
            Some(42)
        );
        assert_eq!(extract_bound_id_from_wire_rep(&PyRep::Integer(99)), Some(99));
        assert_eq!(
            extract_bound_id_from_wire_rep(&PyRep::keyval([("boundID", PyRep::Integer(7))])),
            Some(7)
        );
    }

    #[tokio::test]
    #[should_panic(expected = "ClientHarness wire contract violation on stationSvc.GetAllianceSystems")]
    async fn harness_fails_test_when_handler_returns_wrong_reply_shape() {
        let transport = MockTransport {
            next_exchange: Box::new(|_svc, _m, _args| {
                // `stationSvc.GetAllianceSystems` expects iterable/dict entries; returning None must fail!
                let bad = PyRep::None;
                ClientWireExchange {
                    rpc_result: Ok(bad.clone()),
                    frames: vec![ClientWireFrame {
                        kind: ClientWireFrameKind::CallRsp {
                            call_id: 1,
                            reply: bad,
                        },
                        wire_packet: PyRep::None,
                    }],
                    marshal_errors: Vec::new(),
                }
            }),
        };
        let mut harness = ClientHarness::new(transport);
        let _ = harness
            .call("stationSvc", None, "GetAllianceSystems", &[], &HashMap::new())
            .await;
    }

    #[tokio::test]
    #[should_panic(expected = "ClientHarness wire contract violation")]
    async fn harness_fails_test_when_notification_violates_client_contract() {
        let contracts = ClientContractRatchet::load_default_cached();
        let bad_ev = contracts.expectations["notify"]
            .as_object()
            .expect("notify map")
            .iter()
            .find(|(k, _)| {
                *k != "OnSessionChanged"
                    && !validate_notification_contract(
                        None,
                        contracts.expectations["notify"].as_object().unwrap(),
                        k,
                        &[],
                    )
                    .is_empty()
            })
            .map(|(k, _)| k.clone())
            .expect("notification requiring >= 1 arg");

        let transport = MockTransport {
            next_exchange: Box::new(move |_svc, _m, _args| ClientWireExchange {
                rpc_result: Ok(PyRep::None),
                frames: vec![ClientWireFrame {
                    kind: ClientWireFrameKind::Notification {
                        event: bad_ev.clone(),
                        args: Vec::new(),
                    },
                    wire_packet: PyRep::None,
                }],
                marshal_errors: Vec::new(),
            }),
        };
        let mut harness = ClientHarness::new(transport);
        let _ = harness
            .call("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(1)], &HashMap::new())
            .await;
    }

    #[test]
    fn raw_dispatch_ratchet_is_strictly_below_baseline_and_passes() {
        let ratchet = RawDispatchRatchet::load_default().expect("load raw_dispatch_ratchet.json");
        let report = ratchet
            .verify_default()
            .expect("raw dispatch ratchet must pass with common.rs migrated");
        assert!(
            report.per_file_raw_none.get("common.rs").copied().unwrap_or(0) == 0,
            "tests/it/common.rs must contain 0 raw dispatch_rpc(..., None) calls"
        );
    }

    #[test]
    fn raw_dispatch_scanner_recurses_into_subdirectories() {
        let tmp = std::env::temp_dir().join(format!(
            "evomu_raw_dispatch_scan_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let nested = tmp.join("nested").join("deep");
        std::fs::create_dir_all(&nested).expect("create temp nested dir");
        std::fs::write(
            tmp.join("top.rs"),
            "fn a() { ctx.dispatch_rpc(&mut s, \"svc\", None, \"M\", &[], &m, None); }\n",
        )
        .unwrap();
        std::fs::write(
            nested.join("inner.rs"),
            "fn b() {\n  ctx.dispatch_rpc(&mut s, \"svc\", None, \"M\", &[], &m, None);\n  ctx.dispatch_rpc(&mut s, \"svc\", None, \"M\", &[], &m, None);\n}\n",
        )
        .unwrap();

        let report = scan_raw_dispatch_calls_in_dir(&tmp).expect("recursive scan");
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(report.raw_none_calls, 3);
        assert_eq!(report.files_with_raw_none, 2);
        assert_eq!(report.per_file_raw_none.get("top.rs").copied(), Some(1));
        assert_eq!(
            report.per_file_raw_none.get("nested/deep/inner.rs").copied(),
            Some(2)
        );
    }

    #[tokio::test]
    async fn harness_records_tx_commits_and_enforces_multi_commit_ratchet() {
        struct DbMockTransport {
            db: eve_db::WorldRepository,
            sde: eve_sde::SdeRegistry,
            commits_to_run: u64,
            commit_seq: u64,
        }

        impl ClientWireTransport for DbMockTransport {
            fn send_wire_call<'a>(
                &'a mut self,
                _service: &'a str,
                _bound_id: Option<u64>,
                _method: &'a str,
                _args: &'a [PyRep],
                _kwargs: &'a HashMap<String, PyRep>,
            ) -> Pin<Box<dyn Future<Output = ClientWireExchange> + Send + 'a>> {
                for _idx in 0..self.commits_to_run {
                    self.commit_seq += 1;
                    let key = format!("harness_commit_{}", self.commit_seq);
                    self.db.with_mut_snapshot(|snap| {
                        snap.service_state_blobs.insert(key, "ok".into());
                    });
                }
                let ok_rep = PyRep::Dict(Vec::new());
                Box::pin(async move {
                    ClientWireExchange {
                        rpc_result: Ok(ok_rep.clone()),
                        frames: vec![ClientWireFrame {
                            kind: ClientWireFrameKind::CallRsp {
                                call_id: 1,
                                reply: ok_rep,
                            },
                            wire_packet: PyRep::None,
                        }],
                        marshal_errors: Vec::new(),
                    }
                })
            }

            fn semantic_lookup(&self) -> Option<SemanticLookupContext<'_>> {
                Some(SemanticLookupContext {
                    sde: &self.sde,
                    db: &self.db,
                })
            }
        }

        let ratchet = MultiCommitRpcRatchet::load_default().expect("load multi_commit_rpc_ratchet");
        let transport = DbMockTransport {
            db: eve_db::WorldRepository::with_demo_seed(),
            sde: eve_sde::SdeRegistry::with_canonical_seed(),
            commits_to_run: 1,
            commit_seq: 0,
        };
        let mut harness = ClientHarness::new(transport);
        let _ = harness
            .call("stationSvc", None, "GetAllianceSystems", &[], &HashMap::new())
            .await;
        harness.transport_mut().commits_to_run = 0;
        let _ = harness
            .call("stationSvc", None, "GetAllianceSystems", &[], &HashMap::new())
            .await;
        assert_eq!(
            harness.recorded_commits(),
            &[
                ("stationSvc.GetAllianceSystems".to_string(), 1),
                ("stationSvc.GetAllianceSystems".to_string(), 0),
            ]
        );
        harness
            .check_multi_commit_ratchet(&ratchet)
            .expect("single-commit RPC passes multi-commit ratchet");

        harness.transport_mut().commits_to_run = 2;
        let _ = harness
            .call("stationSvc", None, "GetAllianceSystems", &[], &HashMap::new())
            .await;
        assert_eq!(
            harness.recorded_commits().last(),
            Some(&("stationSvc.GetAllianceSystems".to_string(), 2))
        );
        let err = harness
            .check_multi_commit_ratchet(&ratchet)
            .expect_err("2-commit unratcheted RPC must fail ratchet");
        assert!(err.contains("committed 2 transactions"));

        let mut allowlisted = ratchet;
        allowlisted
            .allowed_multi_commit_rpcs
            .insert("stationSvc.GetAllianceSystems".to_string(), 2);
        harness
            .check_multi_commit_ratchet(&allowlisted)
            .expect("allowlisted 2-commit RPC within ceiling passes");
    }

    #[tokio::test]
    async fn harness_wires_shadow_client_from_select_char_and_tick_hook() {
        struct ShadowWireMock {
            db: eve_db::WorldRepository,
            sde: eve_sde::SdeRegistry,
        }

        impl ClientWireTransport for ShadowWireMock {
            fn send_wire_call<'a>(
                &'a mut self,
                service: &'a str,
                _bound_id: Option<u64>,
                method: &'a str,
                args: &'a [PyRep],
                _kwargs: &'a HashMap<String, PyRep>,
            ) -> Pin<Box<dyn Future<Output = ClientWireExchange> + Send + 'a>> {
                let rep = if service == "charUnboundMgr" && method == "SelectCharacterID" {
                    PyRep::None
                } else if service == "account" && method == "GetCashBalance" {
                    let bal = self.db.with_snapshot(|s| {
                        s.characters.get(&2119000001).map(|c| c.balance.to_wire_f64()).unwrap_or(0.0)
                    });
                    PyRep::Real(bal)
                } else {
                    let _ = args;
                    PyRep::Dict(Vec::new())
                };
                Box::pin(async move {
                    ClientWireExchange {
                        rpc_result: Ok(rep.clone()),
                        frames: vec![ClientWireFrame {
                            kind: ClientWireFrameKind::CallRsp {
                                call_id: 1,
                                reply: rep,
                            },
                            wire_packet: PyRep::None,
                        }],
                        marshal_errors: Vec::new(),
                    }
                })
            }

            fn semantic_lookup(&self) -> Option<SemanticLookupContext<'_>> {
                Some(SemanticLookupContext {
                    sde: &self.sde,
                    db: &self.db,
                })
            }
        }

        let db = eve_db::WorldRepository::with_demo_seed();
        let pilot_id = db.with_snapshot(|s| *s.characters.keys().next().unwrap());
        let sde = eve_sde::SdeRegistry::with_canonical_seed();
        let mut harness = ClientHarness::new(ShadowWireMock { db, sde })
            .with_mode(ContractValidationMode::ReportOnly)
            .with_shadow_enforcement(false);
        assert!(harness.shadow().is_none());

        let _ = harness
            .call(
                "charUnboundMgr",
                None,
                "SelectCharacterID",
                &[PyRep::Integer(pilot_id)],
                &HashMap::new(),
            )
            .await;
        assert_eq!(harness.shadow().map(|s| s.char_id), Some(pilot_id));

        let _ = harness
            .call("account", None, "GetCashBalance", &[], &HashMap::new())
            .await;
        assert!(harness.shadow().unwrap().wallet.personal_balance_isk.is_some());

        // Out-of-band tick notification updates shadow wallet too.
        let tick_args = [
            PyRep::String("cash".into()),
            PyRep::Integer(pilot_id),
            PyRep::Real(12_345_678.0),
        ];
        harness.apply_shadow_tick_notifications([("OnAccountChange", tick_args.as_slice())]);
        assert_eq!(
            harness.shadow().unwrap().wallet.personal_balance_isk,
            Some(12_345_678.0)
        );
    }
}
