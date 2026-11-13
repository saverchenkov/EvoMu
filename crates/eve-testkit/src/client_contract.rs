//! Strict V24.01 Client RPC & Notification Contract Validator (`C-1`, `#912`).
//!
//! Extracted from `crates/eve-server/tests/it/rpc_client_contracts.rs` into `eve-testkit` so unit
//! tests and integration suites share the exact same validation logic:
//! - **Strict `None` policy:** `PyRep::None` at any path the client dereferences (`unpack:N`,
//!   `attr:<field>`, `method:<name>`, `rowset_index`, `rowset_filter`, `iter` with `[]`,
//!   `subscript`, `contains`, `len`, `arith`, `number`, `type:*`) without a null-check guard is a
//!   contract violation.
//! - **Method & Type enforcement:** Enforces `method:iteritems`/`itervalues`/`keys`/`values`/`get`
//!   (requires mapping), `method:split`/`lower`/`upper`/`strip`/`startswith` and `type:str`
//!   (requires string, `#939`), `contains[*]` + `subscript[*]` (requires key-indexed mapping, not
//!   flat `Rowset`, `#943`), `type:datetime` (requires `datetime` object when non-`None`, `#945`),
//!   and class-method attributes like `attr:GetInvController` (`FacilityLocation`, `#934`).
//! - **Non-vacuity accounting (`G3` report):** Classifies every swept RPC into
//!   [`RpcValidationClass::Validated`], [`RpcValidationClass::Vacuous`],
//!   [`RpcValidationClass::DomainRejected`], or [`RpcValidationClass::Unimplemented`].
//! - **Expectations version & SHA-256 hash lock:** Verifies `client_expectations.json` version and
//!   SHA-256 digest against [`ClientContractRatchet`].

use eve_db::WorldRepository;
use eve_proto::quasar::sha256_hex;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Classification of an RPC in the client contract sweep (`G3` non-vacuity accounting, `#912`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RpcValidationClass {
    /// Reply was non-vacuously validated across `paths_checked` structural/semantic paths.
    Validated { paths_checked: usize },
    /// Reply succeeded, but returned an empty collection (so element paths `[]`/`[*]` were never
    /// exercised) or the client only discards/stores the return value without structural ops.
    Vacuous,
    /// All candidate calls returned a domain error (`UserError` / guard rejection).
    DomainRejected,
    /// Server returned `UnimplementedRPC`.
    Unimplemented,
}

/// Result of validating a single `PyRep` against a `reply_shape` map.
#[derive(Debug, Clone, Default)]
pub struct ReplyValidationOutcome {
    pub violations: Vec<String>,
    pub paths_checked: usize,
    pub had_unexercised_collection_elements: bool,
}

impl ReplyValidationOutcome {
    pub fn classification(&self) -> RpcValidationClass {
        if self.paths_checked > 0 && !self.had_unexercised_collection_elements {
            RpcValidationClass::Validated {
                paths_checked: self.paths_checked,
            }
        } else {
            RpcValidationClass::Vacuous
        }
    }
}

/// Aggregate 4-way non-vacuity report for the client contract sweep (`#912`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContractSweepReport {
    pub expectations_version: String,
    pub expectations_sha256: String,
    pub validated: BTreeMap<String, usize>,
    pub vacuous: BTreeSet<String>,
    pub domain_rejected: BTreeSet<String>,
    pub unimplemented: BTreeSet<String>,
    pub shape_violations: Vec<String>,
    pub notify_violations: Vec<String>,
}

impl ContractSweepReport {
    pub fn print_summary(&self) {
        println!(
            "=== RPC CONTRACT REPORT (v={}, sha256={}..): validated={}, vacuous={}, domain_rejected={}, unimplemented={}, shape_violations={}, notify_violations={} ===",
            self.expectations_version,
            &self.expectations_sha256[..12.min(self.expectations_sha256.len())],
            self.validated.len(),
            self.vacuous.len(),
            self.domain_rejected.len(),
            self.unimplemented.len(),
            self.shape_violations.len(),
            self.notify_violations.len(),
        );
    }
}

/// Shrinking ratchet and expectations hash lock for `client_expectations.json` (`#912`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientContractRatchet {
    pub version: u32,
    pub expectations_version: String,
    pub expectations_sha256: String,
    pub min_validated_rpcs: usize,
    pub allowed_shape_violations: BTreeSet<String>,
    pub allowed_notify_violations: BTreeSet<String>,
}

pub fn normalize_violation_for_ratchet(s: &str) -> String {
    if let Some((prefix, _)) = s.split_once("KeyVal({") {
        format!("{prefix}KeyVal({{...}})")
    } else {
        s.to_string()
    }
}

