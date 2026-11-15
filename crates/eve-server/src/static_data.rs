//! Fail-fast startup validation for client-derived static databases (`#1119`).
//!
//! Ensures `data/evomu_sde.sqlite` matches `data/sde_import_manifest.json` (`_evomu_sde_meta`) and
//! `data/quasar_descriptors.sqlite` matches `data/quasar_descriptor_manifest.json` before binding
//! any network listeners unless an explicit dev/test override flag or env var is supplied.

#![forbid(unsafe_code)]

use crate::config::ServerConfig;
use eve_proto::quasar::{sha256_hex, QuasarDescriptorRegistry};
use eve_sde::SdeRegistry;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::warn;

pub const ALLOW_STALE_STATIC_DATA_FLAG: &str = "--allow-stale-static-data";
pub const ALLOW_STALE_STATIC_DATA_ENV: &str = "EVOMU_ALLOW_STALE_STATIC_DATA";

/// Validated static databases loaded during `eve-server` startup.
pub struct ValidatedStartupStaticData {
    pub sde: Arc<SdeRegistry>,
    pub quasar_descriptors: Arc<QuasarDescriptorRegistry>,
}

#[derive(Debug, Deserialize)]
struct SdeImportManifest {
    client_build: u64,
    client_release: String,
    #[serde(default)]
    resource_count: Option<usize>,
    #[serde(default)]
    combined_resource_sha256: Option<String>,
    #[serde(default)]
    resources: Vec<SdeManifestResource>,
}

#[derive(Debug, Clone, Deserialize)]
struct SdeManifestResource {
    res_path: String,
    md5: String,
    byte_size: u64,
}

fn format_static_data_error(reason: &str) -> String {
    format!(
        "Static data startup validation failed: {reason}\n\
         \n\
         Generate or refresh both `data/evomu_sde.sqlite` and `data/quasar_descriptors.sqlite` \
         from your EVE Online client SharedCache:\n\
         \n\
           # macOS (Apple Silicon runs under Rosetta):\n\
           arch -x86_64 /usr/bin/python3 tools/sde-import/import_sde.py\n\
         \n\
           # Windows PowerShell:\n\
           python tools/sde-import/import_sde.py --shared-cache \"C:/Games/EVE\"\n\
         \n\
         For synthetic/dev test fixtures only, bypass with `{ALLOW_STALE_STATIC_DATA_FLAG}` \
         or `{ALLOW_STALE_STATIC_DATA_ENV}=1`."
    )
}

fn compute_sde_resources_sha256(resources: &[SdeManifestResource]) -> String {
    let mut sorted = resources.to_vec();
    sorted.sort_by(|a, b| {
        (&a.res_path, &a.md5, a.byte_size).cmp(&(&b.res_path, &b.md5, b.byte_size))
    });
    let mut buf = Vec::with_capacity(sorted.len().saturating_mul(64));
    for r in &sorted {
        use std::io::Write;
        let _ = writeln!(buf, "{}:{}:{}", r.res_path, r.md5, r.byte_size);
    }
    sha256_hex(&buf)
}

