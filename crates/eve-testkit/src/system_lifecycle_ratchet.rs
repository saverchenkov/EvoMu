//! Per-file falling ratchets for `#1542` solar-system lifecycle migration:
//! 1. `get_or_spawn` / `get_or_spawn_if_runtime` calls outside definitions across non-test
//!    `crates/eve-server/src/`.
//! 2. Calls under non-test `crates/eve-server/src/services/` to the legacy "seed if missing"
//!    helpers listed in `#1542`.
//!
//! Baselines use the v2 per-file ratchet schema (`#1538`, matching [`crate::arch_debt_ratchet`]):
//! files may shrink to zero (and be omitted), existing files may not increase, and unratcheted
//! files with positive counts are rejected.

#![forbid(unsafe_code)]

use crate::arch_debt_ratchet::{validate_per_file_baseline, validate_per_file_counts};
use crate::client_harness::{is_ident_byte, strip_rust_comments_and_literals};
use crate::notify_ratchet::{
    collect_rs_files_recursive, is_fn_def_header, is_test_only_rel_path, strip_cfg_test_items,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Rule key tracking legacy `get_or_spawn` / `get_or_spawn_if_runtime` calls across non-test `crates/eve-server/src`.
pub const RULE_GET_OR_SPAWN: &str = "get_or_spawn_calls_outside_wake_entry_points";

/// Rule key tracking calls to `#1542`'s 13 "seed if missing" helpers under non-test `crates/eve-server/src/services`.
pub const RULE_SEED_IF_MISSING: &str = "seed_if_missing_calls_in_services";

/// Exact helper identifiers tracked by [`RULE_SEED_IF_MISSING`] under `crates/eve-server/src/services/`.
pub const SEED_IF_MISSING_HELPERS: &[&str] = &[
    "ensure_system_pve_sites_seeded",
    "ensure_special_sites_initialized",
    "ensure_dungeon_rooms_seeded",
    "ensure_all_mentioned_dungeon_seeded",
    "ensure_homefront_sites_for_system_locked",
    "ensure_homefront_sites_for_system_locked_with_seed",
    "ensure_store_catalog_seeded",
    "ensure_social_extras_mailing_lists_seeded",
    "ensure_corp_rec_ads_populated_in_snap",
    "seed_component_listings_if_empty",
    "ensure_rookie_overview_seeded",
    "ensure_sde_initialized",
    "ensure_client_flatbuffer_storage_seeded",
];

/// Version-2 per-file ratchet specification (`crates/eve-testkit/ratchets/system_lifecycle_ratchet.json`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemLifecycleRatchet {
    pub version: u32,
    pub description: String,
    pub rules: BTreeMap<String, LifecycleRuleRatchet>,
}

/// Per-rule baseline map inside [`SystemLifecycleRatchet`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleRuleRatchet {
    pub zero_required: bool,
    pub owning_issue: u32,
    #[serde(default)]
    pub path_include: Option<String>,
    pub regex: String,
    pub description: String,
    pub files: BTreeMap<String, usize>,
}

/// Observed vs baseline counts for a single lifecycle ratchet rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleRuleReport {
    pub baseline_total: usize,
    pub observed_total: usize,
    pub observed: BTreeMap<String, usize>,
}

/// Aggregate verification summary returned by [`SystemLifecycleRatchet::verify_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemLifecycleReport {
    pub scanned_files: usize,
    pub rules: BTreeMap<String, LifecycleRuleReport>,
}

/// Default workspace path to `crates/eve-testkit/ratchets/system_lifecycle_ratchet.json`.
pub fn default_ratchet_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("ratchets")
        .join("system_lifecycle_ratchet.json")
}

/// Default workspace path to `crates/eve-server/src`.
pub fn default_server_src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("eve-server")
        .join("src")
}

