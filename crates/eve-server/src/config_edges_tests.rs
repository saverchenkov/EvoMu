//! Edge-case, validation, and environment-override unit tests for `config`.

#![forbid(unsafe_code)]

use super::*;
use uuid::Uuid;

struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn capture(keys: &[&'static str]) -> Self {
        let saved = keys
            .iter()
            .map(|&k| (k, std::env::var(k).ok()))
            .collect();
        Self { saved }
    }
}

impl Drop for EnvGuard {
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
fn config_defaults_and_nested_player_account_validation_edges() {
    let starter = StarterCharacterConfig::default();
    assert_eq!(starter.name, "");
    assert_eq!(starter.bloodline_id, 0);
    assert_eq!(starter.gender, 0);
    assert_eq!(starter.ancestry_id, 0);
    assert_eq!(starter.school_id, 0);
    assert_eq!(starter.solar_system_id, None);
    assert_eq!(starter.station_id, None);
    assert_eq!(starter.initial_balance_isk, None);
    assert_eq!(starter.initial_balance_plex, None);

    let ff = FeatureFlagsConfig::default();
    assert!(!ff.free_fast_checkout_plex);
    assert_eq!(ff.starter_plex_balance, 0);
    assert!(!ff.allow_local_dev_gateway_jwt);

    // Multi-account config: first account valid with positive PLEX, second without starter, third negative PLEX
    let toml_ok = r#"
[[player_accounts]]
username = "p1"
password_token = "tok1"

[player_accounts.starter_character]
name = "Pilot One"
bloodline_id = 1
gender = 1
ancestry_id = 1
school_id = 11
initial_balance_plex = 250

[[player_accounts]]
username = "p2"
password_token = "tok2"
"#;
    let parsed = ServerConfig::from_toml_str(toml_ok).expect("multi-account config valid");
    assert_eq!(parsed.player_accounts.len(), 2);
    assert_eq!(
        parsed.player_accounts[0]
            .starter_character
            .as_ref()
            .unwrap()
            .initial_balance_plex,
        Some(250)
    );
    assert!(parsed.player_accounts[1].starter_character.is_none());

    let toml_bad = format!(
        "{toml_ok}\n[[player_accounts]]\nusername = \"p3\"\npassword_token = \"tok3\"\n\
         [player_accounts.starter_character]\nname = \"Bad\"\nbloodline_id = 1\ngender = 1\n\
         ancestry_id = 1\nschool_id = 11\ninitial_balance_plex = -5\n"
    );
    let err = ServerConfig::from_toml_str(&toml_bad).unwrap_err();
    assert!(
        err.contains("player_accounts[p3].starter_character.initial_balance_plex"),
        "unexpected error: {err}"
    );
}

#[test]
fn config_env_overrides_sde_gm_dev_jwt_and_error_paths() {
    let _lock = crate::lock_test_env_write();
    let _guard = EnvGuard::capture(&[
        "EVOMU_CONFIG_PATH",
        "EVOMU_SDE_PATH",
        "EVOMU_DB_PATH",
        "EVOMU_SNAPSHOT_PATH",
        "EVOMU_ACCOUNTS",
        "EVOMU_GM_ACCOUNT",
        "EVOMU_QUASAR_GRPC_PORT",
        "EVOMU_ALLOW_LOCAL_DEV_JWT",
    ]);

    for k in [
        "EVOMU_CONFIG_PATH",
        "EVOMU_SDE_PATH",
        "EVOMU_DB_PATH",
        "EVOMU_SNAPSHOT_PATH",
        "EVOMU_ACCOUNTS",
        "EVOMU_GM_ACCOUNT",
        "EVOMU_QUASAR_GRPC_PORT",
        "EVOMU_ALLOW_LOCAL_DEV_JWT",
    ] {
        std::env::remove_var(k);
    }

    let tmp_dir = std::env::temp_dir().join(format!("evomu_cfg_edges_{}", Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).unwrap();

    // 1. EVOMU_CONFIG_PATH pointing to missing file returns Err
    let missing_path = tmp_dir.join("does_not_exist.toml");
    std::env::set_var("EVOMU_CONFIG_PATH", missing_path.to_str().unwrap());
    let miss_err = ServerConfig::load_default_or_env().unwrap_err();
    assert!(miss_err.contains("Failed to read config file"));

    // 2. EVOMU_CONFIG_PATH pointing to invalid TOML returns Err
    let bad_toml = tmp_dir.join("invalid.toml");
    std::fs::write(&bad_toml, "bind_address = \"\"").unwrap();
    std::env::set_var("EVOMU_CONFIG_PATH", bad_toml.to_str().unwrap());
    let bad_err = ServerConfig::load_default_or_env().unwrap_err();
    assert!(bad_err.contains("bind_address in evomu.toml must not be empty"));

    // 3. Valid config file + EVOMU_SDE_PATH + trimmed EVOMU_DB_PATH + malformed GM/accounts + JWT flags
    let valid_toml = tmp_dir.join("valid.toml");
    std::fs::write(
        &valid_toml,
        "bind_address = \"10.0.0.5\"\n[feature_flags]\nallow_local_dev_gateway_jwt = false\n",
    )
    .unwrap();
    std::env::set_var("EVOMU_CONFIG_PATH", valid_toml.to_str().unwrap());
    std::env::set_var("EVOMU_SDE_PATH", "  /opt/evomu/sde.sqlite  ");
    std::env::set_var("EVOMU_DB_PATH", "   ");
    std::env::set_var("EVOMU_SNAPSHOT_PATH", "  /opt/evomu/snap.sqlite  ");
    // Malformed EVOMU_GM_ACCOUNT variants should be ignored without panicking
    for bad_gm in ["no_colon", ":only_pass", "only_user:", "  :  "] {
        std::env::set_var("EVOMU_GM_ACCOUNT", bad_gm);
        let cfg = ServerConfig::load_default_or_env().expect("bad gm spec ignored");
        assert!(cfg.admin_accounts.is_empty());
    }
    std::env::set_var("EVOMU_GM_ACCOUNT", " gm_lead : gm_secret ");
    std::env::set_var("EVOMU_ALLOW_LOCAL_DEV_JWT", "YES");

    let cfg = ServerConfig::load_default_or_env().expect("valid env overrides");
    assert_eq!(cfg.bind_address, "10.0.0.5");
    assert_eq!(cfg.sde_path, "/opt/evomu/sde.sqlite");
    // Whitespace-only EVOMU_DB_PATH keeps default world_db_path (does not fall through to EVOMU_SNAPSHOT_PATH)
    assert_eq!(cfg.world_db_path, "data/evomu_world.sqlite");
    assert_eq!(cfg.admin_accounts.len(), 1);
    assert_eq!(cfg.admin_accounts[0].username, "gm_lead");
    assert_eq!(cfg.admin_accounts[0].password_token, "gm_secret");
    assert!(cfg.feature_flags.allow_local_dev_gateway_jwt);

    // 4. Test all truthy and falsy EVOMU_ALLOW_LOCAL_DEV_JWT values + unknown value
    std::env::set_var("EVOMU_CONFIG_PATH", "  ");
    std::env::remove_var("EVOMU_DB_PATH");
    std::env::set_var("EVOMU_SNAPSHOT_PATH", "   ");
    std::env::set_var("EVOMU_SDE_PATH", "   ");

    for truthy in ["1", "true", "yes", "on", "ON"] {
        std::env::set_var("EVOMU_ALLOW_LOCAL_DEV_JWT", truthy);
        let c = ServerConfig::load_default_or_env().unwrap();
        assert!(c.feature_flags.allow_local_dev_gateway_jwt, "failed for {truthy}");
    }

    // Start from TOML with allow_local_dev_gateway_jwt = true and turn it off via falsy env vars
    std::fs::write(
        &valid_toml,
        "[feature_flags]\nallow_local_dev_gateway_jwt = true\n",
    )
    .unwrap();
    std::env::set_var("EVOMU_CONFIG_PATH", valid_toml.to_str().unwrap());
    for falsy in ["0", "false", "no", "off", "False"] {
        std::env::set_var("EVOMU_ALLOW_LOCAL_DEV_JWT", falsy);
        let c = ServerConfig::load_default_or_env().unwrap();
        assert!(!c.feature_flags.allow_local_dev_gateway_jwt, "failed for {falsy}");
    }

    // Unrecognized value leaves TOML value untouched
    std::env::set_var("EVOMU_ALLOW_LOCAL_DEV_JWT", "maybe");
    let c_unchanged = ServerConfig::load_default_or_env().unwrap();
    assert!(c_unchanged.feature_flags.allow_local_dev_gateway_jwt);

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn xmpp_port_defaults_env_filters_and_cli_parsing_edges() {
    assert_eq!(default_xmpp_port(), 5222);
    assert_eq!(ServerConfig::default().xmpp_port, 5222);
    assert_eq!(ServerConfig::from_toml_str("").unwrap().xmpp_port, 5222);

    assert_eq!(ServerConfig::parse_cli_xmpp_port(["evomu"]), Ok(None));
    assert_eq!(
        ServerConfig::parse_cli_xmpp_port(["evomu", "--xmpp-port", "5223"]),
        Ok(Some(5223))
    );
    assert_eq!(
        ServerConfig::parse_cli_xmpp_port(["evomu", "--xmpp-port=5224"]),
        Ok(Some(5224))
    );
    assert_eq!(
        ServerConfig::parse_cli_xmpp_port(["evomu", "--", "--xmpp-port", "5223"]),
        Ok(None)
    );
    assert!(ServerConfig::parse_cli_xmpp_port(["evomu", "--xmpp-port"]).is_err());
    assert!(ServerConfig::parse_cli_xmpp_port(["evomu", "--xmpp-port", "bad"]).is_err());

    let _lock = crate::lock_test_env_write();
    let _guard = EnvGuard::capture(&[
        "EVOMU_CONFIG_PATH",
        "EVOMU_SDE_PATH",
        "EVOMU_DB_PATH",
        "EVOMU_SNAPSHOT_PATH",
        "EVOMU_ACCOUNTS",
        "EVOMU_GM_ACCOUNT",
        "EVOMU_QUASAR_GRPC_PORT",
        "EVOMU_XMPP_PORT",
        "EVEMU_XMPP_PORT",
        "EVOMU_ALLOW_LOCAL_DEV_JWT",
    ]);
    for k in [
        "EVOMU_CONFIG_PATH",
        "EVOMU_SDE_PATH",
        "EVOMU_DB_PATH",
        "EVOMU_SNAPSHOT_PATH",
        "EVOMU_ACCOUNTS",
        "EVOMU_GM_ACCOUNT",
        "EVOMU_QUASAR_GRPC_PORT",
        "EVOMU_XMPP_PORT",
        "EVEMU_XMPP_PORT",
        "EVOMU_ALLOW_LOCAL_DEV_JWT",
    ] {
        std::env::remove_var(k);
    }

    let c_def = ServerConfig::load_default_or_env().unwrap();
    assert_eq!(c_def.xmpp_port, 5222);

    std::env::set_var("EVOMU_XMPP_PORT", " 5555 ");
    std::env::set_var("EVEMU_XMPP_PORT", "6666");
    assert_eq!(ServerConfig::load_default_or_env().unwrap().xmpp_port, 5555);

    std::env::set_var("EVOMU_XMPP_PORT", "   ");
    std::env::set_var("EVEMU_XMPP_PORT", " 5556 ");
    assert_eq!(ServerConfig::load_default_or_env().unwrap().xmpp_port, 5556);

    std::env::set_var("EVOMU_XMPP_PORT", "   ");
    std::env::set_var("EVEMU_XMPP_PORT", "   ");
    assert_eq!(ServerConfig::load_default_or_env().unwrap().xmpp_port, 5222);
}
