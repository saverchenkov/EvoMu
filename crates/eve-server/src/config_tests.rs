//! Unit tests for `config` TOML parsing, boundary validation, file loading, and env overrides.

#![forbid(unsafe_code)]

use super::*;

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
    assert_eq!(def.xmpp_port, 5222);
    assert_eq!(def.sde_path, "data/evomu_sde.sqlite");
    assert_eq!(def.world_db_path, "data/evomu_world.sqlite");
    assert_eq!(def.belt_respawn_interval_secs, 86_400);
    assert_eq!(def.downtime_utc_secs_of_day, 39_600);
    assert!(def.scheduled_downtime_enabled);
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
    let _lock = crate::lock_test_env_write();
    let _guard = EnvVarGuard::capture(&[
        "EVOMU_CONFIG_PATH",
        "EVOMU_DB_PATH",
        "EVOMU_SNAPSHOT_PATH",
        "EVOMU_ACCOUNTS",
        "EVOMU_GM_ACCOUNT",
        "EVOMU_QUASAR_GRPC_PORT",
        "EVOMU_XMPP_PORT",
        "EVEMU_XMPP_PORT",
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
    std::env::remove_var("EVOMU_QUASAR_GRPC_PORT");

    // TOML xmpp_port = 0 (OS ephemeral) and xmpp_port = 25232 deserialize cleanly.
    assert_eq!(
        ServerConfig::from_toml_str("xmpp_port = 0\n").expect("ephemeral xmpp_port").xmpp_port,
        0
    );
    assert_eq!(
        ServerConfig::from_toml_str("xmpp_port = 25232\n").expect("custom xmpp_port").xmpp_port,
        25_232
    );

    // EVOMU_XMPP_PORT ("0" and "25232") and EVEMU_XMPP_PORT fallback override TOML/default.
    std::env::set_var("EVOMU_XMPP_PORT", "0");
    assert_eq!(ServerConfig::load_default_or_env().expect("xmpp 0").xmpp_port, 0);
    std::env::set_var("EVOMU_XMPP_PORT", "25232");
    assert_eq!(ServerConfig::load_default_or_env().expect("xmpp 25232").xmpp_port, 25_232);
    std::env::set_var("EVOMU_XMPP_PORT", "   ");
    std::env::set_var("EVEMU_XMPP_PORT", "25233");
    assert_eq!(ServerConfig::load_default_or_env().expect("evemu xmpp fallback").xmpp_port, 25_233);
    std::env::set_var("EVOMU_XMPP_PORT", "not_a_u16");
    assert!(
        ServerConfig::load_default_or_env()
            .unwrap_err()
            .contains("Invalid EVOMU_XMPP_PORT")
    );
    std::env::remove_var("EVOMU_XMPP_PORT");
    std::env::remove_var("EVEMU_XMPP_PORT");

    // CLI --xmpp-port parser supports both `--xmpp-port <N>` and `--xmpp-port=<N>` and rejects invalid values.
    assert_eq!(ServerConfig::parse_cli_xmpp_port(Vec::<&str>::new()).unwrap(), None);
    assert_eq!(
        ServerConfig::parse_cli_xmpp_port(["--xmpp-port", "25232"]).unwrap(),
        Some(25_232)
    );
    assert_eq!(
        ServerConfig::parse_cli_xmpp_port(["--xmpp-port=0"]).unwrap(),
        Some(0)
    );
    assert!(ServerConfig::parse_cli_xmpp_port(["--xmpp-port"]).is_err());
    assert!(ServerConfig::parse_cli_xmpp_port(["--xmpp-port=bad"]).is_err());

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn storage_backend_validation_and_server_context_initialization_877_907() {
    let _lock = crate::lock_test_env_write();
    let _guard = EnvVarGuard::capture(&[
        "EVOMU_CONFIG_PATH",
        "EVOMU_DB_PATH",
        "EVOMU_SNAPSHOT_PATH",
        "EVOMU_STORAGE_BACKEND",
    ]);

    // Default storage backend is "sqlite" with `.sqlite` path.
    let def = ServerConfig::default();
    assert_eq!(def.storage.backend, "sqlite");
    assert_eq!(def.validate_storage_backend().unwrap(), "sqlite");

    // `.sqlite`, `.db`, and `.sqlite3` are accepted when backend = "sqlite".
    for valid_path in [
        "data/world.sqlite",
        "data/world.db",
        "data/world.sqlite3",
    ] {
        let cfg = ServerConfig::from_toml_str(&format!(
            "world_db_path = \"{valid_path}\"\n[storage]\nbackend = \"sqlite\"\n"
        ))
        .expect("valid sqlite path");
        assert_eq!(cfg.storage.backend, "sqlite");
    }

    // `.json` or non-SQLite extensions are refused when backend = "sqlite" (#877).
    let json_err = ServerConfig::from_toml_str(
        "world_db_path = \"data/evomu_world.json\"\n[storage]\nbackend = \"sqlite\"\n",
    )
    .unwrap_err();
    assert!(json_err.contains("must use a SQLite file extension"));

    // Unknown storage.backend values are refused (#877).
    let unknown_err =
        ServerConfig::from_toml_str("[storage]\nbackend = \"postgres\"\n").unwrap_err();
    assert!(unknown_err.contains("Unsupported storage.backend"));

    // `storage.backend = "memory"` succeeds regardless of `world_db_path` and initializes an ephemeral context.
    let mem_cfg = ServerConfig::from_toml_str(
        "world_db_path = \"ephemeral\"\n[storage]\nbackend = \"memory\"\n",
    )
    .expect("memory backend valid");
    assert_eq!(mem_cfg.validate_storage_backend().unwrap(), "memory");
    let mem_ctx = crate::router::ServerContext::from_config(&mem_cfg)
        .expect("ServerContext::from_config with memory backend");
    assert!(mem_ctx.db.sqlite_path().is_none());

    // `ServerContext::from_sqlite` rejects `.json` paths as well.
    let bad_sqlite = crate::router::ServerContext::from_sqlite("data/world_snapshot.json");
    assert!(bad_sqlite.is_err());

    // `EVOMU_STORAGE_BACKEND` env var overrides `[storage] backend`.
    std::env::remove_var("EVOMU_CONFIG_PATH");
    std::env::set_var("EVOMU_STORAGE_BACKEND", "memory");
    let env_mem = ServerConfig::load_default_or_env().expect("env memory backend");
    assert_eq!(env_mem.storage.backend, "memory");

    std::env::set_var("EVOMU_STORAGE_BACKEND", "invalid_backend");
    assert!(ServerConfig::load_default_or_env().is_err());
    std::env::remove_var("EVOMU_STORAGE_BACKEND");
}

#[test]
fn static_data_override_cli_and_env_flags() {
    let _env_guard = crate::lock_test_env_write();
    let _guard = EnvVarGuard::capture(&["EVOMU_ALLOW_STALE_STATIC_DATA"]);
    assert!(ServerConfig::parse_cli_allow_stale_static_data([
        "--gm",
        "--allow-stale-static-data"
    ]));
    assert!(!ServerConfig::parse_cli_allow_stale_static_data([
        "--",
        "--allow-stale-static-data"
    ]));
    assert!(!ServerConfig::parse_cli_allow_stale_static_data(["--gm"]));

    std::env::remove_var("EVOMU_ALLOW_STALE_STATIC_DATA");
    assert!(!ServerConfig::env_allow_stale_static_data());
    std::env::set_var("EVOMU_ALLOW_STALE_STATIC_DATA", "1");
    assert!(ServerConfig::env_allow_stale_static_data());
    std::env::set_var("EVOMU_ALLOW_STALE_STATIC_DATA", "true");
    assert!(ServerConfig::env_allow_stale_static_data());
    std::env::set_var("EVOMU_ALLOW_STALE_STATIC_DATA", "0");
    assert!(!ServerConfig::env_allow_stale_static_data());
    std::env::remove_var("EVOMU_ALLOW_STALE_STATIC_DATA");
}

#[test]
fn rate_multipliers_parse_defaults_aliases_env_overrides_and_validation() {
    let _lock = crate::lock_test_env_write();
    let _guard = EnvVarGuard::capture(&[
        "EVOMU_CONFIG_PATH",
        "EVOMU_SKILL_TRAINING_MULTIPLIER",
        "EVOMU_MINING_YIELD_MULTIPLIER",
        "EVOMU_STORAGE_BACKEND",
    ]);

    let def = ServerConfig::default();
    assert_eq!(def.skill_training_multiplier(), 1.0);
    assert_eq!(def.mining_yield_multiplier(), 1.0);

    let empty_cfg = ServerConfig::from_toml_str("").expect("empty config");
    assert_eq!(empty_cfg.skill_training_multiplier(), 1.0);
    assert_eq!(empty_cfg.mining_yield_multiplier(), 1.0);

    let repo_cfg = ServerConfig::from_toml_str(include_str!("../../../evomu.toml"))
        .expect("repo evomu.toml parses");
    assert_eq!(repo_cfg.skill_training_multiplier(), 1.0);
    assert_eq!(repo_cfg.mining_yield_multiplier(), 1.0);

    // Top-level keys accept integers and floats.
    let top = ServerConfig::from_toml_str(
        "skill_training_multiplier = 100.0\nmining_yield_multiplier = 5\n",
    )
    .expect("top-level rates");
    assert_eq!(top.skill_training_multiplier(), 100.0);
    assert_eq!(top.mining_yield_multiplier(), 5.0);

    // [rates] alias section parses cleanly and roundtrips via toml::to_string.
    let rates = ServerConfig::from_toml_str(
        "[rates]\nskill_training_multiplier = 25.0\nmining_yield_multiplier = 2.5\n",
    )
    .expect("[rates] section");
    assert_eq!(rates.skill_training_multiplier(), 25.0);
    assert_eq!(rates.mining_yield_multiplier(), 2.5);
    let rendered = toml::to_string(&rates).expect("serialize ServerConfig");
    let reparsed = ServerConfig::from_toml_str(&rendered).expect("roundtrip ServerConfig");
    assert_eq!(rates, reparsed);

    // [feature_flags] alias keys work identically.
    let ff = ServerConfig::from_toml_str(
        "[feature_flags]\nskill_training_multiplier = 10\nmining_yield_multiplier = 3.5\n",
    )
    .expect("[feature_flags] rates");
    assert_eq!(ff.skill_training_multiplier(), 10.0);
    assert_eq!(ff.mining_yield_multiplier(), 3.5);

    // Matching explicit aliases are accepted; conflicting values across sections are rejected.
    let matching = ServerConfig::from_toml_str(
        "skill_training_multiplier = 10\n[rates]\nskill_training_multiplier = 10.0\n",
    )
    .expect("matching aliases succeed");
    assert_eq!(matching.skill_training_multiplier(), 10.0);

    let conflict = ServerConfig::from_toml_str(
        "skill_training_multiplier = 5.0\n[rates]\nskill_training_multiplier = 10.0\n",
    )
    .unwrap_err();
    assert!(conflict.contains("Conflicting skill_training_multiplier"));

    let ff_conflict = ServerConfig::from_toml_str(
        "[rates]\nmining_yield_multiplier = 2.0\n[feature_flags]\nmining_yield_multiplier = 4.0\n",
    )
    .unwrap_err();
    assert!(ff_conflict.contains("Conflicting mining_yield_multiplier"));

    // Unknown keys inside [rates] fail fast instead of silently falling back to 1.0.
    assert!(ServerConfig::from_toml_str("[rates]\nskill_multiplier = 5.0\n").is_err());

    // Invalid (<= 0, non-finite, non-numeric) TOML values fail validation across every alias.
    for bad_val in ["0", "0.0", "-1.0", "nan", "inf", "-inf", "\"fast\""] {
        assert!(
            ServerConfig::from_toml_str(&format!("skill_training_multiplier = {bad_val}\n"))
                .is_err(),
            "top-level skill_training_multiplier={bad_val} must fail"
        );
        assert!(
            ServerConfig::from_toml_str(&format!("[rates]\nmining_yield_multiplier = {bad_val}\n"))
                .is_err(),
            "[rates].mining_yield_multiplier={bad_val} must fail"
        );
        assert!(
            ServerConfig::from_toml_str(&format!(
                "[feature_flags]\nskill_training_multiplier = {bad_val}\n"
            ))
            .is_err(),
            "[feature_flags].skill_training_multiplier={bad_val} must fail"
        );
    }

    // Programmatic invalid rate values are caught before initializing ServerContext.
    let mut manual_bad = ServerConfig::default();
    manual_bad.storage.backend = "memory".into();
    manual_bad.feature_flags.mining_yield_multiplier = 0.0;
    assert!(manual_bad.validate_rates().is_err());
    assert!(crate::router::ServerContext::from_config(&manual_bad).is_err());

    // Valid rates propagate from ServerConfig -> ServerContext.
    let ctx_cfg = ServerConfig::from_toml_str(
        "world_db_path = \"ephemeral\"\n[storage]\nbackend = \"memory\"\n[rates]\nskill_training_multiplier = 100.0\nmining_yield_multiplier = 5.0\n",
    )
    .expect("memory ServerConfig with rates");
    let ctx = crate::router::ServerContext::from_config(&ctx_cfg).expect("ServerContext from_config");
    assert_eq!(ctx.skill_training_multiplier(), 100.0);
    assert_eq!(ctx.mining_yield_multiplier(), 5.0);

    // Env overrides override evomu.toml and reject invalid values; empty strings keep TOML values.
    let tmp_dir = std::env::temp_dir().join(format!("evomu_rates_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let cfg_path = tmp_dir.join("rates.toml");
    std::fs::write(
        &cfg_path,
        "[storage]\nbackend = \"memory\"\n[rates]\nskill_training_multiplier = 4.0\nmining_yield_multiplier = 2.0\n",
    )
    .unwrap();
    std::env::set_var("EVOMU_CONFIG_PATH", &cfg_path);
    std::env::set_var("EVOMU_SKILL_TRAINING_MULTIPLIER", "");
    std::env::set_var("EVOMU_MINING_YIELD_MULTIPLIER", "   ");
    let kept = ServerConfig::load_default_or_env().expect("empty rate env keeps TOML");
    assert_eq!(kept.skill_training_multiplier(), 4.0);
    assert_eq!(kept.mining_yield_multiplier(), 2.0);

    std::env::set_var("EVOMU_SKILL_TRAINING_MULTIPLIER", "100");
    std::env::set_var("EVOMU_MINING_YIELD_MULTIPLIER", "7.5");
    let overridden = ServerConfig::load_default_or_env().expect("valid rate env overrides");
    assert_eq!(overridden.skill_training_multiplier(), 100.0);
    assert_eq!(overridden.mining_yield_multiplier(), 7.5);

    for bad_env in ["0", "-2.0", "NaN", "inf", "abc"] {
        std::env::set_var("EVOMU_SKILL_TRAINING_MULTIPLIER", bad_env);
        assert!(
            ServerConfig::load_default_or_env().is_err(),
            "EVOMU_SKILL_TRAINING_MULTIPLIER={bad_env} must fail"
        );
        std::env::remove_var("EVOMU_SKILL_TRAINING_MULTIPLIER");

        std::env::set_var("EVOMU_MINING_YIELD_MULTIPLIER", bad_env);
        assert!(
            ServerConfig::load_default_or_env().is_err(),
            "EVOMU_MINING_YIELD_MULTIPLIER={bad_env} must fail"
        );
        std::env::remove_var("EVOMU_MINING_YIELD_MULTIPLIER");
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn scheduled_downtime_enabled_parses_env_overrides_and_propagates_to_context() {
    let _lock = crate::lock_test_env_write();
    let _guard = EnvVarGuard::capture(&[
        "EVOMU_CONFIG_PATH",
        "EVOMU_SCHEDULED_DOWNTIME_ENABLED",
        "EVOMU_STORAGE_BACKEND",
    ]);

    let default_cfg = ServerConfig::from_toml_str(
        "world_db_path = \"ephemeral\"\n[storage]\nbackend = \"memory\"\n",
    )
    .expect("default scheduled_downtime_enabled");
    assert!(default_cfg.scheduled_downtime_enabled);
    let default_ctx =
        crate::router::ServerContext::from_config(&default_cfg).expect("default context");
    assert!(crate::services::serverops_cluster::is_scheduled_downtime_enabled(
        &default_ctx
    ));

    let disabled_cfg = ServerConfig::from_toml_str(
        "world_db_path = \"ephemeral\"\nscheduled_downtime_enabled = false\n[storage]\nbackend = \"memory\"\n",
    )
    .expect("scheduled_downtime_enabled = false");
    assert!(!disabled_cfg.scheduled_downtime_enabled);
    let rendered = toml::to_string(&disabled_cfg).expect("serialize ServerConfig");
    let reparsed = ServerConfig::from_toml_str(&rendered).expect("roundtrip ServerConfig");
    assert_eq!(disabled_cfg, reparsed);

    let disabled_ctx =
        crate::router::ServerContext::from_config(&disabled_cfg).expect("disabled context");
    assert!(!crate::services::serverops_cluster::is_scheduled_downtime_enabled(
        &disabled_ctx
    ));

    let tmp_dir = std::env::temp_dir().join(format!(
        "evomu_scheduled_dt_test_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let cfg_path = tmp_dir.join("downtime.toml");
    std::fs::write(
        &cfg_path,
        "scheduled_downtime_enabled = true\n[storage]\nbackend = \"memory\"\n",
    )
    .unwrap();
    std::env::set_var("EVOMU_CONFIG_PATH", &cfg_path);

    // Unrecognized or empty env override keeps TOML value; falsy/truthy overrides apply.
    std::env::set_var("EVOMU_SCHEDULED_DOWNTIME_ENABLED", "maybe");
    assert!(
        ServerConfig::load_default_or_env()
            .expect("invalid bool keeps TOML")
            .scheduled_downtime_enabled
    );
    std::env::set_var("EVOMU_SCHEDULED_DOWNTIME_ENABLED", "false");
    assert!(
        !ServerConfig::load_default_or_env()
            .expect("false env disables")
            .scheduled_downtime_enabled
    );
    std::env::set_var("EVOMU_SCHEDULED_DOWNTIME_ENABLED", "1");
    assert!(
        ServerConfig::load_default_or_env()
            .expect("1 env enables")
            .scheduled_downtime_enabled
    );

    let _ = std::fs::remove_dir_all(&tmp_dir);
}