fn has_word_boundary_before(b: &[u8], pos: usize) -> bool {
    pos == 0 || !is_ident_byte(b[pos - 1])
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

fn is_call_like_suffix(b: &[u8], ident_end: usize) -> bool {
    let pos = skip_ws(b, ident_end);
    if pos < b.len() && b[pos] == b'(' {
        return true;
    }
    // Parenthesized UFCS call: `(SystemRegistry::get_or_spawn)(...)`
    if pos < b.len() && b[pos] == b')' {
        let after = skip_ws(b, pos + 1);
        return after < b.len() && b[after] == b'(';
    }
    // Optional turbofish before call: `helper::<...>(...)`
    if pos + 1 < b.len() && b[pos] == b':' && b[pos + 1] == b':' {
        let mut cur = skip_ws(b, pos + 2);
        if cur < b.len() && b[cur] == b'<' {
            let mut depth = 1usize;
            cur += 1;
            while cur < b.len() && depth > 0 {
                if b[cur] == b'<' {
                    depth += 1;
                } else if b[cur] == b'>' {
                    depth -= 1;
                }
                cur += 1;
            }
            cur = skip_ws(b, cur);
            return cur < b.len() && b[cur] == b'(';
        }
    }
    false
}

/// Counts non-definition calls matching exact `candidates` where identifiers begin with one of `prefixes`.
fn count_ident_calls(text: &str, prefixes: &[&str], candidates: &[&str]) -> usize {
    if !prefixes.iter().any(|p| text.contains(p)) {
        return 0;
    }
    let b = text.as_bytes();
    let mut count = 0usize;
    for prefix in prefixes {
        if !text.contains(prefix) {
            continue;
        }
        for (i, _) in text.match_indices(prefix) {
            if !has_word_boundary_before(b, i) {
                continue;
            }
            let mut ident_end = i + prefix.len();
            while ident_end < b.len() && is_ident_byte(b[ident_end]) {
                ident_end += 1;
            }
            if let Ok(ident) = std::str::from_utf8(&b[i..ident_end]) {
                if candidates.contains(&ident)
                    && !is_fn_def_header(b, i)
                    && is_call_like_suffix(b, ident_end)
                {
                    count += 1;
                }
            }
        }
    }
    count
}

/// Counts non-definition `get_or_spawn` / `get_or_spawn_if_runtime` calls in stripped production code.
pub fn count_get_or_spawn_calls(prod_src: &str) -> usize {
    count_ident_calls(
        prod_src,
        &["get_or_spawn"],
        &["get_or_spawn", "get_or_spawn_if_runtime"],
    )
}

/// Counts non-definition `#1542` seed-if-missing helper calls in stripped production `services/` code.
pub fn count_seed_if_missing_calls(prod_src: &str) -> usize {
    count_ident_calls(prod_src, &["ensure_", "seed_"], SEED_IF_MISSING_HELPERS)
}

#[derive(Default)]
struct PerFileScanCounts {
    get_or_spawn_count: usize,
    seed_helper_count: usize,
}

fn scan_production_file(rel: &str, path: &Path) -> Result<PerFileScanCounts, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let stripped = strip_rust_comments_and_literals(&raw);
    let prod = strip_cfg_test_items(&stripped);
    let get_or_spawn_count = count_get_or_spawn_calls(&prod);
    let seed_helper_count = if rel.starts_with("services/") {
        count_seed_if_missing_calls(&prod)
    } else {
        0
    };
    Ok(PerFileScanCounts {
        get_or_spawn_count,
        seed_helper_count,
    })
}

impl SystemLifecycleRatchet {
    /// Loads [`default_ratchet_path`] from disk.
    pub fn load_default() -> Result<Self, String> {
        Self::load_from_path(&default_ratchet_path())
    }