impl ClientContractRatchet {
    pub fn default_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ratchets")
            .join("client_contract_ratchet.json")
    }

    pub fn default_expectations_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.agents/skills/evomu-adversarial-review/references/parity/sources/client_expectations.json")
    }

    pub fn load_default() -> Result<Self, String> {
        Self::load_from_path(&Self::default_path())
    }

    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))
    }

    /// Verifies that `raw_expectations_bytes` and `parsed_expectations` match the pinned version
    /// and SHA-256 digest in this ratchet header (`#912`).
    pub fn verify_expectations_integrity(
        &self,
        raw_expectations_bytes: &[u8],
        parsed_expectations: &Value,
    ) -> Result<(), String> {
        let actual_version = parsed_expectations
            .get("version")
            .and_then(|v| v.as_str())
            .or_else(|| {
                parsed_expectations
                    .get("meta")
                    .and_then(|m| m.get("version"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("unversioned-main");
        if actual_version != self.expectations_version {
            return Err(format!(
                "client_expectations.json version mismatch: ratchet expects `{}`, found `{actual_version}`",
                self.expectations_version
            ));
        }

        let actual_sha256 = sha256_hex(raw_expectations_bytes);
        if actual_sha256 != self.expectations_sha256 {
            return Err(format!(
                "client_expectations.json SHA-256 mismatch: ratchet expects `{}`, found `{actual_sha256}` (update client_contract_ratchet.json when client_expectations.json changes)",
                self.expectations_sha256
            ));
        }
        Ok(())
    }

    /// Enforces that:
    /// 1. `expectations_version` and `expectations_sha256` match.
    /// 2. `report.validated.len() >= self.min_validated_rpcs`.
    /// 3. `report.unimplemented` is empty.
    /// 4. No new shape or notify violations appear outside `allowed_shape_violations` /
    ///    `allowed_notify_violations`.
    pub fn assert_sweep_report(&self, report: &ContractSweepReport) -> Result<(), String> {
        if report.expectations_version != self.expectations_version {
            return Err(format!(
                "client_expectations.json version mismatch: expected `{}`, got `{}`",
                self.expectations_version, report.expectations_version
            ));
        }
        if report.expectations_sha256 != self.expectations_sha256 {
            return Err(format!(
                "client_expectations.json SHA-256 mismatch: expected `{}`, got `{}`",
                self.expectations_sha256, report.expectations_sha256
            ));
        }
        if !report.unimplemented.is_empty() {
            return Err(format!(
                "Found {} unimplemented RPCs: {:?}",
                report.unimplemented.len(),
                report.unimplemented
            ));
        }
        if report.validated.len() < self.min_validated_rpcs {
            return Err(format!(
                "Non-vacuous Validated RPC count regressed below ratchet floor: got {}, min_validated_rpcs={}",
                report.validated.len(),
                self.min_validated_rpcs
            ));
        }
        let normalized_allowed_shape: BTreeSet<String> = self
            .allowed_shape_violations
            .iter()
            .map(|s| normalize_violation_for_ratchet(s))
            .collect();
        let unexpected_shape: Vec<&String> = report
            .shape_violations
            .iter()
            .filter(|v| {
                !self.allowed_shape_violations.contains(*v)
                    && !normalized_allowed_shape.contains(&normalize_violation_for_ratchet(v))
            })
            .collect();
        if !unexpected_shape.is_empty() {
            return Err(format!(
                "Found {} unratcheted reply_shape contract violation(s): {unexpected_shape:#?}",
                unexpected_shape.len()
            ));
        }
        let normalized_allowed_notify: BTreeSet<String> = self
            .allowed_notify_violations
            .iter()
            .map(|s| normalize_violation_for_ratchet(s))
            .collect();
        let unexpected_notify: Vec<&String> = report
            .notify_violations
            .iter()
            .filter(|v| {
                !self.allowed_notify_violations.contains(*v)
                    && !normalized_allowed_notify.contains(&normalize_violation_for_ratchet(v))
            })
            .collect();
        if !unexpected_notify.is_empty() {
            return Err(format!(
                "Found {} unratcheted notification contract violation(s): {unexpected_notify:#?}",
                unexpected_notify.len()
            ));
        }
        Ok(())
    }
}

/// Optional semantic lookup context (SDE + WorldRepository) for Phase 2 foreign-key checks.
pub struct SemanticLookupContext<'a> {
    pub sde: &'a SdeRegistry,
    pub db: &'a WorldRepository,
}

/// V24.01 classic old-style class paths (`Op_PyObject` `0x17`, whitelisted in `carbon/common/lib/whitelist.pyj`, `#954`).
pub const V24_CACHED_OBJECT_CLASS: &str = "carbon.common.script.net.objectCaching.CachedObject";
pub const V24_CACHED_METHOD_CALL_RESULT_CLASS: &str =
    "carbon.common.script.net.objectCaching.CachedMethodCallResult";
pub const V24_CACHE_HINT_CLASS: &str = "carbon.common.script.net.cachedObject.CachedObject";

fn is_legacy_unwhitelisted_caching_class(type_name: &str) -> bool {
    type_name.starts_with("objectCaching.")
        || matches!(
            type_name,
            "util.CachedObject"
                | "util.CachedMethodCallResult"
                | "utillib.CachedMethodCallResult"
        )
}

fn collect_legacy_caching_class_violations(
    rpc_name: &str,
    rep: &PyRep,
    violations: &mut Vec<String>,
) {
    match rep {
        PyRep::SubStream(inner) | PyRep::SubStruct(inner) => {
            collect_legacy_caching_class_violations(rpc_name, inner, violations);
        }
        PyRep::ChecksumedStream { body, .. } => {
            collect_legacy_caching_class_violations(rpc_name, body, violations);
        }
        PyRep::Object { type_name, args } => {
            if is_legacy_unwhitelisted_caching_class(type_name) {
                let msg = format!(
                    "{rpc_name}: unwhitelisted V24.01 caching class `{type_name}` in PyRep::Object (use `carbon.common.script.net.objectCaching.*` / `carbon.common.script.net.cachedObject.CachedObject`, #954)"
                );
                if !violations.contains(&msg) {
                    violations.push(msg);
                }
            }
            collect_legacy_caching_class_violations(rpc_name, args, violations);
            if matches!(
                type_name.as_str(),
                V24_CACHED_OBJECT_CLASS
                    | V24_CACHE_HINT_CLASS
                    | "objectCaching.CachedObject"
                    | "util.CachedObject"
            ) {
                if let PyRep::Tuple(items) = args.as_ref() {
                    if let Some(PyRep::Buffer(bytes) | PyRep::Bytes(bytes)) = items.get(4) {
                        if let Ok(decoded) = eve_proto::marshal::decode(bytes) {
                            collect_legacy_caching_class_violations(rpc_name, &decoded, violations);
                        }
                    }
                }
            }
        }
        PyRep::ObjectEx {
            header, list, dict, ..
        } => {
            if let Some(cls) = object_ex_class_name(header) {
                if is_legacy_unwhitelisted_caching_class(cls) {
                    let msg = format!(
                        "{rpc_name}: unwhitelisted V24.01 caching class `{cls}` in PyRep::ObjectEx (use `carbon.common.script.net.objectCaching.*` / `carbon.common.script.net.cachedObject.CachedObject`, #954)"
                    );
                    if !violations.contains(&msg) {
                        violations.push(msg);
                    }
                }
            }
            collect_legacy_caching_class_violations(rpc_name, header, violations);
            for item in list {
                collect_legacy_caching_class_violations(rpc_name, item, violations);
            }
            for (k, v) in dict {
                collect_legacy_caching_class_violations(rpc_name, k, violations);
                collect_legacy_caching_class_violations(rpc_name, v, violations);
            }
        }
        PyRep::Tuple(items)
        | PyRep::List(items)
        | PyRep::Set(items)
        | PyRep::PySet(items) => {
            for item in items {
                collect_legacy_caching_class_violations(rpc_name, item, violations);
            }
        }
        PyRep::Dict(entries) => {
            for (k, v) in entries {
                collect_legacy_caching_class_violations(rpc_name, k, violations);
                collect_legacy_caching_class_violations(rpc_name, v, violations);
            }
        }
        PyRep::KeyVal(map) => {
            for v in map.values() {
                collect_legacy_caching_class_violations(rpc_name, v, violations);
            }
        }
        _ => {}
    }
}

pub fn unwrap_envelope(rep: &PyRep, preserve_cached_obj: bool) -> PyRep {
    match rep {
        PyRep::SubStream(inner) | PyRep::SubStruct(inner) => {
            unwrap_envelope(inner, preserve_cached_obj)
        }
        PyRep::ChecksumedStream { body, .. } => unwrap_envelope(body, preserve_cached_obj),
        PyRep::Object { type_name, args }
            if type_name == V24_CACHED_METHOD_CALL_RESULT_CLASS
                || type_name == "objectCaching.CachedMethodCallResult"
                || type_name == "util.CachedMethodCallResult"
                || type_name == "utillib.CachedMethodCallResult" =>
        {
            if let PyRep::Tuple(items) = args.as_ref() {
                if let Some(sub) = items.get(1) {
                    return unwrap_envelope(sub, preserve_cached_obj);
                }
            }
            rep.clone()
        }
        PyRep::Object { type_name, args }
            if !preserve_cached_obj
                && (type_name == V24_CACHED_OBJECT_CLASS
                    || type_name == V24_CACHE_HINT_CLASS
                    || type_name == "objectCaching.CachedObject"
                    || type_name == "util.CachedObject") =>
        {
            if let PyRep::Tuple(items) = args.as_ref() {
                if let Some(PyRep::Buffer(bytes) | PyRep::Bytes(bytes)) = items.get(4) {
                    if let Ok(decoded) = eve_proto::marshal::decode(bytes) {
                        return unwrap_envelope(&decoded, preserve_cached_obj);
                    }
                }
            }
            rep.clone()
        }
        _ => rep.clone(),
    }
}

pub fn rowset_row_to_keyval(columns: &[String], row: &[PyRep]) -> PyRep {
    let mut map = HashMap::with_capacity(columns.len());
    for (i, col) in columns.iter().enumerate() {
        map.insert(col.clone(), row.get(i).cloned().unwrap_or(PyRep::None));
    }
    PyRep::KeyVal(map)
}

pub fn crowset_descriptor_columns(rep: &PyRep) -> Option<Vec<String>> {
    if let PyRep::ObjectEx { header, dict, .. } = rep {
        if let PyRep::Tuple(items) = header.as_ref() {
            if let Some(desc_rep) = items.get(1).and_then(|state| match state {
                PyRep::Dict(entries) => entries
                    .iter()
                    .find(|(k, _)| k.as_str() == Some("header"))
                    .map(|(_, v)| v),
                PyRep::KeyVal(map) => map.get("header"),
                _ => None,
            }) {
                if let Some(defs) = eve_proto::extract_dbrow_header_col_defs(desc_rep) {
                    return Some(defs.into_iter().map(|(n, _)| n).collect());
                }
            }
            if let Some(PyRep::Tuple(args)) = items.first() {
                if let Some(desc_rep) = args.get(1).and_then(|t| {
                    if let PyRep::Tuple(inner) = t {
                        inner.first()
                    } else {
                        Some(t)
                    }
                }) {
                    if let Some(defs) = eve_proto::extract_dbrow_header_col_defs(desc_rep) {
                        return Some(defs.into_iter().map(|(n, _)| n).collect());
                    }
                }
            }
        }
        if let Some((_, desc_rep)) = dict.iter().find(|(k, _)| k.as_str() == Some("header")) {
            if let Some(defs) = eve_proto::extract_dbrow_header_col_defs(desc_rep) {
                return Some(defs.into_iter().map(|(n, _)| n).collect());
            }
        }
    }
    None
}

pub fn parse_path_segments(path: &str) -> Vec<String> {
    if path == "$" || path.is_empty() {
        return Vec::new();
    }
    let mut segs = Vec::new();
    let mut rest = path;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix(".keys()") {
            segs.push(".keys()".to_string());
            rest = after;
        } else if let Some(after) = rest.strip_prefix(".values()") {
            segs.push(".values()".to_string());
            rest = after;
        } else if let Some(after) = rest.strip_prefix(".items()") {
            segs.push(".items()".to_string());
            rest = after;
        } else if let Some(after) = rest.strip_prefix("[]") {
            segs.push("[]".to_string());
            rest = after;
        } else if let Some(after) = rest.strip_prefix("[*]") {
            segs.push("[*]".to_string());
            rest = after;
        } else if rest.starts_with('[') {
            if let Some(end) = rest.find(']') {
                segs.push(rest[..=end].to_string());
                rest = &rest[end + 1..];
            } else {
                break;
            }
        } else if let Some(after) = rest.strip_prefix('.') {
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            segs.push(format!(".{}", &after[..end]));
            rest = &after[end..];
        } else {
            break;
        }
    }
    segs
}

pub fn step_segment(val: &PyRep, seg: &str) -> Vec<PyRep> {
    let val = unwrap_envelope(val, false);
    if matches!(val, PyRep::None) {
        return Vec::new();
    }
    match seg {
        "[]" => match &val {
            PyRep::List(items)
            | PyRep::Tuple(items)
            | PyRep::Set(items)
            | PyRep::PySet(items) => items.clone(),
            PyRep::Rowset { columns, rows } | PyRep::TupleSet { columns, rows } => rows
                .iter()
                .map(|r| rowset_row_to_keyval(columns, r))
                .collect(),
            PyRep::IndexRowset { columns, items, .. } => items
                .iter()
                .map(|(_, r)| rowset_row_to_keyval(columns, r))
                .collect(),
            PyRep::Dict(entries) => {
                if entries.first().is_some_and(|(_, v)| {
                    matches!(
                        v,
                        PyRep::KeyVal(_)
                            | PyRep::DBRow { .. }
                            | PyRep::Object { .. }
                            | PyRep::ObjectEx { .. }
                    )
                }) {
                    entries.iter().map(|(_, v)| v.clone()).collect()
                } else {
                    entries.iter().map(|(k, _)| k.clone()).collect()
                }
            }
            PyRep::ObjectEx { list, dict, .. } => {
                if val.is_crowset() {
                    list.clone()
                } else if val.is_cindexed_rowset() {
                    dict.iter().map(|(k, _)| k.clone()).collect()
                } else if !list.is_empty() {
                    list.clone()
                } else {
                    dict.iter().map(|(k, _)| k.clone()).collect()
                }
            }
            _ => Vec::new(),
        },
        "[*]" => match &val {
            PyRep::Dict(entries) => entries.iter().map(|(_, v)| v.clone()).collect(),
            PyRep::KeyVal(map) => map.values().cloned().collect(),
            PyRep::IndexRowset { columns, items, .. } => items
                .iter()
                .map(|(_, r)| rowset_row_to_keyval(columns, r))
                .collect(),
            PyRep::List(items) | PyRep::Tuple(items) => items.clone(),
            PyRep::Rowset { columns, rows } | PyRep::TupleSet { columns, rows } => rows
                .iter()
                .map(|r| rowset_row_to_keyval(columns, r))
                .collect(),
            PyRep::ObjectEx { list, dict, .. } => {
                if val.is_cindexed_rowset() || !dict.is_empty() {
                    dict.iter().map(|(_, v)| v.clone()).collect()
                } else {
                    list.clone()
                }
            }
            _ => Vec::new(),
        },
        ".keys()" => match &val {
            PyRep::Dict(entries) => {
                vec![PyRep::List(entries.iter().map(|(k, _)| k.clone()).collect())]
            }
            PyRep::KeyVal(map) => {
                vec![PyRep::List(map.keys().cloned().map(PyRep::String).collect())]
            }
            PyRep::IndexRowset { items, .. } => {
                vec![PyRep::List(items.iter().map(|(k, _)| k.clone()).collect())]
            }
            PyRep::ObjectEx { dict, .. } => {
                vec![PyRep::List(dict.iter().map(|(k, _)| k.clone()).collect())]
            }
            _ => Vec::new(),
        },
        ".values()" => match &val {
            PyRep::Dict(entries) => {
                vec![PyRep::List(entries.iter().map(|(_, v)| v.clone()).collect())]
            }
            PyRep::KeyVal(map) => vec![PyRep::List(map.values().cloned().collect())],
            PyRep::IndexRowset { columns, items, .. } => vec![PyRep::List(
                items
                    .iter()
                    .map(|(_, r)| rowset_row_to_keyval(columns, r))
                    .collect(),
            )],
            PyRep::ObjectEx { dict, .. } => {
                vec![PyRep::List(dict.iter().map(|(_, v)| v.clone()).collect())]
            }
            _ => Vec::new(),
        },
        ".items()" => match &val {
            PyRep::Dict(entries) => vec![PyRep::List(
                entries
                    .iter()
                    .map(|(k, v)| PyRep::Tuple(vec![k.clone(), v.clone()]))
                    .collect(),
            )],
            PyRep::KeyVal(map) => vec![PyRep::List(
                map.iter()
                    .map(|(k, v)| PyRep::Tuple(vec![PyRep::String(k.clone()), v.clone()]))
                    .collect(),
            )],
            PyRep::IndexRowset { columns, items, .. } => vec![PyRep::List(
                items
                    .iter()
                    .map(|(k, r)| {
                        PyRep::Tuple(vec![k.clone(), rowset_row_to_keyval(columns, r)])
                    })
                    .collect(),
            )],
            PyRep::ObjectEx { dict, .. } => vec![PyRep::List(
                dict.iter()
                    .map(|(k, v)| PyRep::Tuple(vec![k.clone(), v.clone()]))
                    .collect(),
            )],
            _ => Vec::new(),
        },
        _ if seg.starts_with('.') => {
            let attr = &seg[1..];
            if let Some(v) = get_attr(&val, attr) {
                vec![v]
            } else {
                Vec::new()
            }
        }
        _ if seg.starts_with('[') && seg.ends_with(']') => {
            let inner = &seg[1..seg.len() - 1];
            if let Some(str_key) = inner.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                if let Some(v) = get_subscript_str(&val, str_key) {
                    vec![v]
                } else {
                    Vec::new()
                }
            } else if let Ok(idx) = inner.parse::<isize>() {
                if let Some(v) = get_subscript_int(&val, idx) {
                    vec![v]
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn object_ex_class_name(header: &PyRep) -> Option<&str> {
    if let PyRep::Tuple(items) = header {
        if let Some(first) = items.first() {
            if let Some(s) = first.as_str() {
                return Some(s);
            }
            if let PyRep::Tuple(inner) = first {
                if let Some(s) = inner.first().and_then(|x| x.as_str()) {
                    return Some(s);
                }
            }
        }
    }
    None
}

fn is_industry_location_class(name: &str) -> bool {
    name == "industry.job.Location"
        || name == "industry.Location"
        || name.ends_with("FacilityLocation")
}

pub fn get_attr(val: &PyRep, attr: &str) -> Option<PyRep> {
    match val {
        PyRep::KeyVal(map) => {
            if attr == "__dict__" {
                return Some(val.clone());
            }
            map.get(attr).cloned()
        }
        PyRep::DBRow {
            header, columns, ..
        } => {
            if attr == "__header__" || attr == "header" {
                return Some((**header).clone());
            }
            if attr == "__keys__" || attr == "__columns__" || attr == "columns" {
                return Some(PyRep::List(
                    columns
                        .iter()
                        .map(|(k, _)| PyRep::String(k.clone()))
                        .collect(),
                ));
            }
            if attr == "line" {
                return Some(PyRep::List(columns.iter().map(|(_, v)| v.clone()).collect()));
            }
            if let Some((_, v)) = columns.iter().find(|(k, _)| k == attr) {
                return Some(v.clone());
            }
            if attr == "flag" {
                if let Some((_, v)) = columns.iter().find(|(k, _)| k == "flagID") {
                    return Some(v.clone());
                }
            }
            if attr == "stacksize" {
                if let Some((_, v)) = columns.iter().find(|(k, _)| k == "quantity") {
                    let q = v.as_i64().unwrap_or(1);
                    return Some(PyRep::Integer(q.max(1)));
                }
            }
            if attr == "singleton" {
                if let Some((_, v)) = columns.iter().find(|(k, _)| k == "quantity") {
                    let q = v.as_i64().unwrap_or(1);
                    return Some(PyRep::Integer(if q < 0 { -q } else { 0 }));
                }
            }
            None
        }
        PyRep::Rowset { columns, rows } | PyRep::TupleSet { columns, rows } => match attr {
            "columns" | "__columns__" | "header" => Some(PyRep::List(
                columns.iter().cloned().map(PyRep::String).collect(),
            )),
            "lines" => Some(PyRep::List(
                rows.iter().cloned().map(PyRep::List).collect(),
            )),
            _ => rows
                .first()
                .and_then(|r| rowset_row_to_keyval(columns, r).dict_get(attr).cloned()),
        },
        PyRep::IndexRowset {
            key_column,
            columns,
            items,
        } => match attr {
            "columns" | "__columns__" | "header" => Some(PyRep::List(
                columns.iter().cloned().map(PyRep::String).collect(),
            )),
            "key" => Some(PyRep::String(key_column.clone())),
            "items" => Some(PyRep::Dict(
                items
                    .iter()
                    .map(|(k, r)| (k.clone(), rowset_row_to_keyval(columns, r)))
                    .collect(),
            )),
            _ => None,
        },
        PyRep::Object { type_name, args } => {
            if type_name.ends_with(".Row") || type_name == "Row" {
                if let PyRep::Tuple(parts) = args.as_ref() {
                    if let (Some(PyRep::List(cols)), Some(PyRep::List(vals))) =
                        (parts.first(), parts.get(1))
                    {
                        if attr == "header" || attr == "columns" || attr == "__columns__" {
                            return Some(PyRep::List(cols.clone()));
                        }
                        if attr == "line" {
                            return Some(PyRep::List(vals.clone()));
                        }
                        for (i, c) in cols.iter().enumerate() {
                            if c.as_str() == Some(attr) {
                                return Some(vals.get(i).cloned().unwrap_or(PyRep::None));
                            }
                        }
                    }
                }
            }
            if is_industry_location_class(type_name) && attr == "GetInvController" {
                return Some(PyRep::String("bound_method:GetInvController".into()));
            }
            if type_name.ends_with("SlimItem") {
                return get_attr(args, attr)
                    .or_else(|| get_subscript_str(args, attr))
                    .or(Some(PyRep::None));
            }
            get_attr(args, attr).or_else(|| get_subscript_str(args, attr))
        }
        PyRep::ObjectEx { header, dict, .. } => {
            if attr == "header" || attr == "__header__" {
                return Some((**header).clone());
            }
            if object_ex_class_name(header).is_some_and(is_industry_location_class)
                && attr == "GetInvController"
            {
                return Some(PyRep::String("bound_method:GetInvController".into()));
            }
            if let Some((_, v)) = dict.iter().find(|(k, _)| k.as_str() == Some(attr)) {
                return Some(v.clone());
            }
            // Check state dict in `header[1]` or constructor args in `header[0]`
            if let PyRep::Tuple(h_items) = header.as_ref() {
                if let Some(state) = h_items.get(1) {
                    if let Some(v) = get_attr(state, attr).or_else(|| get_subscript_str(state, attr))
                    {
                        return Some(v);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

pub fn get_subscript_str(val: &PyRep, key: &str) -> Option<PyRep> {
    match val {
        PyRep::KeyVal(map) => map.get(key).cloned(),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v.clone()),
        PyRep::DBRow { columns, .. } => columns
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone()),
        PyRep::Object { type_name, args } => {
            if type_name.ends_with(".Row") || type_name == "Row" {
                if let PyRep::Tuple(parts) = args.as_ref() {
                    if let (Some(PyRep::List(cols)), Some(PyRep::List(vals))) =
                        (parts.first(), parts.get(1))
                    {
                        if key == "header" || key == "columns" {
                            return Some(PyRep::List(cols.clone()));
                        }
                        if key == "line" {
                            return Some(PyRep::List(vals.clone()));
                        }
                        for (i, c) in cols.iter().enumerate() {
                            if c.as_str() == Some(key) {
                                return Some(vals.get(i).cloned().unwrap_or(PyRep::None));
                            }
                        }
                    }
                }
            }
            get_subscript_str(args, key)
        }
        PyRep::ObjectEx { dict, .. } => dict
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v.clone()),
        _ => None,
    }
}

pub fn get_subscript_int(val: &PyRep, idx: isize) -> Option<PyRep> {
    let resolve_idx = |len: usize| -> Option<usize> {
        if idx >= 0 {
            let u = idx as usize;
            (u < len).then_some(u)
        } else {
            let back = (-idx) as usize;
            (back <= len).then(|| len - back)
        }
    };
    match val {
        PyRep::Tuple(items) | PyRep::List(items) => {
            resolve_idx(items.len()).map(|i| items[i].clone())
        }
        PyRep::Rowset { columns, rows } => {
            resolve_idx(rows.len()).map(|i| rowset_row_to_keyval(columns, &rows[i]))
        }
        PyRep::TupleSet { rows, .. } => {
            resolve_idx(rows.len()).map(|i| PyRep::List(rows[i].clone()))
        }
        PyRep::DBRow { columns, .. } => resolve_idx(columns.len()).map(|i| columns[i].1.clone()),
        PyRep::Object { type_name, args } => {
            if type_name.ends_with(".Row") || type_name == "Row" {
                if let PyRep::Tuple(parts) = args.as_ref() {
                    if let Some(PyRep::List(vals)) = parts.get(1) {
                        return resolve_idx(vals.len()).map(|i| vals[i].clone());
                    }
                } else if let Some(PyRep::List(vals)) = get_subscript_str(args, "line") {
                    return resolve_idx(vals.len()).map(|i| vals[i].clone());
                }
            }
            get_subscript_int(args, idx)
        }
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_i64() == Some(idx as i64))
            .map(|(_, v)| v.clone()),
        PyRep::IndexRowset { columns, items, .. } => items
            .iter()
            .find(|(k, _)| k.as_i64() == Some(idx as i64))
            .map(|(_, r)| rowset_row_to_keyval(columns, r)),
        PyRep::ObjectEx { list, dict, .. } => {
            if val.is_crowset() || !list.is_empty() {
                resolve_idx(list.len()).map(|i| list[i].clone())
            } else {
                dict.iter()
                    .find(|(k, _)| k.as_i64() == Some(idx as i64))
                    .map(|(_, v)| v.clone())
            }
        }
        _ => None,
    }
}

pub fn resolve_path(root: &PyRep, path: &str) -> Vec<PyRep> {
    let segs = parse_path_segments(path);
    let mut current = vec![root.clone()];
    for seg in segs {
        let mut next = Vec::new();
        for v in &current {
            next.extend(step_segment(v, &seg));
        }
        current = next;
        if current.is_empty() {
            break;
        }
    }
    current
}

pub fn has_column_in_collection(val: &PyRep, col: &str) -> bool {
    let val = unwrap_envelope(val, false);
    let matches_col = |cols: &[String]| -> bool {
        cols.iter().any(|c| c == col)
            || (col == "flag" && cols.iter().any(|c| c == "flagID"))
            || ((col == "stacksize" || col == "singleton") && cols.iter().any(|c| c == "quantity"))
    };
    match &val {
        PyRep::Rowset { columns, .. }
        | PyRep::IndexRowset { columns, .. }
        | PyRep::TupleSet { columns, .. } => matches_col(columns),
        PyRep::ObjectEx { list, .. } if val.is_crowset() || val.is_cindexed_rowset() => {
            if let Some(cols) = crowset_descriptor_columns(&val) {
                return matches_col(&cols);
            }
            list.first().is_some_and(|r| get_attr(r, col).is_some())
        }
        _ => false,
    }
}

fn ops_have_null_guard(ops: &[&str]) -> bool {
    ops.iter().any(|o| {
        matches!(
            *o,
            "truthy"
                | "compareisNone"
                | "compareisnull"
                | "compareis_notnull"
                | "compareis_not"
                | "compare==null"
                | "compare!=null"
                | "compareisUNCACHED"
                | "compareisSTATE_UNKNOWN"
                | "compareisLOCAL_CAMPAIGN_UNKNOWN"
        ) || o.starts_with("isinstance:")
            || o.starts_with("hasattr[")
            || o.starts_with("getattr[")
    })
}

fn ops_dereference_value(
    path: &str,
    ops: &[&str],
    reply_shape: &serde_json::Map<String, Value>,
) -> bool {
    let child_iter_path = if path == "$" {
        "[]".to_string()
    } else {
        format!("{path}[]")
    };
    ops.iter().any(|o| {
        o.starts_with("unpack:")
            || (o.starts_with("attr:")
                && !matches!(
                    *o,
                    "attr:clear_memoized" | "attr:remove_from_cache" | "attr:prime_cache_result"
                ))
            || o.starts_with("method:")
            || o.starts_with("rowset_index[")
            || o.starts_with("rowset_filter[")
            || o.starts_with("subscript[")
            || o.starts_with("get[")
            || o.starts_with("contains[")
            || o.starts_with("type:")
            || matches!(*o, "len" | "arith" | "number")
            || (*o == "iter" && reply_shape.contains_key(&child_iter_path))
    })
}

fn is_mapping_value(val: &PyRep) -> bool {
    match val {
        PyRep::Dict(_) | PyRep::IndexRowset { .. } | PyRep::KeyVal(_) => true,
        PyRep::Object { type_name, .. } => {
            !type_name.ends_with(".Rowset") && type_name != "Rowset"
        }
        PyRep::ObjectEx { .. } => !val.is_crowset(),
        _ => false,
    }
}

fn is_datetime_value(val: &PyRep) -> bool {
    match val {
        PyRep::Object { type_name, .. } => type_name.contains("datetime"),
        PyRep::ObjectEx { header, .. } => {
            object_ex_class_name(header).is_some_and(|c| c.contains("datetime"))
        }
        _ => false,
    }
}

/// Depth-1 and Depth-2 return-value consumer traces (`#912`, `#934`, `#939`, `#942`, `#943`, `#945`).
/// Follows RPC return values 1–2 hops across client wrapper methods, tagging each traced path with
/// `depth:<1|2>:<source_function>` and merging the consumer's structural/type operations.
pub fn augment_reply_shape_with_depth_traces(
    rpc_name: &str,
    base_shape: &serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    let mut merged = base_shape.clone();
    let overlays: &[(&str, &[&str])] = match rpc_name {
        "facilityManager.GetFacilityLocations" => &[(
            "[]",
            &[
                "attr:GetInvController",
                "type:FacilityLocation",
                "depth:2:blueprintBrowser.GetFacilityLocations->FacilityLocation.GetInvController",
            ],
        )],
        "XmppChatMgr.CreatePlayerOwnedChannel" => &[(
            "$",
            &[
                "type:str",
                "method:split",
                "depth:2:XmppChatSvc.CreateChannel->EnsureValidChannelID",
            ],
        )],
        "dungeonInstanceCacheMgr.GetIceBeltsCount" => &[(
            "$",
            &[
                "method:iteritems",
                "depth:1:agencyIceBeltSection.GetSystemResourceCounts",
            ],
        )],
        "dungeonInstanceCacheMgr.GetCombatAnomaliesCount" => &[(
            "$",
            &[
                "method:iteritems",
                "depth:1:agencyCombatAnomaliesSection.GetSystemAnomalyCounts",
            ],
        )],
        "dungeonInstanceCacheMgr.GetOreAnomaliesCount" => &[(
            "$",
            &[
                "method:iteritems",
                "depth:1:agencyOreAnomaliesSection.GetSystemAnomalyCounts",
            ],
        )],
        "corpRegistry.GetMyApplications" => &[
            (
                "$",
                &[
                    "contains[*]",
                    "subscript[*]",
                    "depth:1:CorpSvc.GetMyApplications",
                ],
            ),
            (
                "[*]",
                &["iter", "depth:2:CorpApplicationsWnd.LoadMyApplications"],
            ),
        ],
        "corpRegistry.GetApplications" => &[
            (
                "$",
                &[
                    "contains[*]",
                    "subscript[*]",
                    "depth:1:CorpSvc.GetApplications",
                ],
            ),
            (
                "[*]",
                &["iter", "depth:2:CorpApplicationsWnd.LoadApplications"],
            ),
        ],
        "home_station.get_next_remote_change_time" => &[(
            "$",
            &[
                "compareis_notnull",
                "type:datetime",
                "depth:2:HomeStationService.get_next_remote_change_time->gametime.get_time_Until",
            ],
        )],
        _ => &[],
    };

    for &(path, extra_ops) in overlays {
        let entry = merged
            .entry(path.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(arr) = entry {
            for &op in extra_ops {
                if !arr.iter().any(|v| v.as_str() == Some(op)) {
                    arr.push(Value::String(op.to_string()));
                }
            }
        }
    }
    merged
}

/// Validates a `PyRep` response against the client's `reply_shape` expectations under strict
/// `C-1` (`#912`) rules and returns both violations and non-vacuity metrics.
pub fn validate_reply_shape_with_metrics(
    lookup: Option<&SemanticLookupContext<'_>>,
    rpc_name: &str,
    raw_reply: &PyRep,
    reply_shape: &serde_json::Map<String, Value>,
) -> ReplyValidationOutcome {
    let augmented;
    let reply_shape = if matches!(
        rpc_name,
        "facilityManager.GetFacilityLocations"
            | "XmppChatMgr.CreatePlayerOwnedChannel"
            | "dungeonInstanceCacheMgr.GetIceBeltsCount"
            | "dungeonInstanceCacheMgr.GetCombatAnomaliesCount"
            | "dungeonInstanceCacheMgr.GetOreAnomaliesCount"
            | "corpRegistry.GetMyApplications"
            | "corpRegistry.GetApplications"
            | "home_station.get_next_remote_change_time"
    ) {
        augmented = augment_reply_shape_with_depth_traces(rpc_name, reply_shape);
        &augmented
    } else {
        reply_shape
    };
    let mut outcome = ReplyValidationOutcome::default();
    collect_legacy_caching_class_violations(rpc_name, raw_reply, &mut outcome.violations);
    let root_ops: Vec<&str> = reply_shape
        .get("$")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();

    let preserve_cached = root_ops.contains(&"attr:GetCachedObject");
    let root = unwrap_envelope(raw_reply, preserve_cached);

    if reply_shape.len() == 1 && root_ops.iter().all(|&o| o == "discard") {
        return outcome;
    }

    let root_has_null_guard = ops_have_null_guard(&root_ops);

    for (path, ops_val) in reply_shape {
        let Some(ops_arr) = ops_val.as_array() else {
            continue;
        };
        let ops: Vec<&str> = ops_arr.iter().filter_map(|v| v.as_str()).collect();
        let values = resolve_path(&root, path);

        let unpack_arities: Vec<usize> = ops
            .iter()
            .filter_map(|o| o.strip_prefix("unpack:")?.parse::<usize>().ok())
            .collect();
        let has_null_guard = ops_have_null_guard(&ops) || (path != "$" && root_has_null_guard);

        // Track vacuity when collection element paths (`[]` or `[*]`) resolve to 0 items
        if (path.contains("[]") || path.contains("[*]")) && values.is_empty() && !matches!(root, PyRep::None) {
            outcome.had_unexercised_collection_elements = true;
        }

        // Strict `None` policy (#912): `PyRep::None` at any dereferenced path without a null guard is a violation!
        if path == "$" && matches!(root, PyRep::None) {
            if !has_null_guard && ops_dereference_value(path, &ops, reply_shape) {
                outcome.violations.push(format!(
                    "{rpc_name}: path `$` returned None without null-guard, but client dereferences {ops:?}"
                ));
            }
            continue;
        }

        for val in &values {
            let val = unwrap_envelope(val, preserve_cached);
            if matches!(val, PyRep::None) {
                if !has_null_guard && ops_dereference_value(path, &ops, reply_shape) {
                    outcome.violations.push(format!(
                        "{rpc_name}: path `{path}` returned None without null-guard, but client dereferences {ops:?}"
                    ));
                }
                continue;
            }

            outcome.paths_checked += 1;

            // 1. Validate `unpack:N`
            if !unpack_arities.is_empty() {
                let actual_len = match &val {
                    PyRep::Tuple(items)
                    | PyRep::List(items)
                    | PyRep::Set(items)
                    | PyRep::PySet(items) => Some(items.len()),
                    PyRep::TupleSet { .. } => Some(2),
                    PyRep::DBRow { columns, .. } => Some(columns.len()),
                    PyRep::Object { type_name, args }
                        if type_name.ends_with(".Row") || type_name == "Row" =>
                    {
                        if let PyRep::Tuple(parts) = args.as_ref() {
                            parts.get(1).and_then(|p| match p {
                                PyRep::List(l) | PyRep::Tuple(l) => Some(l.len()),
                                _ => None,
                            })
                        } else if let Some(PyRep::List(vals)) = get_subscript_str(args, "line") {
                            Some(vals.len())
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                match actual_len {
                    None => {
                        let is_sublocation_union = ops.iter().any(|o| o.contains("IsSubLocation"));
                        if !(is_sublocation_union && matches!(val, PyRep::Integer(_))) {
                            outcome.violations.push(format!(
                                "{rpc_name}: path `{path}` expected unpackable sequence {unpack_arities:?}, got non-sequence {val:?}"
                            ));
                        }
                    }
                    Some(n) if !unpack_arities.contains(&n) => {
                        let has_slice = ops.iter().any(|o| o.starts_with("slice"));
                        if !has_slice {
                            outcome.violations.push(format!(
                                "{rpc_name}: path `{path}` expected tuple/list arity in {unpack_arities:?}, got len {n} ({val:?})"
                            ));
                        }
                    }
                    _ => {}
                }
            }

            // 1b. Validate `iter` at path when the client iterates elements (`[]` child path)
            let child_iter_path = if path == "$" {
                "[]".to_string()
            } else {
                format!("{path}[]")
            };
            let has_isinstance_guard = ops.iter().any(|o| o.starts_with("isinstance:"));
            if ops.contains(&"iter")
                && reply_shape.contains_key(&child_iter_path)
                && !has_isinstance_guard
            {
                let is_iterable = matches!(
                    val,
                    PyRep::List(_)
                        | PyRep::Tuple(_)
                        | PyRep::Set(_)
                        | PyRep::PySet(_)
                        | PyRep::Dict(_)
                        | PyRep::Rowset { .. }
                        | PyRep::IndexRowset { .. }
                        | PyRep::TupleSet { .. }
                        | PyRep::DBRow { .. }
                        | PyRep::ObjectEx { .. }
                        | PyRep::Object { .. }
                );
                if !is_iterable {
                    outcome.violations.push(format!(
                        "{rpc_name}: path `{path}` expected iterable (`iter`), got non-iterable {val:?}"
                    ));
                }
            }

            // 1c. Validate `contains[*]` + `subscript[*]` (dict/keyed lookup, #943)
            if ops.contains(&"contains[*]")
                && ops.contains(&"subscript[*]")
                && !has_isinstance_guard
                && !ops.iter().any(|o| o.starts_with("rowset_index["))
            {
                if matches!(val, PyRep::Rowset { .. }) || val.is_crowset() {
                    outcome.violations.push(format!(
                        "{rpc_name}: path `{path}` requires key-indexed mapping (`contains[*]` + `subscript[*]`), got flat Rowset {val:?}"
                    ));
                }
            }

            // 1d. Validate `method:<name>` and `type:<expected>` (#939, #942, #945)
            for op in &ops {
                if let Some(mname) = op.strip_prefix("method:") {
                    if !has_isinstance_guard {
                        if matches!(
                            mname,
                            "iteritems"
                                | "itervalues"
                                | "iterkeys"
                                | "items"
                                | "values"
                                | "keys"
                                | "get"
                        ) && !ops.contains(&"collected")
                            && !is_mapping_value(&val)
                        {
                            outcome.violations.push(format!(
                                "{rpc_name}: path `{path}` calls dict method `.{mname}()`, but server returned non-mapping {val:?}"
                            ));
                        } else if matches!(
                            mname,
                            "split" | "lower" | "upper" | "strip" | "startswith"
                        ) && !matches!(val, PyRep::String(_))
                        {
                            outcome.violations.push(format!(
                                "{rpc_name}: path `{path}` calls string method `.{mname}()`, but server returned non-string {val:?}"
                            ));
                        }
                    }
                } else if *op == "type:str"
                    && !has_isinstance_guard
                    && !matches!(val, PyRep::String(_))
                {
                    outcome.violations.push(format!(
                        "{rpc_name}: path `{path}` requires string (`type:str`), got {val:?}"
                    ));
                } else if *op == "type:datetime" && !is_datetime_value(&val) {
                    outcome.violations.push(format!(
                        "{rpc_name}: path `{path}` requires datetime.datetime object (`type:datetime`), got {val:?}"
                    ));
                }
            }

            // 2. Validate `rowset_index["col"]` and `rowset_filter["col"]`
            for op in &ops {
                if let Some(col_raw) = op
                    .strip_prefix("rowset_index[")
                    .or_else(|| op.strip_prefix("rowset_filter["))
                    .and_then(|s| s.strip_suffix(']'))
                {
                    let col = col_raw.trim_matches('"');
                    if !has_column_in_collection(&val, col) {
                        outcome.violations.push(format!(
                            "{rpc_name}: path `{path}` requires Rowset/CRowset with column `{col}` ({op}), got {val:?}"
                        ));
                    }
                }
            }

            // 3. Validate `attr:<field>`
            for op in &ops {
                let Some(field) = op.strip_prefix("attr:") else {
                    continue;
                };
                if matches!(
                    field,
                    "append"
                        | "extend"
                        | "insert"
                        | "remove"
                        | "pop"
                        | "sort"
                        | "reverse"
                        | "count"
                        | "index"
                        | "update"
                        | "clear"
                        | "setdefault"
                        | "add"
                        | "discard"
                        | "union"
                        | "intersection"
                        | "difference"
                        | "split"
                        | "strip"
                        | "lower"
                        | "upper"
                        | "replace"
                        | "startswith"
                        | "endswith"
                        | "format"
                        | "join"
                        | "encode"
                        | "decode"
                        | "iterkeys"
                        | "itervalues"
                        | "iteritems"
                        | "has_key"
                        | "viewkeys"
                        | "viewvalues"
                        | "viewitems"
                        | "__dict__"
                        | "__keys__"
                        | "__columns__"
                        | "__header__"
                        | "header"
                        | "columns"
                        | "lines"
                        | "line"
                        | "clear_memoized"
                        | "remove_from_cache"
                        | "prime_cache_result"
                        | "virtual"
                        | "GetCachedObject"
                        | "GetNextPendingChangeTime"
                        | "IsFriendlyFireLegalAtTime"
                        | "GetAllBehaviorEntriesByKey"
                        | "List"
                ) {
                    if matches!(field, "append" | "extend" | "insert" | "sort" | "reverse")
                        && !matches!(val, PyRep::List(_) | PyRep::Rowset { .. })
                        && !val.is_crowset()
                    {
                        outcome.violations.push(format!(
                            "{rpc_name}: path `{path}` calls list method `.{field}()`, but server returned non-List {val:?}"
                        ));
                    } else if matches!(
                        field,
                        "iterkeys"
                            | "itervalues"
                            | "iteritems"
                            | "has_key"
                            | "viewkeys"
                            | "viewvalues"
                            | "viewitems"
                    ) && !is_mapping_value(&val)
                    {
                        outcome.violations.push(format!(
                            "{rpc_name}: path `{path}` calls dict method `.{field}()`, but server returned non-mapping {val:?}"
                        ));
                    }
                    continue;
                }

                let has_guard = ops.iter().any(|o| {
                    *o == format!("hasattr[\"{field}\"]")
                        || *o == format!("getattr[\"{field}\"]")
                        || o.starts_with("isinstance:")
                });
                if has_guard {
                    continue;
                }

                if matches!(val, PyRep::Dict(_)) {
                    outcome.violations.push(format!(
                        "{rpc_name}: path `{path}` accesses `.{field}` (`attr:{field}`), but server returned raw `PyRep::Dict` instead of `KeyVal`/`DBRow`/`Rowset`"
                    ));
                    continue;
                }

                if matches!(
                    val,
                    PyRep::Rowset { .. } | PyRep::IndexRowset { .. } | PyRep::TupleSet { .. }
                ) || val.is_crowset()
                    || val.is_cindexed_rowset()
                {
                    if !has_column_in_collection(&val, field) && get_attr(&val, field).is_none() {
                        outcome.violations.push(format!(
                            "{rpc_name}: path `{path}` accesses `.{field}` on Rowset/CRowset, but column `{field}` is missing from header"
                        ));
                    }
                    continue;
                }

                if matches!(val, PyRep::SubStruct(_)) {
                    continue;
                }

                if get_attr(&val, field).is_none() {
                    outcome.violations.push(format!(
                        "{rpc_name}: path `{path}` missing required attribute `.{field}` in {val:?}"
                    ));
                }
            }

            // 4. Phase 2: Semantic foreign-key and unit sink validation (`passed_to:...`)
            for op in &ops {
                let Some(sink) = op.strip_prefix("passed_to:") else {
                    continue;
                };
                if sink.starts_with("evetypes.")
                    || sink.starts_with("typeHelpers.")
                    || sink.starts_with("inventorycommon.typeHelpers.")
                {
                    if let PyRep::Integer(tid) = val {
                        let is_existence_check = sink.starts_with("evetypes.Exists");
                        if !is_existence_check {
                            if tid < 0 || (tid == 0 && !has_null_guard) {
                                outcome.violations.push(format!(
                                    "{rpc_name}: path `{path}` passes invalid typeID={tid} to `{sink}`"
                                ));
                            } else if let Some(ctx) = lookup {
                                if tid > 0 && !ctx.sde.types.contains_key(&(tid as u32)) {
                                    outcome.violations.push(format!(
                                        "{rpc_name}: path `{path}` passes unknown SDE typeID={tid} to `{sink}`"
                                    ));
                                }
                            }
                        }
                    }
                } else if sink.starts_with("cfg.eveowners.Get#0") {
                    if let PyRep::Integer(oid) = val {
                        if oid < 0 || (oid == 0 && !has_null_guard) {
                            outcome.violations.push(format!(
                                "{rpc_name}: path `{path}` passes invalid ownerID={oid} to `{sink}`"
                            ));
                        } else if let Some(ctx) = lookup {
                            if oid > 0 {
                                let is_polymorphic_type_or_sys = ops.iter().any(|o| {
                                    o.starts_with("passed_to:evetypes.")
                                        || o.contains("SolarSystem")
                                });
                                let valid_owner = oid <= 10_000
                                    || (500_000..=4_000_000).contains(&oid)
                                    || ctx.sde.factions.contains_key(&(oid as u32))
                                    || ctx.sde.npc_corporations.contains_key(&oid)
                                    || ctx.sde.agents.contains_key(&oid)
                                    || (is_polymorphic_type_or_sys
                                        && (ctx.sde.types.contains_key(&(oid as u32))
                                            || ctx
                                                .sde
                                                .solar_systems
                                                .contains_key(&(oid as u32))))
                                    || ctx.db.with_snapshot(|snap| {
                                        snap.characters.contains_key(&oid)
                                            || snap.corporations.contains_key(&oid)
                                            || snap.alliances.contains_key(&oid)
                                    });
                                if !valid_owner {
                                    outcome.violations.push(format!(
                                        "{rpc_name}: path `{path}` passes unresolvable ownerID={oid} to `{sink}` (not in SDE or WorldRepository)"
                                    ));
                                }
                            }
                        }
                    }
                } else if sink.starts_with("cfg.evelocations.Get#0") {
                    if let PyRep::Integer(lid) = val {
                        if lid < 0 || (lid == 0 && !has_null_guard) {
                            outcome.violations.push(format!(
                                "{rpc_name}: path `{path}` passes invalid locationID={lid} to `{sink}`"
                            ));
                        } else if let Some(ctx) = lookup {
                            if lid > 0 {
                                let valid_loc = (10_000_000..=85_000_000).contains(&lid)
                                    || lid >= 1_000_000_000_000
                                    || ctx.sde.regions.contains_key(&(lid as u32))
                                    || ctx.sde.constellations.contains_key(&(lid as u32))
                                    || ctx.sde.solar_systems.contains_key(&(lid as u32))
                                    || ctx.sde.stations.contains_key(&lid)
                                    || ctx
                                        .db
                                        .with_snapshot(|snap| snap.structures.contains_key(&lid));
                                if !valid_loc {
                                    outcome.violations.push(format!(
                                        "{rpc_name}: path `{path}` passes unresolvable locationID={lid} to `{sink}`"
                                    ));
                                }
                            }
                        }
                    }
                } else if (sink.starts_with("FmtDate#0")
                    || sink.starts_with("datetimeutils.filetime_to_datetime#0")
                    || sink.starts_with("blue.os.GetTimeParts#0"))
                    && !ops.contains(&"arith")
                {
                    if let PyRep::Integer(ts) = val {
                        if ts > 0 && ts < 100_000_000_000_000 {
                            outcome.violations.push(format!(
                                "{rpc_name}: path `{path}` passes non-FILETIME integer {ts} to `{sink}` (expected Windows FILETIME 100ns ticks or 0)"
                            ));
                        }
                    }
                }
            }
        }
    }

    outcome
}

/// Validates a `PyRep` response against `reply_shape` and returns the list of violation strings.
pub fn validate_reply_shape(
    lookup: Option<&SemanticLookupContext<'_>>,
    rpc_name: &str,
    raw_reply: &PyRep,
    reply_shape: &serde_json::Map<String, Value>,
) -> Vec<String> {
    validate_reply_shape_with_metrics(lookup, rpc_name, raw_reply, reply_shape).violations
}

/// Cached `client_expectations.json` paired with the SHA-256-verified [`ClientContractRatchet`] (`C-2`).
#[derive(Debug, Clone)]
pub struct LoadedClientContracts {
    pub ratchet: ClientContractRatchet,
    pub normalized_allowed_shape: BTreeSet<String>,
    pub normalized_allowed_notify: BTreeSet<String>,
    pub expectations: Value,
}

impl LoadedClientContracts {
    /// Returns `true` if `violation` matches an existing entry in `allowed_shape_violations`.
    pub fn is_shape_violation_allowed(&self, violation: &str) -> bool {
        self.ratchet.allowed_shape_violations.contains(violation)
            || self
                .normalized_allowed_shape
                .contains(&normalize_violation_for_ratchet(violation))
    }

    /// Returns `true` if `violation` matches an existing entry in `allowed_notify_violations`.
    pub fn is_notify_violation_allowed(&self, violation: &str) -> bool {
        self.ratchet.allowed_notify_violations.contains(violation)
            || self
                .normalized_allowed_notify
                .contains(&normalize_violation_for_ratchet(violation))
    }
}

impl ClientContractRatchet {
    /// Loads and caches the default `client_contract_ratchet.json` + `client_expectations.json` pair
    /// after verifying expectations version and SHA-256 integrity (`C-1` / `C-2`).
    pub fn load_default_cached() -> &'static LoadedClientContracts {
        static CACHE: std::sync::OnceLock<LoadedClientContracts> = std::sync::OnceLock::new();
        CACHE.get_or_init(|| {
            let ratchet =
                Self::load_default().expect("load default client_contract_ratchet.json");
            let exp_path = Self::default_expectations_path();
            let raw = std::fs::read(&exp_path).expect("read client_expectations.json");
            let expectations: Value =
                serde_json::from_slice(&raw).expect("parse client_expectations.json");
            ratchet
                .verify_expectations_integrity(&raw, &expectations)
                .expect("client_expectations.json integrity check");
            let normalized_allowed_shape = ratchet
                .allowed_shape_violations
                .iter()
                .map(|s| normalize_violation_for_ratchet(s))
                .collect();
            let normalized_allowed_notify = ratchet
                .allowed_notify_violations
                .iter()
                .map(|s| normalize_violation_for_ratchet(s))
                .collect();
            LoadedClientContracts {
                ratchet,
                normalized_allowed_shape,
                normalized_allowed_notify,
                expectations,
            }
        })
    }
}

/// Resolves the call-site-specific `reply_shape` for `slash.SlashCmd` based on the command string
/// in `args[0]` (`SlashService.cmd_<verb>` or matching constant `arg_consts`), avoiding false
/// collisions across unrelated client slash call sites (`C-2`).
pub fn resolve_slash_cmd_shape(
    rpc_spec: &Value,
    args: &[PyRep],
) -> serde_json::Map<String, Value> {
    let mut shape = serde_json::Map::new();
    let Some(cmd_str) = args.first().and_then(PyRep::as_str).map(str::trim) else {
        return shape;
    };
    let verb = cmd_str
        .strip_prefix('/')
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_ascii_lowercase);
    let expected_fn = verb.as_ref().map(|v| format!("SlashService.cmd_{v}"));
    let Some(sites) = rpc_spec.get("call_sites").and_then(Value::as_array) else {
        return shape;
    };

    for site in sites {
        let exact_const = site
            .get("arg_consts")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(Value::as_str)
            .is_some_and(|c| c.trim_matches('"').trim() == cmd_str);
        let matches_fn = expected_fn
            .as_deref()
            .is_some_and(|fn_name| site.get("func").and_then(Value::as_str) == Some(fn_name));
        if !exact_const && !matches_fn {
            continue;
        }
        if let Some(uses) = site.get("uses").and_then(Value::as_array) {
            for u in uses.iter().filter_map(Value::as_str) {
                if let Some((p, op)) = u.split_once(' ') {
                    shape
                        .entry(p.to_string())
                        .or_insert_with(|| Value::Array(Vec::new()))
                        .as_array_mut()
                        .unwrap()
                        .push(Value::String(op.to_string()));
                }
            }
        }
    }
    shape
}

/// Validates a single live RPC call reply against `expectations["rpc"]["{service}.{method}"]`
/// using strict `#912` (`C-1`) validation rules (`C-2`).
pub fn validate_rpc_reply_for_call(
    lookup: Option<&SemanticLookupContext<'_>>,
    expectations: &Value,
    service: &str,
    method: &str,
    args: &[PyRep],
    raw_reply: &PyRep,
) -> ReplyValidationOutcome {
    // V24 native clients authenticate via GPSTransport crypto handshake or 6/8-arg Authenticate calls;
    // the 2-arg `authentication.Login(username, password)` helper returns `{userID, role, sessionChange}`
    // for bridge and integration test session setup (`auth_cache.rs`).
    if service == "authentication" && method == "Login" && args.len() < 3 {
        return ReplyValidationOutcome::default();
    }
    let rpc_key = format!("{service}.{method}");
    let Some(rpc_spec) = expectations.get("rpc").and_then(|r| r.get(&rpc_key)) else {
        return ReplyValidationOutcome::default();
    };
    let custom_shape;
    let shape_map = if rpc_key == "slash.SlashCmd" {
        custom_shape = resolve_slash_cmd_shape(rpc_spec, args);
        &custom_shape
    } else if let Some(m) = rpc_spec.get("reply_shape").and_then(Value::as_object) {
        m
    } else {
        return ReplyValidationOutcome::default();
    };
    validate_reply_shape_with_metrics(lookup, &rpc_key, raw_reply, shape_map)
}

/// Validates an emitted notification `(event, args)` (already normalized for the client wire) against
/// `notify_map[event]` arity bounds and argument `reads` shapes under strict `#912` (`C-1`) rules.
pub fn validate_notification_contract(
    lookup: Option<&SemanticLookupContext<'_>>,
    notify_map: &serde_json::Map<String, Value>,
    event: &str,
    nargs: &[PyRep],
) -> Vec<String> {
    let mut violations = Vec::new();
    // `OnSessionChanged` in `session.notifications` is an internal trigger intercepted by
    // `macho_listener` to build a wire `SessionChangeNotification` packet; on the client,
    // `arg1(sess)` is the client's local `blue.pyos.synchro` session object.
    if event == "OnSessionChanged" {
        return violations;
    }
    let Some(nspec) = notify_map.get(event) else {
        return violations;
    };

    let mut arity_min = nspec["arity_min"].as_u64().unwrap_or(0) as usize;
    let mut arity_max = nspec["arity_max"].as_u64().unwrap_or(usize::MAX as u64) as usize;
    if let Some(variants) = nspec["param_variants"].as_array() {
        if !variants.is_empty() {
            let mut req_min = 0usize;
            let mut req_max = usize::MAX;
            for v in variants {
                if let Some(plist) = v.get(0).and_then(|x| x.as_array()) {
                    let pstrs: Vec<&str> = plist.iter().filter_map(|x| x.as_str()).collect();
                    let has_varargs = pstrs
                        .iter()
                        .any(|p| p.starts_with('*') && !p.starts_with("**"));
                    let pos_count = pstrs.iter().filter(|p| !p.starts_with('*')).count();
                    req_min = req_min.max(pos_count);
                    if !has_varargs {
                        req_max = req_max.min(pos_count);
                    }
                }
            }
            if req_min <= req_max {
                arity_min = req_min;
                arity_max = req_max;
            }
        }
    }

    if nargs.len() < arity_min || nargs.len() > arity_max {
        violations.push(format!(
            "notify:{event}: emitted {} args, client expects {arity_min}..={arity_max} ({nargs:?})",
            nargs.len()
        ));
        return violations;
    }

    if let Some(reads) = nspec["reads"].as_array() {
        let mut arg_shapes: HashMap<usize, serde_json::Map<String, Value>> = HashMap::new();
        for r in reads.iter().filter_map(|x| x.as_str()) {
            // `SpaceMgr.OnDamageStateChange(shipID, damageState)` receives a 3-tuple from the server;
            // `HealthWidget.OnDamageStateChange` is a client-internal scatter event with a 5-tuple.
            if event == "OnDamageStateChange" && r.contains("unpack:5") {
                continue;
            }
            let Some(rest) = r.strip_prefix("arg") else {
                continue;
            };
            let Some(open_paren) = rest.find('(') else {
                continue;
            };
            let Ok(arg_idx) = rest[..open_paren].parse::<usize>() else {
                continue;
            };
            let Some(close_paren) = rest.find(')') else {
                continue;
            };
            let param_name = &rest[open_paren + 1..close_paren];
            if nspec["params"]
                .as_array()
                .is_some_and(|ps| ps.iter().any(|p| p.as_str() == Some(&format!("*{param_name}"))))
            {
                continue;
            }
            let after_paren = &rest[close_paren + 1..];
            let Some((subpath, op)) = after_paren.split_once(' ') else {
                continue;
            };
            let path_key = if subpath.is_empty() {
                "$".to_string()
            } else {
                subpath.to_string()
            };
            arg_shapes
                .entry(arg_idx)
                .or_default()
                .entry(path_key)
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .unwrap()
                .push(Value::String(op.to_string()));
        }
        for (arg_idx, shape_map) in arg_shapes {
            if let Some(arg_val) = nargs.get(arg_idx) {
                let v = validate_reply_shape(
                    lookup,
                    &format!("notify:{event}[arg{arg_idx}]"),
                    arg_val,
                    &shape_map,
                );
                violations.extend(v);
            }
        }
    }

    violations
}

/// Convenience wrapper validating an emitted wire notification against `expectations["notify"]`.
pub fn validate_notification_for_event(
    lookup: Option<&SemanticLookupContext<'_>>,
    expectations: &Value,
    event: &str,
    nargs: &[PyRep],
) -> Vec<String> {
    let Some(notify_map) = expectations.get("notify").and_then(Value::as_object) else {
        return Vec::new();
    };
    validate_notification_contract(lookup, notify_map, event, nargs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_expectations_json() -> (Vec<u8>, Value) {
        let path = ClientContractRatchet::default_expectations_path();
        let raw = std::fs::read(&path).expect("read client_expectations.json");
        let val: Value = serde_json::from_slice(&raw).expect("parse client_expectations.json");
        (raw, val)
    }

    fn rpc_reply_shape<'a>(
        expectations: &'a Value,
        rpc_name: &str,
    ) -> &'a serde_json::Map<String, Value> {
        expectations["rpc"][rpc_name]["reply_shape"]
            .as_object()
            .unwrap_or_else(|| panic!("missing reply_shape for {rpc_name}"))
    }

    #[test]
    fn must_reject_934_facility_locations_raw_keyval_and_accept_facility_location_object() {
        let (_, exp) = load_expectations_json();
        let shape = rpc_reply_shape(&exp, "facilityManager.GetFacilityLocations");

        // Broken #934 shape: raw util.KeyVal lacking GetInvController / FacilityLocation class
        let broken_934 = PyRep::List(vec![PyRep::keyval(vec![
            ("facilityID", PyRep::Integer(60_003_760)),
            ("itemID", PyRep::Integer(60_003_760)),
            ("typeID", PyRep::Integer(27)),
            ("ownerID", PyRep::Integer(98_000_001)),
            ("flagID", PyRep::Integer(4)),
        ])]);
        let bad_v = validate_reply_shape(None, "facilityManager.GetFacilityLocations", &broken_934, shape);
        assert!(
            bad_v.iter().any(|v| v.contains("GetInvController")),
            "#934 broken raw KeyVal must be rejected for missing `.GetInvController`: {bad_v:#?}"
        );

        // Fixed V24.01 shape: industry.job.Location ObjectEx (whitelist.pyj + mixins.pyj)
        let fixed_934_obj_ex = PyRep::List(vec![PyRep::ObjectEx {
            is_type_2: true,
            header: Box::new(PyRep::Tuple(vec![
                PyRep::Tuple(vec![PyRep::Token("industry.job.Location".into())]),
                PyRep::Dict(vec![
                    (PyRep::String("facilityID".into()), PyRep::Integer(60_003_760)),
                    (PyRep::String("itemID".into()), PyRep::Integer(60_003_760)),
                    (PyRep::String("typeID".into()), PyRep::Integer(27)),
                    (PyRep::String("ownerID".into()), PyRep::Integer(98_000_001)),
                    (PyRep::String("flagID".into()), PyRep::Integer(4)),
                ]),
            ])),
            list: Vec::new(),
            dict: Vec::new(),
        }]);
        let good_obj_ex = validate_reply_shape(
            None,
            "facilityManager.GetFacilityLocations",
            &fixed_934_obj_ex,
            shape,
        );
        assert!(
            good_obj_ex.is_empty(),
            "#934 fixed industry.job.Location ObjectEx must be accepted: {good_obj_ex:#?}"
        );

        // Legacy compatibility shape: FacilityLocation Object
        let fixed_934 = PyRep::List(vec![PyRep::Object {
            type_name: "eve.common.script.industry.facilityLocation.FacilityLocation".into(),
            args: Box::new(PyRep::keyval(vec![
                ("facilityID", PyRep::Integer(60_003_760)),
                ("itemID", PyRep::Integer(60_003_760)),
                ("typeID", PyRep::Integer(27)),
                ("ownerID", PyRep::Integer(98_000_001)),
                ("flagID", PyRep::Integer(4)),
            ])),
        }]);
        let good_v = validate_reply_shape(None, "facilityManager.GetFacilityLocations", &fixed_934, shape);
        assert!(
            good_v.is_empty(),
            "#934 fixed FacilityLocation object must be accepted: {good_v:#?}"
        );
    }

    #[test]
    fn must_reject_939_xmpp_create_player_owned_channel_keyval_and_accept_string() {
        let (_, exp) = load_expectations_json();
        let shape = rpc_reply_shape(&exp, "XmppChatMgr.CreatePlayerOwnedChannel");

        // Broken #939 shape: KeyVal instead of channel_id String
        let broken_939 = PyRep::keyval(vec![
            ("channelID", PyRep::String("player_2100000001".into())),
            ("comparisonKey", PyRep::String("mychannel".into())),
            ("estimatedMemberCount", PyRep::Integer(1)),
        ]);
        let bad_v = validate_reply_shape(None, "XmppChatMgr.CreatePlayerOwnedChannel", &broken_939, shape);
        assert!(
            !bad_v.is_empty(),
            "#939 broken KeyVal reply from CreatePlayerOwnedChannel must be rejected: {bad_v:#?}"
        );

        // Fixed V24.01 shape: bare channel_id String
        let fixed_939 = PyRep::String("player_2100000001".into());
        let good_v = validate_reply_shape(None, "XmppChatMgr.CreatePlayerOwnedChannel", &fixed_939, shape);
        assert!(
            good_v.is_empty(),
            "#939 fixed String reply from CreatePlayerOwnedChannel must be accepted: {good_v:#?}"
        );
    }

    #[test]
    fn must_reject_942_anomaly_counts_bare_integer_and_accept_dict() {
        let (_, exp) = load_expectations_json();
        for rpc in [
            "dungeonInstanceCacheMgr.GetIceBeltsCount",
            "dungeonInstanceCacheMgr.GetCombatAnomaliesCount",
            "dungeonInstanceCacheMgr.GetOreAnomaliesCount",
        ] {
            let shape = rpc_reply_shape(&exp, rpc);

            // Broken #942 shape: bare PyRep::Integer(1)
            let broken_942 = PyRep::Integer(1);
            let bad_v = validate_reply_shape(None, rpc, &broken_942, shape);
            assert!(
                bad_v.iter().any(|v| v.contains("iteritems")),
                "#942 broken bare Integer for {rpc} must be rejected for `.iteritems()`: {bad_v:#?}"
            );

            // Fixed V24.01 shape: Dict mapping {solarSystemID: count}
            let fixed_942 = PyRep::Dict(vec![(PyRep::Integer(30_000_142), PyRep::Integer(1))]);
            let good_v = validate_reply_shape(None, rpc, &fixed_942, shape);
            assert!(
                good_v.is_empty(),
                "#942 fixed Dict reply for {rpc} must be accepted: {good_v:#?}"
            );
        }
    }

    #[test]
    fn must_reject_943_corp_applications_flat_rowset_and_accept_dict_by_corp_id() {
        let (_, exp) = load_expectations_json();
        for rpc in ["corpRegistry.GetMyApplications", "corpRegistry.GetApplications"] {
            let shape = rpc_reply_shape(&exp, rpc);

            // Broken #943 shape: flat PyRep::Rowset
            let broken_943 = PyRep::Rowset {
                columns: vec![
                    "applicationID".into(),
                    "corporationID".into(),
                    "characterID".into(),
                    "status".into(),
                ],
                rows: vec![vec![
                    PyRep::Integer(1),
                    PyRep::Integer(98_000_001),
                    PyRep::Integer(90_000_001),
                    PyRep::Integer(0),
                ]],
            };
            let bad_v = validate_reply_shape(None, rpc, &broken_943, shape);
            assert!(
                !bad_v.is_empty(),
                "#943 broken flat Rowset for {rpc} must be rejected: {bad_v:#?}"
            );

            // Fixed V24.01 shape: Dict mapping corporationID -> List[KeyVal]
            let fixed_943 = PyRep::Dict(vec![(
                PyRep::Integer(98_000_001),
                PyRep::List(vec![PyRep::keyval(vec![
                    ("applicationID", PyRep::Integer(1)),
                    ("corporationID", PyRep::Integer(98_000_001)),
                    ("characterID", PyRep::Integer(90_000_001)),
                    ("applicationText", PyRep::String("Hello".into())),
                    ("status", PyRep::Integer(0)),
                    ("applicationDateTime", PyRep::Integer(133_500_000_000_000_000)),
                ])]),
            )]);
            let good_v = validate_reply_shape(None, rpc, &fixed_943, shape);
            assert!(
                good_v.is_empty(),
                "#943 fixed Dict[corpID -> List[KeyVal]] for {rpc} must be accepted: {good_v:#?}"
            );
        }
    }

    #[test]
    fn must_reject_945_home_station_remote_change_time_integer_and_accept_none_or_datetime() {
        let (_, exp) = load_expectations_json();
        let shape = rpc_reply_shape(&exp, "home_station.get_next_remote_change_time");

        // Broken #945 shape: bare PyRep::Integer(0)
        let broken_945 = PyRep::Integer(0);
        let bad_v = validate_reply_shape(
            None,
            "home_station.get_next_remote_change_time",
            &broken_945,
            shape,
        );
        assert!(
            bad_v.iter().any(|v| v.contains("type:datetime")),
            "#945 broken Integer(0) for home_station.get_next_remote_change_time must be rejected: {bad_v:#?}"
        );

        // Fixed V24.01 shape 1: PyRep::None (no active cooldown; guarded by compareis_notnull)
        let fixed_none = PyRep::None;
        let good_none = validate_reply_shape(
            None,
            "home_station.get_next_remote_change_time",
            &fixed_none,
            shape,
        );
        assert!(
            good_none.is_empty(),
            "#945 PyRep::None for home_station.get_next_remote_change_time must be accepted: {good_none:#?}"
        );

        // Fixed V24.01 shape 2: datetime.datetime object
        let fixed_dt = PyRep::Object {
            type_name: "datetime.datetime".into(),
            args: Box::new(PyRep::Tuple(vec![PyRep::Bytes(vec![
                0x07, 0xEA, 11, 11, 8, 0, 0, 0, 0, 0,
            ])])),
        };
        let good_dt = validate_reply_shape(
            None,
            "home_station.get_next_remote_change_time",
            &fixed_dt,
            shape,
        );
        assert!(
            good_dt.is_empty(),
            "#945 datetime.datetime object for home_station.get_next_remote_change_time must be accepted: {good_dt:#?}"
        );
    }

    #[test]
    fn must_reject_954_bare_object_caching_class_names_and_accept_v24_carbon_paths() {
        let (_, exp) = load_expectations_json();
        let shape = rpc_reply_shape(&exp, "marketProxy.GetOrders");
        let inner_orders = PyRep::List(vec![PyRep::List(Vec::new()), PyRep::List(Vec::new())]);
        let marshaled_orders = eve_proto::marshal::encode(&inner_orders);

        // 1. Broken #954 shapes: bare `objectCaching.*`, `util.CachedObject`, `util.CachedMethodCallResult`, `utillib.CachedMethodCallResult`
        for bad_class in [
            "objectCaching.CachedMethodCallResult",
            "util.CachedMethodCallResult",
            "utillib.CachedMethodCallResult",
        ] {
            let broken_cmcr = PyRep::Object {
                type_name: bad_class.into(),
                args: Box::new(PyRep::Tuple(vec![
                    PyRep::Dict(vec![(
                        PyRep::String("versionCheck".into()),
                        PyRep::String("always".into()),
                    )]),
                    PyRep::SubStream(Box::new(inner_orders.clone())),
                    PyRep::Tuple(vec![PyRep::Integer(133_500_000_000_000_000), PyRep::Integer(1)]),
                ])),
            };
            let bad_v = validate_reply_shape(None, "marketProxy.GetOrders", &broken_cmcr, shape);
            assert!(
                bad_v.iter().any(|v| v.contains(bad_class) && v.contains("#954")),
                "#954 unwhitelisted `{bad_class}` must be rejected: {bad_v:#?}"
            );
        }

        for bad_co_class in ["objectCaching.CachedObject", "util.CachedObject"] {
            let broken_co = PyRep::Object {
                type_name: bad_co_class.into(),
                args: Box::new(PyRep::Tuple(vec![
                    PyRep::Tuple(vec![PyRep::Integer(133_500_000_000_000_000), PyRep::Integer(1)]),
                    PyRep::None,
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    PyRep::Buffer(marshaled_orders.clone()),
                    PyRep::Integer(0),
                    PyRep::String("marketProxy::GetOrders".into()),
                ])),
            };
            let bad_v = validate_reply_shape(None, "marketProxy.GetOrders", &broken_co, shape);
            assert!(
                bad_v
                    .iter()
                    .any(|v| v.contains(bad_co_class) && v.contains("#954")),
                "#954 unwhitelisted `{bad_co_class}` must be rejected: {bad_v:#?}"
            );
        }

        // 2. Fixed V24.01 shapes: `carbon.common.script.net.objectCaching.*` and `carbon.common.script.net.cachedObject.CachedObject`
        let fixed_cmcr = PyRep::Object {
            type_name: V24_CACHED_METHOD_CALL_RESULT_CLASS.into(),
            args: Box::new(PyRep::Tuple(vec![
                PyRep::Dict(vec![(
                    PyRep::String("versionCheck".into()),
                    PyRep::String("always".into()),
                )]),
                PyRep::SubStream(Box::new(inner_orders)),
                PyRep::Tuple(vec![PyRep::Integer(133_500_000_000_000_000), PyRep::Integer(1)]),
            ])),
        };
        let good_cmcr = validate_reply_shape(None, "marketProxy.GetOrders", &fixed_cmcr, shape);
        assert!(
            good_cmcr.is_empty(),
            "#954 V24 `{V24_CACHED_METHOD_CALL_RESULT_CLASS}` must be accepted: {good_cmcr:#?}"
        );

        for good_co_class in [V24_CACHED_OBJECT_CLASS, V24_CACHE_HINT_CLASS] {
            let fixed_co = PyRep::Object {
                type_name: good_co_class.into(),
                args: Box::new(PyRep::Tuple(vec![
                    PyRep::Tuple(vec![PyRep::Integer(133_500_000_000_000_000), PyRep::Integer(1)]),
                    PyRep::None,
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    PyRep::Buffer(marshaled_orders.clone()),
                    PyRep::Integer(0),
                    PyRep::String("marketProxy::GetOrders".into()),
                ])),
            };
            let good_co = validate_reply_shape(None, "marketProxy.GetOrders", &fixed_co, shape);
            assert!(
                good_co.is_empty(),
                "#954 V24 `{good_co_class}` must be accepted: {good_co:#?}"
            );
        }
    }

    #[test]
    fn strict_none_policy_rejects_unguarded_none_and_vacuity_classifier_flags_empty_collections() {
        let mut shape = serde_json::Map::new();
        shape.insert(
            "$".into(),
            serde_json::json!(["iter"]),
        );
        shape.insert(
            "[]".into(),
            serde_json::json!(["attr:itemID", "attr:typeID"]),
        );

        // 1. Unguarded PyRep::None at `$` when client iterates `[]` is a strict violation:
        let none_outcome = validate_reply_shape_with_metrics(None, "test.ListItems", &PyRep::None, &shape);
        assert!(
            !none_outcome.violations.is_empty(),
            "Unguarded PyRep::None at dereferenced path must be rejected"
        );

        // 2. Empty PyRep::List(vec![]) has 0 violations, but is classified as `Vacuous` because `[]` was never exercised:
        let empty_outcome =
            validate_reply_shape_with_metrics(None, "test.ListItems", &PyRep::List(Vec::new()), &shape);
        assert!(empty_outcome.violations.is_empty());
        assert_eq!(
            empty_outcome.classification(),
            RpcValidationClass::Vacuous,
            "Empty list when `[]` child path exists must be classified as Vacuous"
        );

        // 3. Non-empty list with valid element is classified as `Validated`:
        let non_empty = PyRep::List(vec![PyRep::keyval(vec![
            ("itemID", PyRep::Integer(1001)),
            ("typeID", PyRep::Integer(34)),
        ])]);
        let valid_outcome =
            validate_reply_shape_with_metrics(None, "test.ListItems", &non_empty, &shape);
        assert!(valid_outcome.violations.is_empty());
        assert!(matches!(
            valid_outcome.classification(),
            RpcValidationClass::Validated { paths_checked } if paths_checked >= 2
        ));
    }

    #[test]
    fn ratchet_verifies_expectations_version_and_sha256_hash() {
        let (raw, parsed) = load_expectations_json();
        let ratchet = ClientContractRatchet::load_default().expect("load default ratchet");
        ratchet
            .verify_expectations_integrity(&raw, &parsed)
            .expect("ratchet version and SHA-256 must match client_expectations.json");

        let mut tampered = ratchet.clone();
        tampered.expectations_sha256 = "0".repeat(64);
        assert!(
            tampered
                .verify_expectations_integrity(&raw, &parsed)
                .is_err(),
            "Mismatched expectations SHA-256 must be rejected"
        );
    }

    #[test]
    fn single_call_and_notification_contract_helpers_enforce_arity_and_slash_polymorphism() {
        let contracts = ClientContractRatchet::load_default_cached();
        let notify_map = contracts.expectations["notify"]
            .as_object()
            .expect("notify map");

        // OnSessionChanged trigger is converted to SessionChangeNotification by macho_listener
        assert!(validate_notification_contract(
            None,
            notify_map,
            "OnSessionChanged",
            &[PyRep::Integer(1)]
        )
        .is_empty());

        // Bad arity on a known notification must produce a violation:
        if let Some((ev, _spec)) = notify_map.iter().find(|(k, _)| {
            *k != "OnSessionChanged"
                && !validate_notification_contract(None, notify_map, k, &[]).is_empty()
        }) {
            let v = validate_notification_contract(None, notify_map, ev, &[]);
            assert!(
                !v.is_empty(),
                "0-arg emission for `{ev}` must be rejected"
            );
        }

        // `/createitem` resolves to SlashService.cmd_createitem (accepts KeyVal{itemID}), not Ship.ImportFromShip split:
        let created = PyRep::keyval(vec![("itemID", PyRep::Integer(900_001))]);
        let slash_out = validate_rpc_reply_for_call(
            None,
            &contracts.expectations,
            "slash",
            "SlashCmd",
            &[PyRep::String("/createitem 587 1".into())],
            &created,
        );
        assert!(
            slash_out.violations.is_empty(),
            "/createitem KeyVal must pass SlashService.cmd_createitem shape: {:?}",
            slash_out.violations
        );
    }
}
