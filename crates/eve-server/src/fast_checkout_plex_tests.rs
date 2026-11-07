//! Unit tests for configurable free Fast-Checkout PLEX packs, starter PLEX grants,
//! and New Eden Store PLEX spending.

#![forbid(unsafe_code)]

use crate::config::{FeatureFlagsConfig, PlayerAccountConfig, ServerConfig, StarterCharacterConfig};
use crate::router::ServerContext;
use crate::services::quasar_payment_store::get_plex_transactions;
use crate::starter_characters::provision_starter_characters;
use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        Arc::new(SdeRegistry::open(SdeRegistry::default_db_path()).expect("open default SDE"))
    }))
}

pub(super) fn test_ctx(free_fast_checkout_plex: bool, starter_plex_balance: i64) -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let mut ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_free_fast_checkout_plex(free_fast_checkout_plex);
    ctx.feature_flags.starter_plex_balance = starter_plex_balance;
    ctx
}

pub(super) fn starter_cfg(name: &str, initial_balance_plex: Option<i64>) -> StarterCharacterConfig {
    StarterCharacterConfig {
        name: name.to_string(),
        bloodline_id: 1,
        gender: 0,
        ancestry_id: 11,
        school_id: 17,
        solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760),
        initial_balance_isk: Some(1_000_000_000),
        initial_balance_plex,
    }
}

pub(super) fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    let PyRep::Dict(entries) = rep else {
        return None;
    };
    entries
        .iter()
        .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
        .map(|(_, v)| v)
}

#[test]
fn config_parses_fast_checkout_and_starter_plex_flags_and_validates_bounds() {
    let def_flags = FeatureFlagsConfig::default();
    assert!(!def_flags.free_fast_checkout_plex);
    assert_eq!(def_flags.starter_plex_balance, 0);
    assert_eq!(StarterCharacterConfig::default().initial_balance_plex, None);

    let repo_toml = include_str!("../../../evomu.toml");
    let repo_cfg = ServerConfig::from_toml_str(repo_toml).expect("parse repo evomu.toml");
    assert!(repo_cfg.feature_flags.free_fast_checkout_plex);
    assert_eq!(repo_cfg.feature_flags.starter_plex_balance, 50_000);

    let custom_toml = r#"
[feature_flags]
free_fast_checkout_plex = true
starter_plex_balance = 12500

[[player_accounts]]
username = "alpha"
password_token = "secret"

[player_accounts.starter_character]
name = "Alpha Pilot"
bloodline_id = 1
gender = 0
ancestry_id = 11
school_id = 17
initial_balance_plex = 7500
"#;
    let custom_cfg = ServerConfig::from_toml_str(custom_toml).expect("parse custom config");
    assert!(custom_cfg.feature_flags.free_fast_checkout_plex);
    assert_eq!(custom_cfg.feature_flags.starter_plex_balance, 12_500);
    assert_eq!(
        custom_cfg.player_accounts[0]
            .starter_character
            .as_ref()
            .and_then(|s| s.initial_balance_plex),
        Some(7_500)
    );

    let neg_global = ServerConfig::from_toml_str("[feature_flags]\nstarter_plex_balance = -1\n")
        .expect_err("negative starter_plex_balance must error");
    assert!(neg_global.contains("starter_plex_balance"));

    let neg_char = ServerConfig::from_toml_str(
        "[[player_accounts]]\nusername = \"bad\"\npassword_token = \"pw\"\n\
         [player_accounts.starter_character]\nname = \"Bad\"\nbloodline_id = 1\n\
         gender = 0\nancestry_id = 11\nschool_id = 17\ninitial_balance_plex = -5\n",
    )
    .expect_err("negative initial_balance_plex must error");
    assert!(neg_char.contains("initial_balance_plex"));
}

