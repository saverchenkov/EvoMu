//! Deep branch-coverage and mutation-killing unit tests for `starter_characters`.

#![forbid(unsafe_code)]

use crate::config::{FeatureFlagsConfig, PlayerAccountConfig, ServerConfig, StarterCharacterConfig};
use crate::router::ServerContext;
use crate::starter_characters::{
    grant_starter_plex_in_snapshot, provision_starter_characters, should_grant_default_starter_plex,
};
use eve_db::{IskCents, ServerClock, VirtualServerClock, WorldRepository};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        Arc::new(SdeRegistry::open(SdeRegistry::default_db_path()).expect("open default SDE"))
    }))
}

fn fast_test_context(base_unix: u64) -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db));
    let clock = Arc::new(VirtualServerClock::with_fixed_base(base_unix));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    ctx
}

#[tokio::test]
async fn starter_characters_task_local_and_plex_snapshot_grants_cover_all_branches() {
    assert!(should_grant_default_starter_plex());

    let ctx = fast_test_context(1_800_000_000);
    ctx.db.with_mut_snapshot(|snap| {
        // 1. Non-positive grants (0 and <0) are exact no-ops
        grant_starter_plex_in_snapshot(snap, 801, 90_000_001, 0, 1_800_000_000);
        grant_starter_plex_in_snapshot(snap, 801, 90_000_001, -75, 1_800_000_000);
        assert_eq!(snap.account_plex_vault.get(&801).copied(), None);

        // 2. Character ID not in `snap.characters` uses `account_plex_vault` fallback and adds to non-zero prior balance
        snap.account_plex_vault.insert(802, 300);
        grant_starter_plex_in_snapshot(snap, 802, 999_777_666, 250, 1_800_000_000);
        assert_eq!(snap.account_plex_vault.get(&802).copied(), Some(550));

        // 3. Existing character updates vault and persists ledger state
        let ch = snap.characters[&90_000_001].clone();
        let before_bal = snap.account_plex_vault.get(&ch.user_id).copied().unwrap_or(0);
        grant_starter_plex_in_snapshot(snap, ch.user_id, ch.character_id, 120, 1_800_000_000);
        assert_eq!(snap.account_plex_vault.get(&ch.user_id).copied(), Some(before_bal + 120));
        assert!(!snap.quasar_state_blobs.is_empty());
    });
}

