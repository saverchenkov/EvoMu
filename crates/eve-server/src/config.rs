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
    /// Optional character created for this account at startup (through
    /// `charUnboundMgr.CreateCharacterWithDoll`) when the account owns no character yet.
    #[serde(default)]
    pub starter_character: Option<StarterCharacterConfig>,
}

/// `CreateCharacterWithDoll(charactername, bloodlineID, genderID, ancestryID, ..., schoolID)` inputs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StarterCharacterConfig {
    pub name: String,
    pub bloodline_id: i64,
    pub gender: i64,
    pub ancestry_id: i64,
    pub school_id: i64,
    #[serde(default)]
    pub solar_system_id: Option<i64>,
    #[serde(default)]
    pub station_id: Option<i64>,
    #[serde(default)]
    pub initial_balance_isk: Option<i64>,
    #[serde(default)]
    pub initial_balance_plex: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureFlagsConfig {
    #[serde(default = "default_true")]
    pub enable_quasar_gateway: bool,
    #[serde(default = "default_true")]
    pub enable_esi_http: bool,
    #[serde(default = "default_true")]
    pub enable_living_world: bool,
    #[serde(default)]
    pub seed_demo: bool,
    #[serde(default = "default_true")]
    pub enable_project_discovery: bool,
    #[serde(default = "default_true")]
    pub enable_info_gathering: bool,
    #[serde(default = "default_info_gathering_interval_secs")]
    pub info_gathering_interval_secs: u64,
    #[serde(default)]
    pub free_fast_checkout_plex: bool,
    #[serde(default)]
    pub starter_plex_balance: i64,
    #[serde(default)]
    pub allow_local_dev_gateway_jwt: bool,
    #[serde(default)]
    pub gm_mode: bool,
    /// Global multiplier applied to skill training speed (SP/min) and inverse duration (`#1123`).
    #[serde(
        default = "default_rate_multiplier",
        deserialize_with = "deserialize_rate_f64"
    )]
    pub skill_training_multiplier: f64,
    /// Global multiplier applied to player mining yield per cycle (`#1123`).
    #[serde(
        default = "default_rate_multiplier",
        deserialize_with = "deserialize_rate_f64"
    )]
    pub mining_yield_multiplier: f64,
}

fn default_true() -> bool {
    true
}

fn default_info_gathering_interval_secs() -> u64 {
    300
}

fn default_rate_multiplier() -> f64 {
    1.0
}

fn deserialize_rate_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct RateF64Visitor;

    impl<'de> serde::de::Visitor<'de> for RateF64Visitor {
        type Value = f64;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a finite positive number")
        }

        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
            Ok(v as f64)
        }

        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
            Ok(v as f64)
        }

        fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
            Ok(v)
        }
    }

    deserializer.deserialize_any(RateF64Visitor)
}

#[derive(Debug, Clone, Copy)]
struct RateValue(f64);

