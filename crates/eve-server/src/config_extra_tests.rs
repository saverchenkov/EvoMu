//! Additional unit and mutation-killing tests for `config`.

#![forbid(unsafe_code)]

use super::*;
use uuid::Uuid;

#[test]
fn test_plex_balances_and_feature_flag_boundary_validations() {
    // 1. feature_flags.starter_plex_balance: -1 fails, 0 succeeds, positive succeeds
    let neg_starter_plex = r#"
[feature_flags]
starter_plex_balance = -1
"#;
    let err = ServerConfig::from_toml_str(neg_starter_plex).unwrap_err();
    assert!(
        err.contains("feature_flags.starter_plex_balance in evomu.toml must be >= 0"),
        "unexpected error: {err}"
    );

    let zero_starter_plex = r#"
[feature_flags]
starter_plex_balance = 0
free_fast_checkout_plex = true
"#;
    let cfg0 = ServerConfig::from_toml_str(zero_starter_plex).expect("0 starter_plex_balance is valid");
    assert_eq!(cfg0.feature_flags.starter_plex_balance, 0);
    assert!(cfg0.feature_flags.free_fast_checkout_plex);

    let pos_starter_plex = r#"
[feature_flags]
starter_plex_balance = 500
"#;
    let cfg_pos =
        ServerConfig::from_toml_str(pos_starter_plex).expect("positive starter_plex_balance is valid");
    assert_eq!(cfg_pos.feature_flags.starter_plex_balance, 500);

    // 2. player_accounts[].starter_character.initial_balance_plex: -1 fails, 0 and positive succeed
    let neg_char_plex = r#"
[[player_accounts]]
username = "capsuleer_neg"
password_token = "pw"

[player_accounts.starter_character]
name = "NegPlexPilot"
bloodline_id = 1
gender = 1
ancestry_id = 1
school_id = 11
initial_balance_plex = -1
"#;
    let char_err = ServerConfig::from_toml_str(neg_char_plex).unwrap_err();
    assert!(
        char_err.contains("player_accounts[capsuleer_neg].starter_character.initial_balance_plex"),
        "unexpected error: {char_err}"
    );

    let zero_char_plex = r#"
[[player_accounts]]
username = "capsuleer_zero"
password_token = "pw"

[player_accounts.starter_character]
name = "ZeroPlexPilot"
bloodline_id = 1
gender = 1
ancestry_id = 1
school_id = 11
initial_balance_plex = 0
"#;
    let cfg_char0 =
        ServerConfig::from_toml_str(zero_char_plex).expect("0 initial_balance_plex is valid");
    assert_eq!(
        cfg_char0.player_accounts[0]
            .starter_character
            .as_ref()
            .unwrap()
            .initial_balance_plex,
        Some(0)
    );
}

#[test]
fn test_admin_account_custom_fields_and_toml_serde_roundtrip() {
    let toml_input = r#"
bind_address = "192.168.10.20"
macho_port = 26000
bridge_port = 26002
esi_port = 26004
quasar_grpc_port = 26099
sde_path = "data/evomu_sde.sqlite"
world_db_path = "data/evomu_world.sqlite"
belt_respawn_interval_secs = 86400
downtime_utc_secs_of_day = 39600

[[admin_accounts]]
username = "custom_gm"
password_token = "gm_tok"
character_id = 90000777
character_name = "ISD Aurora"

[[player_accounts]]
username = "bare_pilot"
password_token = "pilot_tok"
"#;
    let parsed = ServerConfig::from_toml_str(toml_input).expect("parse custom admin & player config");
    assert_eq!(parsed.quasar_grpc_port, 26_099);
    assert_eq!(parsed.admin_accounts.len(), 1);
    assert_eq!(parsed.admin_accounts[0].username, "custom_gm");
    assert_eq!(parsed.admin_accounts[0].character_id, 90_000_777);
    assert_eq!(parsed.admin_accounts[0].character_name, "ISD Aurora");
    assert_eq!(parsed.player_accounts.len(), 1);
    assert!(parsed.player_accounts[0].starter_character.is_none());

    // Roundtrip through TOML serializer and load_from_toml on disk
    let serialized = toml::to_string(&parsed).expect("serialize ServerConfig to TOML");
    let reparsed = ServerConfig::from_toml_str(&serialized).expect("reparse serialized TOML");
    assert_eq!(parsed, reparsed);

    let tmp_path = std::env::temp_dir().join(format!("evomu_cfg_extra_{}.toml", Uuid::new_v4()));
    std::fs::write(&tmp_path, &serialized).expect("write temp config");
    let loaded = ServerConfig::load_from_toml(&tmp_path).expect("load_from_toml");
    assert_eq!(loaded, parsed);
    let _ = std::fs::remove_file(&tmp_path);
}

#[test]
fn test_exact_error_messages_for_invalid_fields() {
    assert_eq!(
        ServerConfig::from_toml_str("bind_address = \"   \"").unwrap_err(),
        "bind_address in evomu.toml must not be empty"
    );
    assert_eq!(
        ServerConfig::from_toml_str("sde_path = \"\"").unwrap_err(),
        "sde_path in evomu.toml must not be empty"
    );
    assert_eq!(
        ServerConfig::from_toml_str("world_db_path = \"\"").unwrap_err(),
        "world_db_path in evomu.toml must not be empty"
    );
    assert_eq!(
        ServerConfig::from_toml_str("belt_respawn_interval_secs = 0").unwrap_err(),
        "belt_respawn_interval_secs in evomu.toml must be in 1..=604800"
    );
    assert_eq!(
        ServerConfig::from_toml_str("belt_respawn_interval_secs = 604801").unwrap_err(),
        "belt_respawn_interval_secs in evomu.toml must be in 1..=604800"
    );
    assert_eq!(
        ServerConfig::from_toml_str("downtime_utc_secs_of_day = 86400").unwrap_err(),
        "downtime_utc_secs_of_day in evomu.toml must be < 86400"
    );
    assert_eq!(
        ServerConfig::from_toml_str("[feature_flags]\ninfo_gathering_interval_secs = 0").unwrap_err(),
        "feature_flags.info_gathering_interval_secs in evomu.toml must be >= 1"
    );
}
