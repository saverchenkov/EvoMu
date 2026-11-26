//! Shrink-only allowlist ratchet guarding against silent unmapped `CustomInfo` wire fallbacks
//! in `ClientHarness` integration tests (`#1195`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static DEFAULT_UNMAPPED_CUSTOM_INFO_RATCHET: OnceLock<UnmappedCustomInfoRatchet> = OnceLock::new();

/// Shrink-only allowlist of legacy `"{service}.{method}:{code}"` pairs permitted under
/// `ContractValidationMode::Ratcheted` when `BridgeRpcError::to_client_error_mapped` hits its
/// final `_ => CustomInfo` fallback (`#1195`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnmappedCustomInfoRatchet {
    pub version: u32,
    pub description: String,
    pub allowed_fallbacks: BTreeSet<String>,
}

impl UnmappedCustomInfoRatchet {
    /// Repository path to `crates/eve-testkit/ratchets/unmapped_custom_info_ratchet.json`.
    pub fn default_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ratchets")
            .join("unmapped_custom_info_ratchet.json")
    }

    /// Loads and validates `unmapped_custom_info_ratchet.json` from its default repository path.
    pub fn load_default() -> Result<Self, String> {
        Self::load_from_path(&Self::default_path())
    }

    /// Loads and validates an [`UnmappedCustomInfoRatchet`] from `path`.
    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let parsed: Self = serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
        parsed.validate_schema()?;
        Ok(parsed)
    }

    /// Process-cached [`UnmappedCustomInfoRatchet`] loaded once from [`Self::default_path`].
    pub fn load_default_cached() -> &'static Self {
        DEFAULT_UNMAPPED_CUSTOM_INFO_RATCHET.get_or_init(|| {
            Self::load_default().unwrap_or_else(|err| {
                panic!("failed to load default UnmappedCustomInfoRatchet: {err}")
            })
        })
    }

    /// Canonical ratchet key format: `"{service}.{method}:{code}"`.
    pub fn fallback_key(service: &str, method: &str, code: &str) -> String {
        format!("{service}.{method}:{code}")
    }

    /// Returns `true` iff `"{service}.{method}:{code}"` is present in [`Self::allowed_fallbacks`].
    pub fn is_allowed(&self, service: &str, method: &str, code: &str) -> bool {
        self.allowed_fallbacks
            .contains(&Self::fallback_key(service, method, code))
    }

    /// Validates ratchet schema (`version == 1` and every entry non-empty `service.method:code`).
    pub fn validate_schema(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err(format!(
                "unsupported unmapped_custom_info_ratchet version {} (expected 1)",
                self.version
            ));
        }
        for entry in &self.allowed_fallbacks {
            let Some((rpc, code)) = entry.split_once(':') else {
                return Err(format!(
                    "invalid unmapped_custom_info_ratchet entry {entry:?}: expected service.method:code"
                ));
            };
            let Some((service, method)) = rpc.split_once('.') else {
                return Err(format!(
                    "invalid unmapped_custom_info_ratchet entry {entry:?}: expected service.method:code"
                ));
            };
            if service.is_empty()
                || method.is_empty()
                || code.is_empty()
                || method.contains('.')
                || code.contains(':')
                || entry.chars().any(char::is_whitespace)
            {
                return Err(format!(
                    "malformed unmapped_custom_info_ratchet entry {entry:?}: empty or ambiguous segments"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_ratchet_loads_and_validates_schema() {
        let ratchet = UnmappedCustomInfoRatchet::load_default()
            .expect("checked-in unmapped_custom_info_ratchet.json must parse and validate");
        assert_eq!(ratchet.version, 1);
        assert!(ratchet.validate_schema().is_ok());
        assert_eq!(
            UnmappedCustomInfoRatchet::fallback_key(
                "charUnboundMgr",
                "SelectCharacterID",
                "InvalidArgument"
            ),
            "charUnboundMgr.SelectCharacterID:InvalidArgument"
        );
        assert!(ratchet.is_allowed("charUnboundMgr", "SelectCharacterID", "InvalidArgument"));
        assert!(ratchet.is_allowed("planetMgr", "UserLaunchCommodities", "NotInSpace"));
        assert!(!ratchet.is_allowed("dogmaIM", "LoadAmmo", "ItemNotFound"));
        assert!(!ratchet.is_allowed("beyonce", "CmdWarpToStuff", "ShipAlreadyInWarp"));
    }

    #[test]
    fn validate_schema_rejects_bad_versions_and_malformed_keys() {
        let bad_version = UnmappedCustomInfoRatchet {
            version: 2,
            description: "bad".into(),
            allowed_fallbacks: BTreeSet::new(),
        };
        assert!(bad_version.validate_schema().is_err());

        for bad_key in [
            "",
            "noColon.Method",
            "svcNoMethod:Code",
            ".Method:Code",
            "svc.:Code",
            "svc.Method:",
            "svc.Method.Extra:Code",
            "svc.Method:Code:Extra",
            "svc.Method:Bad Code",
        ] {
            let bad = UnmappedCustomInfoRatchet {
                version: 1,
                description: "bad".into(),
                allowed_fallbacks: BTreeSet::from([bad_key.to_owned()]),
            };
            assert!(
                bad.validate_schema().is_err(),
                "expected validate_schema to reject {bad_key:?}"
            );
        }
    }
}