impl<'de> serde::Deserialize<'de> for RateValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserialize_rate_f64(deserializer).map(Self)
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawConfigRateAliases {
    #[serde(default)]
    skill_training_multiplier: Option<RateValue>,
    #[serde(default)]
    mining_yield_multiplier: Option<RateValue>,
    #[serde(default)]
    rates: RawRatesSection,
    #[serde(default)]
    feature_flags: RawFeatureFlagsRates,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRatesSection {
    #[serde(default)]
    skill_training_multiplier: Option<RateValue>,
    #[serde(default)]
    mining_yield_multiplier: Option<RateValue>,
}

#[derive(Debug, Default, Deserialize)]
struct RawFeatureFlagsRates {
    #[serde(default)]
    skill_training_multiplier: Option<RateValue>,
    #[serde(default)]
    mining_yield_multiplier: Option<RateValue>,
}

fn validate_rate_multiplier(label: &str, value: f64) -> Result<f64, String> {
    if !value.is_finite() || value <= 0.0 {
        return Err(format!("{label} must be finite and > 0.0 (got {value})"));
    }
    Ok(value)
}

fn resolve_rate_multiplier(
    field_name: &str,
    top_level: Option<f64>,
    rates_section: Option<f64>,
    feature_flags: Option<f64>,
) -> Result<f64, String> {
    let candidates = [
        ("top-level", top_level),
        ("[rates]", rates_section),
        ("[feature_flags]", feature_flags),
    ];
    let mut resolved: Vec<(&str, f64)> = Vec::new();
    for (section, maybe_val) in candidates {
        if let Some(v) = maybe_val {
            let label = if section == "top-level" {
                format!("{field_name} in evomu.toml")
            } else {
                format!("{section}.{field_name} in evomu.toml")
            };
            validate_rate_multiplier(&label, v)?;
            resolved.push((section, v));
        }
    }
    let Some((_, first)) = resolved.first().copied() else {
        return Ok(default_rate_multiplier());
    };
    for (_, val) in resolved.iter().skip(1) {
        let tol = 1e-12 * first.abs().max(val.abs()).max(1.0);
        if (first - *val).abs() > tol {
            let summary = resolved
                .iter()
                .map(|(sec, v)| format!("{sec}={v}"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "Conflicting {field_name} values in evomu.toml ({summary}); specify it in only one of top-level, [rates], or [feature_flags]"
            ));
        }
    }
    Ok(first)
}

fn parse_rate_env(var_name: &str) -> Result<Option<f64>, String> {
    let Ok(raw) = std::env::var(var_name) else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let parsed = trimmed
        .parse::<f64>()
        .map_err(|e| format!("Invalid {var_name} '{trimmed}': {e}"))?;
    validate_rate_multiplier(var_name, parsed)
        .map_err(|e| format!("Invalid {var_name} '{trimmed}': {e}"))?;
    Ok(Some(parsed))
}

impl Default for FeatureFlagsConfig {
    fn default() -> Self {
        Self {
            enable_quasar_gateway: true,
            enable_esi_http: true,
            enable_living_world: true,
            seed_demo: false,
            enable_project_discovery: true,
            enable_info_gathering: true,
            info_gathering_interval_secs: default_info_gathering_interval_secs(),
            free_fast_checkout_plex: false,
            starter_plex_balance: 0,
            allow_local_dev_gateway_jwt: false,
            gm_mode: false,
            skill_training_multiplier: default_rate_multiplier(),
            mining_yield_multiplier: default_rate_multiplier(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageConfig {
    /// World persistence backend (`"sqlite"` or `"memory"`).
    #[serde(default = "default_storage_backend")]
    pub backend: String,
}

fn default_storage_backend() -> String {
    "sqlite".to_string()
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: default_storage_backend(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind_address")]
    pub bind_address: String,
    #[serde(default = "default_macho_port")]
    pub macho_port: u16,
    #[serde(default = "default_bridge_port")]
    pub bridge_port: u16,
    #[serde(default = "default_esi_port")]
    pub esi_port: u16,
    #[serde(default = "default_quasar_grpc_port")]
    pub quasar_grpc_port: u16,
    #[serde(default = "default_xmpp_port")]
    pub xmpp_port: u16,
    #[serde(default = "default_sde_path")]
    pub sde_path: String,
    #[serde(default = "default_world_db_path")]
    pub world_db_path: String,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub admin_accounts: Vec<AdminAccountConfig>,
    #[serde(default)]
    pub player_accounts: Vec<PlayerAccountConfig>,
    #[serde(default = "default_belt_respawn_interval_secs")]
    pub belt_respawn_interval_secs: u64,
    #[serde(default = "default_downtime_utc_secs_of_day")]
    pub downtime_utc_secs_of_day: u64,
    /// Enables automatic daily scheduled downtime countdowns and shutdown (`true` by default; `#1586`).
    #[serde(default = "default_true")]
    pub scheduled_downtime_enabled: bool,
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

fn default_quasar_grpc_port() -> u16 {
    26_005
}

fn default_xmpp_port() -> u16 {
    5222
}

fn default_sde_path() -> String {
    "data/evomu_sde.sqlite".to_string()
}

fn default_world_db_path() -> String {
    "data/evomu_world.sqlite".to_string()
}

fn default_belt_respawn_interval_secs() -> u64 {
    86_400
}

fn default_downtime_utc_secs_of_day() -> u64 {
    39_600
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_address: default_bind_address(),
            macho_port: default_macho_port(),
            bridge_port: default_bridge_port(),
            esi_port: default_esi_port(),
            quasar_grpc_port: default_quasar_grpc_port(),
            xmpp_port: default_xmpp_port(),
            sde_path: default_sde_path(),
            world_db_path: default_world_db_path(),
            storage: StorageConfig::default(),
            admin_accounts: Vec::new(),
            player_accounts: Vec::new(),
            belt_respawn_interval_secs: default_belt_respawn_interval_secs(),
            downtime_utc_secs_of_day: default_downtime_utc_secs_of_day(),
            scheduled_downtime_enabled: true,
            feature_flags: FeatureFlagsConfig::default(),
        }
    }
}

impl ServerConfig {
    /// Validates `[storage] backend` (`"sqlite"` or `"memory"`) and `world_db_path` extension (`#877`).
    pub fn validate_storage_backend(&self) -> Result<&str, String> {
        let backend = self.storage.backend.trim();
        match backend {
            "sqlite" => {
                let db_path = self.world_db_path.trim();
                if db_path.is_empty() {
                    return Err("world_db_path in evomu.toml must not be empty".into());
                }
                let ext = Path::new(db_path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase());
                if !matches!(ext.as_deref(), Some("sqlite" | "sqlite3" | "db")) {
                    return Err(format!(
                        "world_db_path '{db_path}' must use a SQLite file extension (.sqlite, .sqlite3, or .db) when storage.backend = \"sqlite\""
                    ));
                }
                Ok("sqlite")
            }
            "memory" => Ok("memory"),
            other => Err(format!(
                "Unsupported storage.backend '{other}' in evomu.toml (expected \"sqlite\" or \"memory\")"
            )),
        }
    }

    /// Validates configured rate multipliers (`#1123`).
    pub fn validate_rates(&self) -> Result<(), String> {
        validate_rate_multiplier(
            "feature_flags.skill_training_multiplier",
            self.feature_flags.skill_training_multiplier,
        )?;
        validate_rate_multiplier(
            "feature_flags.mining_yield_multiplier",
            self.feature_flags.mining_yield_multiplier,
        )?;
        Ok(())
    }

    /// Returns the configured global skill training speed multiplier (`1.0` = vanilla).
    #[inline]
    pub fn skill_training_multiplier(&self) -> f64 {
        self.feature_flags.skill_training_multiplier
    }

    /// Returns the configured global mining yield multiplier (`1.0` = vanilla).
    #[inline]
    pub fn mining_yield_multiplier(&self) -> f64 {
        self.feature_flags.mining_yield_multiplier
    }

    /// Parses a `ServerConfig` from a TOML string (`D-5`).
    pub fn from_toml_str(toml_str: &str) -> Result<Self, String> {
        let mut cfg: Self =
            toml::from_str(toml_str).map_err(|e| format!("Invalid evomu.toml: {e}"))?;
        let raw_rates: RawConfigRateAliases =
            toml::from_str(toml_str).map_err(|e| format!("Invalid evomu.toml: {e}"))?;
        cfg.feature_flags.skill_training_multiplier = resolve_rate_multiplier(
            "skill_training_multiplier",
            raw_rates.skill_training_multiplier.map(|r| r.0),
            raw_rates.rates.skill_training_multiplier.map(|r| r.0),
            raw_rates.feature_flags.skill_training_multiplier.map(|r| r.0),
        )?;
        cfg.feature_flags.mining_yield_multiplier = resolve_rate_multiplier(
            "mining_yield_multiplier",
            raw_rates.mining_yield_multiplier.map(|r| r.0),
            raw_rates.rates.mining_yield_multiplier.map(|r| r.0),
            raw_rates.feature_flags.mining_yield_multiplier.map(|r| r.0),
        )?;
        if cfg.bind_address.trim().is_empty() {
            return Err("bind_address in evomu.toml must not be empty".into());
        }
        if cfg.sde_path.trim().is_empty() {
            return Err("sde_path in evomu.toml must not be empty".into());
        }
        if cfg.world_db_path.trim().is_empty() {
            return Err("world_db_path in evomu.toml must not be empty".into());
        }
        cfg.validate_storage_backend()?;
        if !(1..=604_800).contains(&cfg.belt_respawn_interval_secs) {
            return Err(
                "belt_respawn_interval_secs in evomu.toml must be in 1..=604800".into(),
            );
        }
        if cfg.downtime_utc_secs_of_day >= 86_400 {
            return Err("downtime_utc_secs_of_day in evomu.toml must be < 86400".into());
        }
        if cfg.feature_flags.info_gathering_interval_secs == 0 {
            return Err("feature_flags.info_gathering_interval_secs in evomu.toml must be >= 1".into());
        }
        if cfg.feature_flags.starter_plex_balance < 0 {
            return Err("feature_flags.starter_plex_balance in evomu.toml must be >= 0".into());
        }
        for acc in &cfg.player_accounts {
            if let Some(plex) = acc
                .starter_character
                .as_ref()
                .and_then(|s| s.initial_balance_plex)
            {
                if plex < 0 {
                    return Err(format!(
                        "player_accounts[{}].starter_character.initial_balance_plex in evomu.toml must be >= 0",
                        acc.username
                    ));
                }
            }
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
    /// overrides (`EVOMU_SDE_PATH`, `EVOMU_DB_PATH`, `EVOMU_SNAPSHOT_PATH`, `EVOMU_STORAGE_BACKEND`, `EVOMU_ACCOUNTS`, `EVOMU_GM_ACCOUNT`, `EVOMU_SKILL_TRAINING_MULTIPLIER`, `EVOMU_MINING_YIELD_MULTIPLIER`).
    pub fn load_default_or_env() -> Result<Self, String> {
        #[cfg(test)]
        let _env_read_guard = crate::lock_test_env_read();
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
        if let Ok(backend_override) = std::env::var("EVOMU_STORAGE_BACKEND") {
            if !backend_override.trim().is_empty() {
                cfg.storage.backend = backend_override.trim().to_string();
            }
        }
        cfg.validate_storage_backend()?;
        if let Ok(accounts_spec) = std::env::var("EVOMU_ACCOUNTS") {
            for entry in accounts_spec.split(',') {
                if let Some((user, pass)) = entry.trim().split_once(':') {
                    let u = user.trim();
                    let p = pass.trim();
                    if !u.is_empty() && !p.is_empty() {
                        cfg.player_accounts.push(PlayerAccountConfig {
                            username: u.to_string(),
                            password_token: p.to_string(),
                            starter_character: None,
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
        if let Ok(grpc_port_str) = std::env::var("EVOMU_QUASAR_GRPC_PORT") {
            let trimmed = grpc_port_str.trim();
            if !trimmed.is_empty() {
                cfg.quasar_grpc_port = trimmed.parse::<u16>().map_err(|e| {
                    format!("Invalid EVOMU_QUASAR_GRPC_PORT '{trimmed}': {e}")
                })?;
            }
        }
        let xmpp_port_env = std::env::var("EVOMU_XMPP_PORT")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                std::env::var("EVEMU_XMPP_PORT")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
            });
        if let Some(xmpp_port_str) = xmpp_port_env {
            let trimmed = xmpp_port_str.trim();
            cfg.xmpp_port = trimmed.parse::<u16>().map_err(|e| {
                format!("Invalid EVOMU_XMPP_PORT '{trimmed}': {e}")
            })?;
        }
        if let Ok(dev_jwt_str) = std::env::var("EVOMU_ALLOW_LOCAL_DEV_JWT") {
            if let Some(enabled) = parse_bool_flag(&dev_jwt_str) {
                cfg.feature_flags.allow_local_dev_gateway_jwt = enabled;
            }
        }
        if let Ok(dt_enabled_str) = std::env::var("EVOMU_SCHEDULED_DOWNTIME_ENABLED") {
            if let Some(enabled) = parse_bool_flag(&dt_enabled_str) {
                cfg.scheduled_downtime_enabled = enabled;
            }
        }
        if let Some(mult) = parse_rate_env("EVOMU_SKILL_TRAINING_MULTIPLIER")? {
            cfg.feature_flags.skill_training_multiplier = mult;
        }
        if let Some(mult) = parse_rate_env("EVOMU_MINING_YIELD_MULTIPLIER")? {
            cfg.feature_flags.mining_yield_multiplier = mult;
        }
        Ok(cfg)
    }

    /// Parses optional `--xmpp-port <N>` or `--xmpp-port=<N>` CLI flag (`0` = OS ephemeral port).
    pub fn parse_cli_xmpp_port<I, S>(args: I) -> Result<Option<u16>, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut iter = args.into_iter();
        let mut out = None;
        while let Some(raw) = iter.next() {
            let arg = raw.as_ref();
            if arg == "--" {
                break;
            }
            if arg == "--xmpp-port" {
                let Some(val) = iter.next() else {
                    return Err("Missing port value after --xmpp-port".to_string());
                };
                let trimmed = val.as_ref().trim();
                let port = trimmed.parse::<u16>().map_err(|e| {
                    format!("Invalid --xmpp-port '{trimmed}': {e}")
                })?;
                out = Some(port);
            } else if let Some(rest) = arg.strip_prefix("--xmpp-port=") {
                let trimmed = rest.trim();
                let port = trimmed.parse::<u16>().map_err(|e| {
                    format!("Invalid --xmpp-port '{trimmed}': {e}")
                })?;
                out = Some(port);
            }
        }
        Ok(out)
    }

    /// Parses optional `--gm` CLI flag enabling GM-only loopback ops endpoints (`#1077 §A`).
    pub fn parse_cli_gm_flag<I, S>(args: I) -> bool
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        has_cli_flag(args, "--gm")
    }

    /// Parses optional `--allow-stale-static-data` CLI flag bypassing startup SDE/Quasar validation
    /// for synthetic test fixtures (`#1119`).
    pub fn parse_cli_allow_stale_static_data<I, S>(args: I) -> bool
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        has_cli_flag(args, "--allow-stale-static-data")
    }

    /// Returns `true` when `EVOMU_ALLOW_STALE_STATIC_DATA` is set to a truthy boolean (`1/true/yes/on`).
    pub fn env_allow_stale_static_data() -> bool {
        std::env::var("EVOMU_ALLOW_STALE_STATIC_DATA")
            .ok()
            .and_then(|v| parse_bool_flag(&v))
            .unwrap_or(false)
    }
}

fn parse_bool_flag(raw: &str) -> Option<bool> {
    let v = raw.trim().to_ascii_lowercase();
    if matches!(v.as_str(), "1" | "true" | "yes" | "on") {
        Some(true)
    } else if matches!(v.as_str(), "0" | "false" | "no" | "off") {
        Some(false)
    } else {
        None
    }
}

fn has_cli_flag<I, S>(args: I, flag: &str) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    for raw in args {
        let arg = raw.as_ref();
        if arg == "--" {
            break;
        }
        if arg == flag {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // parity: plan:D-5, feat:serverops.config-file
    fn test_d5_server_config_parse_and_feature_flags() {
        let toml_text = r#"
bind_address = "127.0.0.1"
macho_port = 26010
bridge_port = 26012
esi_port = 26014
sde_path = "data/evomu_sde_fixture.sqlite"
world_db_path = "data/test_world.sqlite"
belt_respawn_interval_secs = 43200
downtime_utc_secs_of_day = 36000

[feature_flags]
enable_quasar_gateway = false
enable_esi_http = true
enable_living_world = false
seed_demo = true
enable_project_discovery = false
enable_info_gathering = false
info_gathering_interval_secs = 600
"#;
        let cfg = ServerConfig::from_toml_str(toml_text).expect("parse ServerConfig");
        assert_eq!(cfg.bind_address, "127.0.0.1");
        assert_eq!(cfg.macho_port, 26010);
        assert_eq!(cfg.bridge_port, 26012);
        assert_eq!(cfg.esi_port, 26014);
        assert_eq!(cfg.sde_path, "data/evomu_sde_fixture.sqlite");
        assert_eq!(cfg.world_db_path, "data/test_world.sqlite");
        assert_eq!(cfg.belt_respawn_interval_secs, 43_200);
        assert_eq!(cfg.downtime_utc_secs_of_day, 36_000);
        assert!(cfg.scheduled_downtime_enabled);
        assert!(!cfg.feature_flags.enable_quasar_gateway);
        assert!(cfg.feature_flags.enable_esi_http);
        assert!(!cfg.feature_flags.enable_living_world);
        assert!(cfg.feature_flags.seed_demo);
        assert!(!cfg.feature_flags.enable_project_discovery);
        assert!(!cfg.feature_flags.enable_info_gathering);
        assert_eq!(cfg.feature_flags.info_gathering_interval_secs, 600);

        assert!(ServerConfig::from_toml_str(r#"bind_address = """#).is_err());
        assert!(ServerConfig::from_toml_str(r#"sde_path = """#).is_err());
        assert!(ServerConfig::from_toml_str(r#"world_db_path = """#).is_err());
        assert!(ServerConfig::from_toml_str(r#"belt_respawn_interval_secs = 0"#).is_err());
        assert!(ServerConfig::from_toml_str(r#"belt_respawn_interval_secs = 9999999"#).is_err());
        assert!(ServerConfig::from_toml_str(r#"downtime_utc_secs_of_day = 86400"#).is_err());
        assert!(ServerConfig::from_toml_str("[feature_flags]\ninfo_gathering_interval_secs = 0").is_err());
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;
#[cfg(test)] #[path = "config_extra_tests.rs"] mod config_extra_tests;
#[cfg(test)]
#[path = "config_edges_tests.rs"]
mod config_edges_tests;

