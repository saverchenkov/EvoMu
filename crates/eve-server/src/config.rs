//! `config` — `evomu.toml` server configuration loader (`D-5`).
//!
//! Configures bind address, network ports (`machoNet`, client bridge, ESI/SSO), database paths
//! (`sde_path`, `world_db_path`), operator-provisioned admin and player accounts, and runtime
//! feature flags.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminAccountConfig {
    pub username: String,
    pub password_token: String,
    #[serde(default = "default_admin_char_id")]
    pub character_id: i64,
    #[serde(default = "default_admin_char_name")]
    pub character_name: String,
}

fn default_admin_char_id() -> i64 {
    90_000_003
}

fn default_admin_char_name() -> String {
    "ISD Polaris".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerAccountConfig {
    pub username: String,
    pub password_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureFlagsConfig {
    #[serde(default = "default_true")]
    pub enable_quasar_gateway: bool,
    #[serde(default = "default_true")]
    pub enable_esi_http: bool,
    #[serde(default = "default_true")]
    pub enable_living_world: bool,
    #[serde(default)]
    pub seed_demo: bool,
}

fn default_true() -> bool {
    true
}

impl Default for FeatureFlagsConfig {
    fn default() -> Self {
        Self {
            enable_quasar_gateway: true,
            enable_esi_http: true,
            enable_living_world: true,
            seed_demo: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind_address")]
    pub bind_address: String,
    #[serde(default = "default_macho_port")]
    pub macho_port: u16,
    #[serde(default = "default_bridge_port")]
    pub bridge_port: u16,
    #[serde(default = "default_esi_port")]
    pub esi_port: u16,
    #[serde(default = "default_sde_path")]
    pub sde_path: String,
    #[serde(default = "default_world_db_path")]
    pub world_db_path: String,
    #[serde(default)]
    pub admin_accounts: Vec<AdminAccountConfig>,
    #[serde(default)]
    pub player_accounts: Vec<PlayerAccountConfig>,
    #[serde(default)]
    pub feature_flags: FeatureFlagsConfig,
}

fn default_bind_address() -> String {
    "127.0.0.1".to_string()
}

fn default_macho_port() -> u16 {
    26_000
}

fn default_bridge_port() -> u16 {
    26_002
}

fn default_esi_port() -> u16 {
    26_004
}

fn default_sde_path() -> String {
    "data/evomu_sde.sqlite".to_string()
}

fn default_world_db_path() -> String {
    "data/evomu_world.sqlite".to_string()
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_address: default_bind_address(),
            macho_port: default_macho_port(),
            bridge_port: default_bridge_port(),
            esi_port: default_esi_port(),
            sde_path: default_sde_path(),
            world_db_path: default_world_db_path(),
            admin_accounts: Vec::new(),
            player_accounts: Vec::new(),
            feature_flags: FeatureFlagsConfig::default(),
        }
    }
}

impl ServerConfig {
    /// Parses a `ServerConfig` from a TOML string (`D-5`).
    pub fn from_toml_str(toml_str: &str) -> Result<Self, String> {
        let cfg: Self = toml::from_str(toml_str).map_err(|e| format!("Invalid evomu.toml: {e}"))?;
        if cfg.bind_address.trim().is_empty() {
            return Err("bind_address in evomu.toml must not be empty".into());
        }
        if cfg.sde_path.trim().is_empty() {
            return Err("sde_path in evomu.toml must not be empty".into());
        }
        if cfg.world_db_path.trim().is_empty() {
            return Err("world_db_path in evomu.toml must not be empty".into());
        }
        Ok(cfg)
    }

    /// Loads a `ServerConfig` from a TOML file on disk (`D-5`).
    pub fn load_from_toml(path: impl AsRef<Path>) -> Result<Self, String> {
        let target = path.as_ref();
        let text = std::fs::read_to_string(target)
            .map_err(|e| format!("Failed to read config file '{}': {e}", target.display()))?;
        Self::from_toml_str(&text)
    }

    /// Loads `EVOMU_CONFIG_PATH` (or `evomu.toml` if present) and applies any environment variable
    /// overrides (`EVOMU_SDE_PATH`, `EVOMU_DB_PATH`, `EVOMU_SNAPSHOT_PATH`, `EVOMU_ACCOUNTS`, `EVOMU_GM_ACCOUNT`).
    pub fn load_default_or_env() -> Result<Self, String> {
        let mut cfg = if let Ok(custom_path) = std::env::var("EVOMU_CONFIG_PATH") {
            let trimmed = custom_path.trim();
            if !trimmed.is_empty() {
                Self::load_from_toml(trimmed)?
            } else {
                Self::default()
            }
        } else if Path::new("evomu.toml").exists() {
            Self::load_from_toml("evomu.toml")?
        } else {
            Self::default()
        };

        if let Ok(sde_override) = std::env::var("EVOMU_SDE_PATH") {
            if !sde_override.trim().is_empty() {
                cfg.sde_path = sde_override.trim().to_string();
            }
        }
        if let Ok(db_override) = std::env::var("EVOMU_DB_PATH") {
            if !db_override.trim().is_empty() {
                cfg.world_db_path = db_override.trim().to_string();
            }
        } else if let Ok(snap_override) = std::env::var("EVOMU_SNAPSHOT_PATH") {
            if !snap_override.trim().is_empty() {
                cfg.world_db_path = snap_override.trim().to_string();
            }
        }
        if let Ok(accounts_spec) = std::env::var("EVOMU_ACCOUNTS") {
            for entry in accounts_spec.split(',') {
                if let Some((user, pass)) = entry.trim().split_once(':') {
                    let u = user.trim();
                    let p = pass.trim();
                    if !u.is_empty() && !p.is_empty() {
                        cfg.player_accounts.push(PlayerAccountConfig {
                            username: u.to_string(),
                            password_token: p.to_string(),
                        });
                    }
                }
            }
        }
        if let Ok(gm_spec) = std::env::var("EVOMU_GM_ACCOUNT") {
            if let Some((user, pass)) = gm_spec.trim().split_once(':') {
                let u = user.trim();
                let p = pass.trim();
                if !u.is_empty() && !p.is_empty() {
                    cfg.admin_accounts.push(AdminAccountConfig {
                        username: u.to_string(),
                        password_token: p.to_string(),
                        character_id: default_admin_char_id(),
                        character_name: default_admin_char_name(),
                    });
                }
            }
        }
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_d5_server_config_parse_and_feature_flags() {
        let toml_text = r#"
bind_address = "127.0.0.1"
macho_port = 26010
bridge_port = 26012
esi_port = 26014
sde_path = "data/evomu_sde_fixture.sqlite"
world_db_path = "data/test_world.sqlite"

[feature_flags]
enable_quasar_gateway = false
enable_esi_http = true
enable_living_world = false
seed_demo = true
"#;
        let cfg = ServerConfig::from_toml_str(toml_text).expect("parse ServerConfig");
        assert_eq!(cfg.bind_address, "127.0.0.1");
        assert_eq!(cfg.macho_port, 26010);
        assert_eq!(cfg.bridge_port, 26012);
        assert_eq!(cfg.esi_port, 26014);
        assert_eq!(cfg.sde_path, "data/evomu_sde_fixture.sqlite");
        assert_eq!(cfg.world_db_path, "data/test_world.sqlite");
        assert!(!cfg.feature_flags.enable_quasar_gateway);
        assert!(cfg.feature_flags.enable_esi_http);
        assert!(!cfg.feature_flags.enable_living_world);
        assert!(cfg.feature_flags.seed_demo);

        assert!(ServerConfig::from_toml_str(r#"bind_address = """#).is_err());
        assert!(ServerConfig::from_toml_str(r#"sde_path = """#).is_err());
        assert!(ServerConfig::from_toml_str(r#"world_db_path = """#).is_err());
    }
}
