//! Shrink-only allowlist ratchet guarding against silent unmapped `CustomInfo` wire fallbacks
//! in `ClientHarness` integration tests (`#1195`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static DEFAULT_UNMAPPED_CUSTOM_INFO_RATCHET: OnceLock<UnmappedCustomInfoRatchet> = OnceLock::new();

/// All `#1195` amended live-client `BridgeRpcError.code` values, constructor-name strings, and
/// mapped aliases (`crates/eve-proto/src/client_error.rs`, `client_error_facts.rs`) that must
/// never be allowlisted in [`UnmappedCustomInfoRatchet`].
pub const FORBIDDEN_AMENDED_1195_CODES: [&str; 33] = [
    "QuantumCoreRequired",
    "QuantumCoreRequiredFitting",
    "QuantumCoreRequiredRepair",
    "QuantumCoreRequiredService",
    "ShipAlreadyInWarp",
    "ShipInWarp",
    "CantActivateInWarp",
    "DeniedActivateInWarp",
    "NoChargesLoaded",
    "NoAmmoLoaded",
    "NotEnoughCapacitor",
    "ShipNotInSpace",
    "FittingValidationFailed",
    "CannotFitStackedModule",
    "InvalidTarget",
    "TargetingFailed",
    "InventoryNotAccessible",
    "MustBeDockedAtStartStation",
    "MustBeDocked",
    "StructureNotDockable",
    "StructureServiceAccessDenied",
    "WeaponsTimerActive",
    "CannotJumpWithWeaponsTimer",
    "InvalidContractState",
    "ContractOwnershipViolation",
    "ModuleNotFound",
    "OwnershipOrLocationViolation",
    "InsufficientRole",
    "GM_InsufficientRole",
    "UserError: GM_InsufficientRole",
    "GMRoleRequired",
    "GmRoleRequired",
    "InsufficientRoles",
];

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
        self.validate_no_forbidden_live_client_codes()?;
        Ok(())
    }

    /// Rejects any allowlist entry whose `:<code>` suffix is in [`FORBIDDEN_AMENDED_1195_CODES`]
    /// or is explicitly mapped by [`eve_proto::BridgeRpcError::to_client_error_mapped`] instead
    /// of hitting the unmapped `CustomInfo` fallback (`#1195`).
    pub fn validate_no_forbidden_live_client_codes(&self) -> Result<(), String> {
        for entry in &self.allowed_fallbacks {
            let (rpc, code) = entry.split_once(':').ok_or_else(|| {
                format!("invalid unmapped_custom_info_ratchet entry {entry:?}: expected service.method:code")
            })?;
            if FORBIDDEN_AMENDED_1195_CODES.contains(&code) {
                return Err(format!(
                    "ratchet must never allowlist amended #1195 live-client code {code:?} (entry {entry:?})"
                ));
            }
            let (service, method) = rpc.split_once('.').ok_or_else(|| {
                format!(
                    "invalid unmapped_custom_info_ratchet entry {entry:?}: expected service.method:code"
                )
            })?;
            let mapping = eve_proto::BridgeRpcError::user_error(code)
                .to_client_error_mapped(service, method);
            if !mapping.used_unmapped_custom_info_fallback {
                return Err(format!(
                    "ratchet entry {entry:?} is already mapped by BridgeRpcError::to_client_error_mapped ({:?}) and must not be allowlisted",
                    mapping.client_error
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
    fn checked_in_ratchet_never_allowlists_amended_1195_live_client_codes() {
        let ratchet = UnmappedCustomInfoRatchet::load_default()
            .expect("checked-in unmapped_custom_info_ratchet.json must parse and validate");
        ratchet
            .validate_no_forbidden_live_client_codes()
            .expect("checked-in ratchet must never allowlist #1195 live-client or mapped codes");
        for entry in &ratchet.allowed_fallbacks {
            let (rpc, code) = entry
                .split_once(':')
                .unwrap_or_else(|| panic!("expected service.method:code in {entry:?}"));
            let (service, method) = rpc
                .split_once('.')
                .unwrap_or_else(|| panic!("expected service.method:code in {entry:?}"));
            assert!(
                !FORBIDDEN_AMENDED_1195_CODES.contains(&code),
                "ratchet must never allowlist amended #1195 live-client code {code:?} (entry {entry:?})"
            );
            assert!(
                eve_proto::BridgeRpcError::user_error(code)
                    .to_client_error_mapped(service, method)
                    .used_unmapped_custom_info_fallback,
                "ratchet entry {entry:?} must hit used_unmapped_custom_info_fallback == true"
            );
        }
    }

    #[test]
    fn validate_no_forbidden_live_client_codes_rejects_live_client_and_mapped_entries() {
        for forbidden_entry in [
            "invbroker.Add:QuantumCoreRequired",
            "slash.SlashCmd:GM_InsufficientRole",
            "slash.SlashCmd:UserError: GM_InsufficientRole",
            "beyonce.CmdWarpToStuff:DeniedActivateInWarp",
            "dogmaIM.Activate:NoChargesLoaded",
            "contractProxy.AcceptContract:MustBeDocked",
            "beyonce.CmdWarpToStuff:ShipAlreadyInWarp",
            "marketProxy.PlaceBuyOrder:MaxOpenOrdersReached",
        ] {
            let ratchet = UnmappedCustomInfoRatchet {
                version: 1,
                description: "forbidden test".into(),
                allowed_fallbacks: BTreeSet::from([forbidden_entry.to_owned()]),
            };
            assert!(
                ratchet.validate_no_forbidden_live_client_codes().is_err(),
                "expected validate_no_forbidden_live_client_codes to reject {forbidden_entry:?}"
            );
            assert!(
                ratchet.validate_schema().is_err(),
                "expected validate_schema to reject {forbidden_entry:?}"
            );
        }
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