#[tokio::test]
async fn provision_starter_characters_three_tier_plex_precedence_and_error_isolation() {
    let ctx = fast_test_context(1_800_000_000);
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .starter_plex_balance = 125;

    let uid_none_cfg = ctx.db.provision_player_account("acct_no_starter", "pw1");
    let uid_existing = ctx.db.provision_player_account("acct_existing_char", "pw2");
    let uid_flag = ctx.db.provision_player_account("acct_ctx_flag", "pw_flag");
    let uid_override = ctx.db.provision_player_account("acct_explicit_plex", "pw_ovr");

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&90_000_001].clone();
        ch.character_id = 90_077_001;
        ch.user_id = uid_existing;
        ch.name = "ExistingPilot".into();
        snap.characters.insert(ch.character_id, ch);
    });

    let mut cfg = ServerConfig::default();
    cfg.feature_flags = FeatureFlagsConfig {
        starter_plex_balance: 50,
        ..Default::default()
    };
    cfg.player_accounts = vec![
        PlayerAccountConfig {
            username: "acct_no_starter".into(),
            password_token: "pw1".into(),
            starter_character: None,
        },
        PlayerAccountConfig {
            username: "acct_existing_char".into(),
            password_token: "pw2".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "ShouldSkipExisting".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                ..Default::default()
            }),
        },
        PlayerAccountConfig {
            username: "missing_or_wrong_pw".into(),
            password_token: "wrong".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "AuthFailed".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                ..Default::default()
            }),
        },
        // Whitespace-trimmed credentials + explicit solar_system_id + inherits ctx.feature_flags.starter_plex_balance (125)
        PlayerAccountConfig {
            username: "  acct_ctx_flag  ".into(),
            password_token: "  pw_flag  ".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "CtxFlagPilot".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                solar_system_id: Some(30_000_142),
                station_id: Some(60_008_494),
                initial_balance_isk: None,
                initial_balance_plex: None,
            }),
        },
        // Explicit initial_balance_plex (750) overrides ctx.feature_flags (125) without double-granting
        PlayerAccountConfig {
            username: "acct_explicit_plex".into(),
            password_token: "pw_ovr".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "ExplicitPlexPilot".into(),
                bloodline_id: 3,
                gender: 0,
                ancestry_id: 19,
                school_id: 14,
                solar_system_id: None,
                station_id: Some(60_008_494),
                initial_balance_isk: Some(7_777_000),
                initial_balance_plex: Some(750),
            }),
        },
        // Invalid bloodline/school fails RPC gracefully without aborting loop
        PlayerAccountConfig {
            username: "acct_no_starter".into(),
            password_token: "pw1".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "InvalidBloodline".into(),
                bloodline_id: 999_999,
                gender: 1,
                ancestry_id: 999_999,
                school_id: 999_999,
                ..Default::default()
            }),
        },
    ];

    let created1 = provision_starter_characters(&ctx, &cfg).await;
    assert_eq!(created1.len(), 2);
    assert!(ctx.db.get_characters_for_user(uid_none_cfg).is_empty());

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.account_plex_vault.get(&uid_flag).copied(), Some(125));
        assert_eq!(snap.account_plex_vault.get(&uid_override).copied(), Some(750));
        let ch_flag = &snap.characters[&created1[0]];
        assert_eq!(ch_flag.station_id, Some(60_008_494));
        assert_eq!(ch_flag.home_station_id, 60_008_494);
        assert_eq!(snap.items[&ch_flag.active_ship_id].location_id, 60_008_494);
        let ch_ovr = &snap.characters[&created1[1]];
        assert_eq!(ch_ovr.balance, IskCents::from_isk(7_777_000));
    });

    // Second pass with ctx.feature_flags.starter_plex_balance = 0 falls back to cfg.feature_flags.starter_plex_balance (50)
    let ctx_cfg_fallback = fast_test_context(1_800_001_000);
    ctx_cfg_fallback
        .feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .starter_plex_balance = 0;
    let uid_cfg_fallback = ctx_cfg_fallback.db.provision_player_account("acct_cfg_fallback", "pw_cfg");
    let uid_zero_plex = ctx_cfg_fallback.db.provision_player_account("acct_zero_plex", "pw_zero");
    let mut cfg2 = ServerConfig::default();
    cfg2.feature_flags.starter_plex_balance = 50;
    cfg2.player_accounts = vec![
        PlayerAccountConfig {
            username: "acct_cfg_fallback".into(),
            password_token: "pw_cfg".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "CfgFallbackPilot".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                ..Default::default()
            }),
        },
        // Explicit 0 PLEX suppresses grant even when feature flag is non-zero
        PlayerAccountConfig {
            username: "acct_zero_plex".into(),
            password_token: "pw_zero".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "ZeroPlexPilot".into(),
                bloodline_id: 3,
                gender: 0,
                ancestry_id: 19,
                school_id: 14,
                initial_balance_plex: Some(0),
                ..Default::default()
            }),
        },
    ];
    let created2 = provision_starter_characters(&ctx_cfg_fallback, &cfg2).await;
    assert_eq!(created2.len(), 2);
    ctx_cfg_fallback.db.with_snapshot(|snap| {
        assert_eq!(snap.account_plex_vault.get(&uid_cfg_fallback).copied(), Some(50));
        assert_eq!(snap.account_plex_vault.get(&uid_zero_plex).copied(), None);
    });
}