#[tokio::test]
async fn create_character_and_starter_provisioning_plex_grants_and_sqlite_roundtrip() {
    // Direct CreateCharacterWithDoll with starter_plex_balance = 0 grants 0 PLEX
    let ctx_zero = test_ctx(true, 0);
    let uid0 = ctx_zero.db.provision_player_account("client0", "pw");
    let mut sess0 = SessionState::new(0);
    sess0.user_id = Some(uid0);
    let create_args = vec![
        PyRep::String("Client Zero Pilot".into()),
        PyRep::Integer(1),
        PyRep::Integer(0),
        PyRep::Integer(11),
        PyRep::Dict(vec![
            (PyRep::String("bloodlineID".into()), PyRep::Integer(1)),
            (PyRep::String("genderID".into()), PyRep::Integer(0)),
        ]),
        PyRep::Dict(vec![(PyRep::String("poseSelection".into()), PyRep::Integer(0))]),
        PyRep::Integer(17),
    ];
    let cid0 = ctx_zero
        .dispatch_rpc(
            &mut sess0,
            "charUnboundMgr",
            None,
            "CreateCharacterWithDoll",
            &create_args,
            &HashMap::new(),
            None,
        )
        .await
        .expect("rpc ok")
        .as_i64()
        .expect("char id");
    assert_eq!(ctx_zero.db.with_snapshot(|s| s.plex_vault_balance(cid0)), 0);

    // Direct CreateCharacterWithDoll with starter_plex_balance = 50_000 grants 50_000 PLEX
    let ctx_client = test_ctx(true, 50_000);
    let uid_client = ctx_client.db.provision_player_account("client50k", "pw");
    let mut sess_client = SessionState::new(0);
    sess_client.user_id = Some(uid_client);
    let cid_client = ctx_client
        .dispatch_rpc(
            &mut sess_client,
            "charUnboundMgr",
            None,
            "CreateCharacterWithDoll",
            &create_args,
            &HashMap::new(),
            None,
        )
        .await
        .expect("rpc ok")
        .as_i64()
        .expect("char id");
    assert_eq!(
        ctx_client.db.with_snapshot(|s| s.plex_vault_balance(cid_client)),
        50_000
    );

    // Startup provisioning: default None -> 50_000 (no double credit), Some(25_000) -> 25_000, Some(0) -> 0
    let ctx = test_ctx(true, 50_000);
    let uid_def = ctx.db.provision_player_account("starter_def", "pw");
    let uid_custom = ctx.db.provision_player_account("starter_custom", "pw");
    let uid_none_plex = ctx.db.provision_player_account("starter_zero", "pw");
    let mut cfg = ServerConfig::default();
    cfg.feature_flags.starter_plex_balance = 50_000;
    cfg.player_accounts = vec![
        PlayerAccountConfig {
            username: "starter_def".into(),
            password_token: "pw".into(),
            starter_character: Some(starter_cfg("Starter Default Plex", None)),
        },
        PlayerAccountConfig {
            username: "starter_custom".into(),
            password_token: "pw".into(),
            starter_character: Some(starter_cfg("Starter Custom Plex", Some(25_000))),
        },
        PlayerAccountConfig {
            username: "starter_zero".into(),
            password_token: "pw".into(),
            starter_character: Some(starter_cfg("Starter Zero Plex", Some(0))),
        },
    ];

    let created = provision_starter_characters(&ctx, &cfg).await;
    assert_eq!(created.len(), 3);
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(created[0])), 50_000);
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(created[1])), 25_000);
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(created[2])), 0);

    // Re-running startup provisioning does not double-credit
    let created_again = provision_starter_characters(&ctx, &cfg).await;
    assert!(created_again.is_empty());
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(created[0])), 50_000);

    let txs = ctx.db.with_snapshot(get_plex_transactions);
    assert!(txs.iter().any(|t| t.user_id == uid_def && t.amount == 50_000 && t.description == "Starter PLEX Grant"));
    assert!(txs.iter().any(|t| t.user_id == uid_custom && t.amount == 25_000 && t.description == "Starter PLEX Grant"));
    assert!(!txs.iter().any(|t| t.user_id == uid_none_plex));

    // SQLite restart persistence check
    let tmp_dir = std::env::temp_dir().join(format!(
        "evomu_starter_plex_test_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    let db_path = tmp_dir.join("world.sqlite");
    let mut cfg1 = ServerConfig::default();
    cfg1.sde_path = SdeRegistry::default_db_path().display().to_string();
    cfg1.world_db_path = db_path.display().to_string();
    cfg1.feature_flags.starter_plex_balance = 50_000;
    cfg1.player_accounts = vec![PlayerAccountConfig {
        username: "persist_pilot".into(),
        password_token: "pw".into(),
        starter_character: Some(starter_cfg("Persist Pilot", None)),
    }];
    let ctx1 = ServerContext::from_config(&cfg1).expect("from_config");
    let ids = provision_starter_characters(&ctx1, &cfg1).await;
    assert_eq!(ids.len(), 1);
    drop(ctx1);

    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let (bal_after, txs_after) =
        repo2.with_snapshot(|s| (s.plex_vault_balance(ids[0]), get_plex_transactions(s)));
    assert_eq!(bal_after, 50_000);
    assert!(txs_after.iter().any(|t| t.amount == 50_000 && t.description == "Starter PLEX Grant"));
    let _ = std::fs::remove_dir_all(&tmp_dir);
}