fn resolve_sde_manifest_path(sde_path: &Path) -> PathBuf {
    if let Ok(env_path) = std::env::var("EVOMU_SDE_MANIFEST_PATH") {
        let trimmed = env_path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    let cwd_manifest = PathBuf::from("data/sde_import_manifest.json");
    if cwd_manifest.exists() {
        return cwd_manifest;
    }
    if let Some(sib) = sde_path.parent().map(|p| p.join("sde_import_manifest.json")) {
        if sib.exists() {
            return sib;
        }
    }
    if let Some(manifest_dir) = option_env!("CARGO_MANIFEST_DIR") {
        if let Some(root) = Path::new(manifest_dir).parent().and_then(|p| p.parent()) {
            let candidate = root.join("data").join("sde_import_manifest.json");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    cwd_manifest
}

fn resolve_quasar_paths(sde_path: &Path) -> (PathBuf, PathBuf) {
    let (sqlite_path, mut manifest_path) = QuasarDescriptorRegistry::default_paths();
    if !manifest_path.exists() {
        if let Some(sib) = sde_path
            .parent()
            .map(|p| p.join("quasar_descriptor_manifest.json"))
        {
            if sib.exists() {
                manifest_path = sib;
            }
        }
    }
    if !manifest_path.exists() {
        if let Some(manifest_dir) = option_env!("CARGO_MANIFEST_DIR") {
            if let Some(root) = Path::new(manifest_dir).parent().and_then(|p| p.parent()) {
                let candidate = root.join("data").join("quasar_descriptor_manifest.json");
                if candidate.exists() {
                    manifest_path = candidate;
                }
            }
        }
    }
    (sqlite_path, manifest_path)
}

fn read_evomu_sde_meta(sde_path: &Path) -> Result<HashMap<String, String>, String> {
    let conn = Connection::open_with_flags(sde_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("failed to open SDE SQLite '{}': {e}", sde_path.display()))?;
    let mut stmt = conn
        .prepare("SELECT key, value FROM _evomu_sde_meta")
        .map_err(|e| {
            format!(
                "missing or unreadable '_evomu_sde_meta' table in '{}' ({e}); rebuild with import_sde.py",
                sde_path.display()
            )
        })?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| {
            format!(
                "failed reading '_evomu_sde_meta' from '{}': {e}",
                sde_path.display()
            )
        })?;
    let mut meta = HashMap::new();
    for entry in rows {
        let (k, v) = entry.map_err(|e| {
            format!(
                "failed decoding '_evomu_sde_meta' row in '{}': {e}",
                sde_path.display()
            )
        })?;
        meta.insert(k, v);
    }
    Ok(meta)
}

fn check_sde_meta_against_manifest(
    sde_path: &Path,
    manifest_path: &Path,
    manifest: &SdeImportManifest,
    meta: &HashMap<String, String>,
) -> Result<(), String> {
    let expected_build = manifest.client_build.to_string();
    match meta.get("client_build") {
        Some(actual) if actual == &expected_build => {}
        other => {
            return Err(format!(
                "SDE client_build mismatch for '{}': got {:?}, expected '{}' from '{}'",
                sde_path.display(),
                other,
                expected_build,
                manifest_path.display()
            ));
        }
    }
    if let Some(actual_release) = meta.get("client_release") {
        if actual_release != &manifest.client_release {
            return Err(format!(
                "SDE client_release mismatch for '{}': got '{}', expected '{}' from '{}'",
                sde_path.display(),
                actual_release,
                manifest.client_release,
                manifest_path.display()
            ));
        }
    }

    let expected_count = manifest
        .resource_count
        .unwrap_or(manifest.resources.len())
        .to_string();
    match meta.get("resource_count") {
        Some(actual) if actual == &expected_count => {}
        other => {
            return Err(format!(
                "SDE resource_count mismatch for '{}': got {:?}, expected '{}' from '{}'",
                sde_path.display(),
                other,
                expected_count,
                manifest_path.display()
            ));
        }
    }

    let expected_sha256 = manifest
        .combined_resource_sha256
        .clone()
        .unwrap_or_else(|| compute_sde_resources_sha256(&manifest.resources));
    match meta.get("combined_resource_sha256") {
        Some(actual) if actual == &expected_sha256 => Ok(()),
        other => Err(format!(
            "SDE combined_resource_sha256 mismatch for '{}': got {:?}, expected '{}' from '{}'",
            sde_path.display(),
            other,
            expected_sha256,
            manifest_path.display()
        )),
    }
}

/// Verifies `sde_path` exists and its `_evomu_sde_meta` table matches `manifest_path` in O(1).
pub fn validate_sde_metadata(sde_path: &Path, manifest_path: &Path) -> Result<(), String> {
    if !sde_path.exists() {
        return Err(format_static_data_error(&format!(
            "missing SDE SQLite database at '{}'",
            sde_path.display()
        )));
    }
    if !manifest_path.exists() {
        return Err(format_static_data_error(&format!(
            "missing SDE manifest JSON at '{}'",
            manifest_path.display()
        )));
    }
    let manifest_raw = std::fs::read_to_string(manifest_path).map_err(|e| {
        format_static_data_error(&format!(
            "failed reading SDE manifest '{}': {e}",
            manifest_path.display()
        ))
    })?;
    let manifest: SdeImportManifest = serde_json::from_str(&manifest_raw).map_err(|e| {
        format_static_data_error(&format!(
            "invalid SDE manifest JSON at '{}': {e}",
            manifest_path.display()
        ))
    })?;

    let meta = read_evomu_sde_meta(sde_path).map_err(|e| format_static_data_error(&e))?;
    check_sde_meta_against_manifest(sde_path, manifest_path, &manifest, &meta)
        .map_err(|e| format_static_data_error(&e))?;
    Ok(())
}

/// Verifies `sde_path` exists and matches `manifest_path` in O(1) via `_evomu_sde_meta`,
/// then opens the validated [`SdeRegistry`].
pub fn validate_sde_against_manifest(
    sde_path: &Path,
    manifest_path: &Path,
) -> Result<SdeRegistry, String> {
    validate_sde_metadata(sde_path, manifest_path)?;

    SdeRegistry::open(sde_path).map_err(|e| {
        format_static_data_error(&format!(
            "failed loading validated SDE SQLite '{}': {e}",
            sde_path.display()
        ))
    })
}

/// Validates `quasar_sqlite_path` against `quasar_manifest_path` using strict manifest checks.
pub fn validate_quasar_descriptors(
    quasar_sqlite_path: &Path,
    quasar_manifest_path: &Path,
) -> Result<QuasarDescriptorRegistry, String> {
    QuasarDescriptorRegistry::open(quasar_sqlite_path, quasar_manifest_path).map_err(|e| {
        format_static_data_error(&format!(
            "Quasar descriptor check failed ('{}' vs '{}'): {e}",
            quasar_sqlite_path.display(),
            quasar_manifest_path.display()
        ))
    })
}

/// Validates and loads both static databases using explicit file paths.
///
/// When `allow_stale` is `true`, stale/missing DBs emit warnings and fall back to minimal
/// in-memory fixtures so synthetic test environments can boot without a client installation.
pub fn validate_and_load_startup_static_data_with_paths(
    sde_path: &Path,
    sde_manifest_path: &Path,
    quasar_sqlite_path: &Path,
    quasar_manifest_path: &Path,
    allow_stale: bool,
) -> Result<ValidatedStartupStaticData, String> {
    let sde = match validate_sde_against_manifest(sde_path, sde_manifest_path) {
        Ok(reg) => Arc::new(reg),
        Err(err) if allow_stale => {
            warn!(
                sde_path = %sde_path.display(),
                manifest_path = %sde_manifest_path.display(),
                error = %err,
                "bypassing SDE manifest check because stale static data override is enabled"
            );
            match SdeRegistry::open(sde_path) {
                Ok(reg) => Arc::new(reg),
                Err(_) => Arc::new(SdeRegistry::with_canonical_seed()),
            }
        }
        Err(err) => return Err(err),
    };

    let quasar_descriptors =
        match validate_quasar_descriptors(quasar_sqlite_path, quasar_manifest_path) {
            Ok(reg) => Arc::new(reg),
            Err(err) if allow_stale => {
                warn!(
                    quasar_sqlite_path = %quasar_sqlite_path.display(),
                    quasar_manifest_path = %quasar_manifest_path.display(),
                    error = %err,
                    "bypassing Quasar descriptor manifest check because stale static data override is enabled"
                );
                Arc::new(QuasarDescriptorRegistry::empty_with_builtins())
            }
            Err(err) => return Err(err),
        };

    Ok(ValidatedStartupStaticData {
        sde,
        quasar_descriptors,
    })
}

/// Resolves configured paths from `cfg`/env and validates both static databases before listener bind.
pub fn validate_and_load_startup_static_data(
    cfg: &ServerConfig,
    allow_stale: bool,
) -> Result<ValidatedStartupStaticData, String> {
    let sde_path = PathBuf::from(&cfg.sde_path);
    let sde_manifest_path = resolve_sde_manifest_path(&sde_path);
    let (quasar_sqlite_path, quasar_manifest_path) = resolve_quasar_paths(&sde_path);
    validate_and_load_startup_static_data_with_paths(
        &sde_path,
        &sde_manifest_path,
        &quasar_sqlite_path,
        &quasar_manifest_path,
        allow_stale,
    )
}
