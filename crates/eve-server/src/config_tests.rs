//! Unit tests for `config` TOML parsing, boundary validation, file loading, and env overrides.

#![forbid(unsafe_code)]

use super::*;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvVarGuard {
    fn capture(keys: &[&'static str]) -> Self {
        let saved = keys
            .iter()
            .map(|&k| (k, std::env::var(k).ok()))
            .collect();
        Self { saved }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        for (k, v) in self.saved.drain(..) {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

#[test]
fn server_config_defaults_accounts_and_boundary_validation() {
    let def = ServerConfig::default();
    assert_eq!(def.bind_address, "127.0.0.1");
    assert_eq!(def.macho_port, 26_000);
    assert_eq!(def.bridge_port, 26_002);
    assert_eq!(def.esi_port, 26_004);
    assert_eq!(def.quasar_grpc_port, 26_005);
    assert_eq!(def.sde_path, "data/evomu_sde.sqlite");
    assert_eq!(def.world_db_path, "data/evomu_world.sqlite");
    assert_eq!(def.belt_respawn_interval_secs, 86_400);
    assert_eq!(def.downtime_utc_secs_of_day, 39_600);
    assert!(def.feature_flags.enable_quasar_gateway);
    assert!(def.feature_flags.enable_esi_http);
    assert!(def.feature_flags.enable_living_world);
    assert!(!def.feature_flags.seed_demo);
    assert!(def.feature_flags.enable_project_discovery);
    assert!(def.feature_flags.enable_info_gathering);
    assert_eq!(def.feature_flags.info_gathering_interval_secs, 300);

    // Exact inclusive bounds: belt_respawn in 1..=604_800, downtime < 86_400, info_gathering >= 1.
    let min_ok = ServerConfig::from_toml_str(
        "belt_respawn_interval_secs = 1\ndowntime_utc_secs_of_day = 0\n[feature_flags]\ninfo_gathering_interval_secs = 1\n",
    )
    .expect("minimum bounds valid");
    assert_eq!(min_ok.belt_respawn_interval_secs, 1);
    assert_eq!(min_ok.downtime_utc_secs_of_day, 0);

    let max_ok = ServerConfig::from_toml_str(
        "belt_respawn_interval_secs = 604800\ndowntime_utc_secs_of_day = 86399\n",
    )
    .expect("maximum bounds valid");
    assert_eq!(max_ok.belt_respawn_interval_secs, 604_800);
    assert_eq!(max_ok.downtime_utc_secs_of_day, 86_399);

    // Out-of-bounds and whitespace-only strings must fail.
    assert!(ServerConfig::from_toml_str("belt_respawn_interval_secs = 604801").is_err());
    assert!(ServerConfig::from_toml_str("bind_address = \"   \"").is_err());
    assert!(ServerConfig::from_toml_str("sde_path = \"  \"").is_err());
    assert!(ServerConfig::from_toml_str("world_db_path = \"\\t\"").is_err());
    assert!(ServerConfig::from_toml_str("not = [valid toml").is_err());

    // Admin & player accounts with starter character config.
    let with_accounts = ServerConfig::from_toml_str(
        r#"
[[admin_accounts]]
username = "admin1"
password_token = "tok1"

[[player_accounts]]
username = "pilot1"
password_token = "tok2"

[player_accounts.starter_character]
name = "Aurelia"
bloodline_id = 1
gender = 0
ancestry_id = 11
school_id = 11
solar_system_id = 30000142
station_id = 60003760
initial_balance_isk = 5000000
"#,
    )
    .expect("accounts parse");
    assert_eq!(with_accounts.admin_accounts.len(), 1);
    assert_eq!(with_accounts.admin_accounts[0].character_id, 90_000_003);
    assert_eq!(with_accounts.admin_accounts[0].character_name, "ISD Polaris");
    let starter = with_accounts.player_accounts[0]
        .starter_character
        .as_ref()
        .expect("starter character");
    assert_eq!(starter.name, "Aurelia");
    assert_eq!(starter.solar_system_id, Some(30_000_142));
    assert_eq!(starter.station_id, Some(60_003_760));
    assert_eq!(starter.initial_balance_isk, Some(5_000_000));
}

#[test]
fn server_config_load_from_toml_and_env_overrides() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _guard = EnvVarGuard::capture(&[
        "EVOMU_CONFIG_PATH",
        "EVOMU_DB_PATH",
        "EVOMU_SNAPSHOT_PATH",
        "EVOMU_ACCOUNTS",
        "EVOMU_GM_ACCOUNT",
        "EVOMU_QUASAR_GRPC_PORT",
    ]);

    let tmp_dir = std::env::temp_dir().join(format!("evomu_cfg_test_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let cfg_file = tmp_dir.join("evomu_test.toml");
    std::fs::write(
        &cfg_file,
        "bind_address = \"0.0.0.0\"\nmacho_port = 26100\n",
    )
    .unwrap();

    // Missing file error.
    let missing_err = ServerConfig::load_from_toml(tmp_dir.join("missing.toml")).unwrap_err();
    assert!(missing_err.contains("Failed to read config file"));

    // Load via EVOMU_CONFIG_PATH + EVOMU_DB_PATH + accounts + GM + gRPC port.
    std::env::set_var("EVOMU_CONFIG_PATH", cfg_file.to_str().unwrap());
    std::env::set_var("EVOMU_DB_PATH", "/custom/world.sqlite");
    std::env::set_var("EVOMU_SNAPSHOT_PATH", "/ignored/snap.sqlite");
    std::env::set_var("EVOMU_ACCOUNTS", "alice:pw_a, alice:, :pw_only, , invalid_entry, bob:pw_b");
    std::env::set_var("EVOMU_GM_ACCOUNT", "gm_user:gm_pw");
    std::env::set_var("EVOMU_QUASAR_GRPC_PORT", "27005");

    let cfg = ServerConfig::load_default_or_env().expect("load_default_or_env");
    assert_eq!(cfg.bind_address, "0.0.0.0");
    assert_eq!(cfg.macho_port, 26_100);
    assert_eq!(cfg.world_db_path, "/custom/world.sqlite");
    assert_eq!(cfg.quasar_grpc_port, 27_005);
    assert_eq!(cfg.player_accounts.len(), 2);
    assert_eq!(cfg.player_accounts[0].username, "alice");
    assert_eq!(cfg.player_accounts[1].username, "bob");
    assert_eq!(cfg.admin_accounts.len(), 1);
    assert_eq!(cfg.admin_accounts[0].username, "gm_user");
    assert_eq!(cfg.admin_accounts[0].character_id, 90_000_003);

    // EVOMU_SNAPSHOT_PATH applies when EVOMU_DB_PATH is unset, and blank EVOMU_CONFIG_PATH uses defaults.
    std::env::set_var("EVOMU_CONFIG_PATH", "   ");
    std::env::remove_var("EVOMU_DB_PATH");
    std::env::set_var("EVOMU_SNAPSHOT_PATH", "/custom/snapshot.sqlite");
    std::env::remove_var("EVOMU_ACCOUNTS");
    std::env::remove_var("EVOMU_GM_ACCOUNT");
    std::env::set_var("EVOMU_QUASAR_GRPC_PORT", "");

    let cfg2 = ServerConfig::load_default_or_env().expect("load_default_or_env fallback");
    assert_eq!(cfg2.bind_address, "127.0.0.1");
    assert_eq!(cfg2.world_db_path, "/custom/snapshot.sqlite");
    assert_eq!(cfg2.quasar_grpc_port, 26_005);

    // Invalid EVOMU_QUASAR_GRPC_PORT fails with descriptive error.
    std::env::set_var("EVOMU_QUASAR_GRPC_PORT", "not_a_u16");
    let port_err = ServerConfig::load_default_or_env().unwrap_err();
    assert!(port_err.contains("Invalid EVOMU_QUASAR_GRPC_PORT"));

    let _ = std::fs::remove_dir_all(&tmp_dir);
}