    /// Loads and deserializes a [`SystemLifecycleRatchet`] JSON file from `path`.
    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))
    }

    /// Verifies the live `crates/eve-server/src` tree against this baseline.
    pub fn verify_default(&self) -> Result<SystemLifecycleReport, String> {
        self.verify_dir(&default_server_src_dir())
    }

    /// Scans `server_src_dir` (non-test Rust files) and verifies both `#1542` rules hold.
    pub fn verify_dir(&self, server_src_dir: &Path) -> Result<SystemLifecycleReport, String> {
        if self.version != 2 {
            return Err(format!(
                "unsupported system_lifecycle_ratchet version {} (expected 2)",
                self.version
            ));
        }
        if self.description.trim().is_empty() {
            return Err("system_lifecycle_ratchet must declare non-empty description".to_string());
        }
        for required_rule in [RULE_GET_OR_SPAWN, RULE_SEED_IF_MISSING] {
            if !self.rules.contains_key(required_rule) {
                return Err(format!(
                    "system_lifecycle_ratchet missing required rule '{required_rule}'"
                ));
            }
        }
        for (rule_name, rule) in &self.rules {
            if rule_name != RULE_GET_OR_SPAWN && rule_name != RULE_SEED_IF_MISSING {
                return Err(format!(
                    "unknown rule '{rule_name}' in system_lifecycle_ratchet"
                ));
            }
            if rule.owning_issue == 0 {
                return Err(format!(
                    "rule '{rule_name}' must declare non-zero owning_issue"
                ));
            }
            if rule.regex.trim().is_empty() || rule.description.trim().is_empty() {
                return Err(format!(
                    "rule '{rule_name}' must declare non-empty regex and description"
                ));
            }
            validate_per_file_baseline(rule_name, rule.zero_required, &rule.files)?;
            if let Some(ref prefix) = rule.path_include {
                for rel in rule.files.keys() {
                    if !rel.starts_with(prefix.as_str()) {
                        return Err(format!(
                            "rule '{rule_name}' baseline entry '{rel}' must be under '{prefix}'"
                        ));
                    }
                }
            }
            if rule_name == RULE_SEED_IF_MISSING && rule.path_include.as_deref() != Some("services/")
            {
                return Err(format!(
                    "rule '{rule_name}' must scope path_include to 'services/'"
                ));
            }
        }

        let mut all_files = Vec::new();
        collect_rs_files_recursive(server_src_dir, server_src_dir, &mut all_files)?;
        let prod_files: Vec<(String, PathBuf)> = all_files
            .into_iter()
            .filter(|(rel, _)| !is_test_only_rel_path(rel))
            .collect();
        let scanned_files = prod_files.len();

        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(1, 8)
            .min(prod_files.len().max(1));
        let chunk_size = prod_files.len().div_ceil(workers).max(1);
        let worker_results: Vec<Result<Vec<(String, PerFileScanCounts)>, String>> =
            std::thread::scope(|scope| {
                let mut handles = Vec::new();
                for chunk in prod_files.chunks(chunk_size) {
                    handles.push(scope.spawn(move || -> Result<Vec<(String, PerFileScanCounts)>, String> {
                        let mut out = Vec::with_capacity(chunk.len());
                        for (rel, path) in chunk {
                            let counts = scan_production_file(rel, path)?;
                            if counts.get_or_spawn_count > 0 || counts.seed_helper_count > 0 {
                                out.push((rel.clone(), counts));
                            }
                        }
                        Ok(out)
                    }));
                }
                handles
                    .into_iter()
                    .map(|h| h.join().unwrap_or_else(|_| Err("worker thread panicked".into())))
                    .collect()
            });

        let mut observed_spawn: BTreeMap<String, usize> = BTreeMap::new();
        let mut observed_seed: BTreeMap<String, usize> = BTreeMap::new();
        for res in worker_results {
            for (rel, counts) in res? {
                if counts.get_or_spawn_count > 0 {
                    observed_spawn.insert(rel.clone(), counts.get_or_spawn_count);
                }
                if counts.seed_helper_count > 0 {
                    observed_seed.insert(rel, counts.seed_helper_count);
                }
            }
        }

        let mut reports = BTreeMap::new();
        let mut errors = Vec::new();

        for (rule_name, observed) in [
            (RULE_GET_OR_SPAWN, observed_spawn),
            (RULE_SEED_IF_MISSING, observed_seed),
        ] {
            let rule = &self.rules[rule_name];
            if let Err(err) =
                validate_per_file_counts(rule_name, rule.zero_required, &rule.files, &observed)
            {
                errors.push(err);
            }
            reports.insert(
                rule_name.to_string(),
                LifecycleRuleReport {
                    baseline_total: rule.files.values().sum(),
                    observed_total: observed.values().sum(),
                    observed,
                },
            );
        }

        if errors.is_empty() {
            Ok(SystemLifecycleReport {
                scanned_files,
                rules: reports,
            })
        } else {
            Err(errors.join("\n\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDirGuard(PathBuf);

    impl TempDirGuard {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "evomu_sys_lifecycle_ratchet_{label}_{}_{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("create tempdir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_rel(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(&p, body).expect("write test rust file");
    }

    fn synthetic_ratchet(
        spawn_files: BTreeMap<String, usize>,
        seed_files: BTreeMap<String, usize>,
    ) -> SystemLifecycleRatchet {
        let mut rules = BTreeMap::new();
        rules.insert(
            RULE_GET_OR_SPAWN.to_string(),
            LifecycleRuleRatchet {
                zero_required: false,
                owning_issue: 1542,
                path_include: None,
                regex: "\\bget_or_spawn(?:_if_runtime)?\\s*\\(".to_string(),
                description: "synthetic spawn ratchet".to_string(),
                files: spawn_files,
            },
        );
        rules.insert(
            RULE_SEED_IF_MISSING.to_string(),
            LifecycleRuleRatchet {
                zero_required: false,
                owning_issue: 1542,
                path_include: Some("services/".to_string()),
                regex: "synthetic".to_string(),
                description: "synthetic seed ratchet".to_string(),
                files: seed_files,
            },
        );
        SystemLifecycleRatchet {
            version: 2,
            description: "synthetic".to_string(),
            rules,
        }
    }

    #[test]
    fn system_lifecycle_ratchet_holds_against_live_server_src() {
        let ratchet = SystemLifecycleRatchet::load_default()
            .expect("load system_lifecycle_ratchet.json");
        assert_eq!(ratchet.version, 2);
        assert_eq!(ratchet.rules.len(), 2);
        for rule in ratchet.rules.values() {
            assert_eq!(rule.owning_issue, 1542);
        }

        let report = ratchet
            .verify_default()
            .expect("live crates/eve-server/src must satisfy system_lifecycle_ratchet.json");
        assert!(report.scanned_files >= 100);
        for name in [RULE_GET_OR_SPAWN, RULE_SEED_IF_MISSING] {
            let rule_report = &report.rules[name];
            assert!(
                rule_report.observed_total <= rule_report.baseline_total,
                "rule '{name}' total regressed above baseline"
            );
        }

        let spawn_obs = &report.rules[RULE_GET_OR_SPAWN].observed;
        assert!(
            !spawn_obs.contains_key("system_actor/lifecycle.rs"),
            "system_actor/lifecycle.rs must never call legacy get_or_spawn"
        );
        assert!(
            spawn_obs.get("system_actor.rs").copied().unwrap_or(0) <= 2,
            "system_actor.rs must have at most the unmigrated stargate-intent and get_or_spawn_if_runtime callers"
        );
    }

    #[test]
    fn scanner_catches_disguised_calls_and_ignores_comments_strings_tests_and_fn_defs() {
        let temp = TempDirGuard::new("disguised");
        write_rel(
            temp.path(),
            "mod_a.rs",
            r##"
// ctx.systems.get_or_spawn(30_000_142);
/* ctx.systems.get_or_spawn_if_runtime(30_000_142); */
pub fn get_or_spawn(&self, id: u32) {}
pub fn get_or_spawn_if_runtime(&self, id: u32) {}
pub fn disguised_callers(ctx: &Ctx, sys_id: u32) {
    let _s1 = "ctx.systems.get_or_spawn(sys_id)";
    let _s2 = r#"ctx.systems.get_or_spawn_if_runtime(sys_id)"#;
    ctx.systems
        .get_or_spawn
        (sys_id);
    ctx.systems.get_or_spawn_if_runtime /* inline comment */ (sys_id);
    SystemRegistry::get_or_spawn(&ctx.systems, sys_id);
    (SystemRegistry::get_or_spawn_if_runtime)(&ctx.systems, sys_id);
}
#[cfg(test)]
mod tests {
    #[test]
    fn test_only_spawn() {
        ctx.systems.get_or_spawn(30_000_142);
    }
}
"##,
        );
        write_rel(
            temp.path(),
            "mod_a_tests.rs",
            "fn t() { ctx.systems.get_or_spawn(1); }\n",
        );
        write_rel(
            temp.path(),
            "services/helper_caller.rs",
            r#"
use crate::services::quasar_payment_store_types::ensure_store_catalog_seeded;
pub async fn ensure_system_pve_sites_seeded() {}
// ensure_rookie_overview_seeded(ctx);
pub async fn handler(ctx: &Ctx, sys_id: u32) {
    let _lit = "ensure_corp_rec_ads_populated_in_snap(snap)";
    Box::pin(ensure_system_pve_sites_seeded /* sneaky */ (ctx, sys_id)).await;
    crate::services::pve_homefront_sites::ensure_homefront_sites_for_system_locked
        (ctx, reg, sys_id);
    (ensure_client_flatbuffer_storage_seeded)("id", "v1", b"1");
}
#[cfg(test)]
fn cfg_test_helper(ctx: &Ctx) {
    ensure_special_sites_initialized(ctx);
}
"#,
        );
        write_rel(
            temp.path(),
            "services/helper_caller_tests.rs",
            "fn t() { ensure_store_catalog_seeded(snap, sde); }\n",
        );
        write_rel(
            temp.path(),
            "outside_services.rs",
            "fn startup(snap: &mut Snap, sde: &Sde) { ensure_store_catalog_seeded(snap, sde); }\n",
        );

        let mut spawn_baseline = BTreeMap::new();
        spawn_baseline.insert("mod_a.rs".to_string(), 4);
        let mut seed_baseline = BTreeMap::new();
        seed_baseline.insert("services/helper_caller.rs".to_string(), 3);

        let ratchet = synthetic_ratchet(spawn_baseline, seed_baseline);
        let report = ratchet
            .verify_dir(temp.path())
            .expect("synthetic disguised-call baseline should match exact counts");
        assert_eq!(report.rules[RULE_GET_OR_SPAWN].observed_total, 4);
        assert_eq!(report.rules[RULE_SEED_IF_MISSING].observed_total, 3);
    }

    #[test]
    fn ratchet_rejects_regression_and_accepts_reduction() {
        let temp = TempDirGuard::new("regression");
        write_rel(
            temp.path(),
            "system_actor.rs",
            r#"
pub fn wake_for_session_arrival(&self, id: u32) {
    self.get_or_spawn(id);
}
"#,
        );
        write_rel(
            temp.path(),
            "services/pve_svc.rs",
            "fn f() { ensure_special_sites_initialized(ctx); }\n",
        );

        // Baseline allows 1 in system_actor.rs? No, say baseline dropped to 0, or test +1 increase:
        let mut spawn_exact = BTreeMap::new();
        spawn_exact.insert("system_actor.rs".to_string(), 1);
        let mut seed_exact = BTreeMap::new();
        seed_exact.insert("services/pve_svc.rs".to_string(), 1);

        // 1. Reduction succeeds when baseline has higher counts.
        let mut higher_spawn = BTreeMap::new();
        higher_spawn.insert("system_actor.rs".to_string(), 2);
        higher_spawn.insert("removed_caller.rs".to_string(), 1);
        let mut higher_seed = BTreeMap::new();
        higher_seed.insert("services/pve_svc.rs".to_string(), 3);
        let higher_ratchet = synthetic_ratchet(higher_spawn, higher_seed);
        assert!(higher_ratchet.verify_dir(temp.path()).is_ok());

        // 2. Increase (+1 in existing file) is rejected.
        write_rel(
            temp.path(),
            "system_actor.rs",
            r#"
pub fn wake_for_session_arrival(&self, id: u32) {
    self.get_or_spawn(id);
    self.get_or_spawn_if_runtime(id);
}
"#,
        );
        let err = synthetic_ratchet(spawn_exact.clone(), seed_exact.clone())
            .verify_dir(temp.path())
            .expect_err("+1 get_or_spawn call must fail ratchet");
        assert!(err.contains(RULE_GET_OR_SPAWN));
        assert!(err.contains("system_actor.rs"));

        // 3. New file not present in seed baseline is rejected.
        write_rel(
            temp.path(),
            "system_actor.rs",
            "pub fn f(&self, id: u32) { self.get_or_spawn(id); }\n",
        );
        write_rel(
            temp.path(),
            "services/new_handler.rs",
            "fn g() { ensure_sde_initialized(sde); }\n",
        );
        let err2 = synthetic_ratchet(spawn_exact, seed_exact)
            .verify_dir(temp.path())
            .expect_err("new seed-if-missing file must fail ratchet");
        assert!(err2.contains(RULE_SEED_IF_MISSING));
        assert!(err2.contains("services/new_handler.rs"));
    }

    #[test]
    fn json_schema_rejects_unknown_fields_and_zero_baseline_entries() {
        let bad_json = r#"{
            "version": 2,
            "description": "bad",
            "extra_field": true,
            "rules": {}
        }"#;
        assert!(serde_json::from_str::<SystemLifecycleRatchet>(bad_json).is_err());

        let temp = TempDirGuard::new("zero_entry");
        write_rel(temp.path(), "a.rs", "");
        let mut zero_spawn = BTreeMap::new();
        zero_spawn.insert("a.rs".to_string(), 0);
        let ratchet = synthetic_ratchet(zero_spawn, BTreeMap::new());
        assert!(
            ratchet.verify_dir(temp.path()).is_err(),
            "non-zero baseline map must reject explicit 0 entry"
        );
    }
}
