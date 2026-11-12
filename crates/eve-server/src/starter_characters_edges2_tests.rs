//! Additional edge-case and branch-coverage tests for `starter_characters`.

#![forbid(unsafe_code)]

use super::*;
use crate::config::{FeatureFlagsConfig, PlayerAccountConfig, ServerConfig, StarterCharacterConfig};
use crate::router::ServerContext;
use eve_db::{ServerClock, VirtualServerClock, WorldRepository};
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn should_grant_default_starter_plex_false_inside_task_local_scope() {
    assert!(should_grant_default_starter_plex());
    let inside = SKIP_DEFAULT_STARTER_PLEX
        .scope((), async { should_grant_default_starter_plex() })
        .await;
    assert!(!inside);
    assert!(should_grant_default_starter_plex());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provision_starter_characters_existing_char_plex_topup_and_empty_config() {
    let ctx = fast_test_context(1_850_000_000);

    // Empty config returns empty vec cleanly.
    let empty_cfg = ServerConfig::default();
    assert!(provision_starter_characters(&ctx, &empty_cfg).await.is_empty());

    // Provision three accounts that already own characters.
    let uid_topup = ctx.db.provision_player_account("acct_topup", "pw_topup");
    let uid_sufficient = ctx.db.provision_player_account("acct_sufficient", "pw_suf");
    let uid_zero_target = ctx.db.provision_player_account("acct_zero_target", "pw_zero");

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters[&90_000_001].clone();
        for (cid, uid, name) in [
            (90_088_001, uid_topup, "TopupPilot"),
            (90_088_002, uid_sufficient, "SufficientPilot"),
            (90_088_003, uid_zero_target, "ZeroTargetPilot"),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.name = name.into();
            snap.characters.insert(cid, ch);
        }
        // Seed initial PLEX balances: 150 (< 500 target), 600 (>= 500 target), 50 (with 0 target).
        snap.account_plex_vault.insert(uid_topup, 150);
        snap.account_plex_vault.insert(uid_sufficient, 600);
        snap.account_plex_vault.insert(uid_zero_target, 50);
    });

    let mut cfg = ServerConfig::default();
    cfg.feature_flags = FeatureFlagsConfig {
        starter_plex_balance: 999,
        ..Default::default()
    };
    cfg.player_accounts = vec![
        PlayerAccountConfig {
            username: "acct_topup".into(),
            password_token: "pw_topup".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "Ignored1".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                initial_balance_plex: Some(500),
                ..Default::default()
            }),
        },
        PlayerAccountConfig {
            username: "acct_sufficient".into(),
            password_token: "pw_suf".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "Ignored2".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                initial_balance_plex: Some(500),
                ..Default::default()
            }),
        },
        PlayerAccountConfig {
            username: "acct_zero_target".into(),
            password_token: "pw_zero".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "Ignored3".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                initial_balance_plex: Some(0),
                ..Default::default()
            }),
        },
    ];

    // No new characters are created since all accounts already own characters.
    let created = provision_starter_characters(&ctx, &cfg).await;
    assert!(created.is_empty());

    ctx.db.with_snapshot(|snap| {
        // Topped up from 150 -> 500 (+350 delta).
        assert_eq!(snap.account_plex_vault.get(&uid_topup).copied(), Some(500));
        // Already >= 500 -> untouched at 600.
        assert_eq!(snap.account_plex_vault.get(&uid_sufficient).copied(), Some(600));
        // target_plex == 0 -> untouched at 50.
        assert_eq!(snap.account_plex_vault.get(&uid_zero_target).copied(), Some(50));
    });

    // Running again with a higher target for uid_topup is idempotent because uid_topup is already
    // recorded in `starter_plex_granted_users`.
    cfg.player_accounts[0]
        .starter_character
        .as_mut()
        .unwrap()
        .initial_balance_plex = Some(1_000);
    let created_again = provision_starter_characters(&ctx, &cfg).await;
    assert!(created_again.is_empty());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.account_plex_vault.get(&uid_topup).copied(), Some(500));
    });
}

#[test]
fn grant_starter_plex_in_snapshot_clamps_negative_next_entity_id_and_is_idempotent() {
    let mut snap = eve_db::WorldStateSnapshot::default();
    snap.next_entity_id = -5;

    // Non-positive grants are strict no-ops and do not mark user_id as already granted.
    grant_starter_plex_in_snapshot(&mut snap, 7001, 90_000_001, 0, 1_850_000_000);
    grant_starter_plex_in_snapshot(&mut snap, 7001, 90_000_001, -10, 1_850_000_000);
    assert!(snap.account_plex_vault.get(&7001).is_none());
    assert_eq!(snap.next_entity_id, -5);
    assert_eq!(
        snap.kv(eve_db::Ns::StarterPlexGranted)
            .get::<bool>(7001, ""),
        None
    );

    grant_starter_plex_in_snapshot(&mut snap, 7001, 90_000_001, 250, 1_850_000_000);
    assert_eq!(snap.account_plex_vault.get(&7001).copied(), Some(250));
    assert_eq!(snap.next_entity_id, -4);
    assert_eq!(
        snap.kv(eve_db::Ns::StarterPlexGranted)
            .get::<bool>(7001, ""),
        Some(true)
    );

    let txs_after_first: Vec<_> =
        crate::services::quasar_payment_store::get_plex_transactions(&snap)
            .into_iter()
            .filter(|t| t.user_id == 7001)
            .collect();
    assert_eq!(txs_after_first.len(), 1);
    assert_eq!(txs_after_first[0].id, 1);
    assert_eq!(txs_after_first[0].amount, 250);
    assert_eq!(txs_after_first[0].balance_after, 250);
    assert_eq!(txs_after_first[0].timestamp_unix, 1_850_000_000);
    assert_eq!(txs_after_first[0].description, "Starter PLEX Grant");

    // Second grant for same user_id is ignored (balance, next_entity_id, and tx count unchanged).
    grant_starter_plex_in_snapshot(&mut snap, 7001, 90_000_001, 999, 1_850_000_010);
    assert_eq!(snap.account_plex_vault.get(&7001).copied(), Some(250));
    assert_eq!(snap.next_entity_id, -4);
    let txs_after_second: Vec<_> =
        crate::services::quasar_payment_store::get_plex_transactions(&snap)
            .into_iter()
            .filter(|t| t.user_id == 7001)
            .collect();
    assert_eq!(txs_after_second.len(), 1);

    // Corrupted JSON marker recovers cleanly; minimal positive grant (1) is applied and advances tx_id.
    snap.entity_kv.insert(
        eve_db::EntityKvKey::new(eve_db::Ns::StarterPlexGranted, 7002, ""),
        eve_db::EntityKvRow {
            value_json: "{not-json".to_string(),
            updated_unix: 0,
        },
    );
    snap.next_entity_id = 40;
    grant_starter_plex_in_snapshot(&mut snap, 7002, 999_123_456, 1, 1_850_000_020);
    assert_eq!(snap.account_plex_vault.get(&7002).copied(), Some(1));
    assert_eq!(snap.next_entity_id, 41);
    let txs_7002: Vec<_> = crate::services::quasar_payment_store::get_plex_transactions(&snap)
        .into_iter()
        .filter(|t| t.user_id == 7002)
        .collect();
    assert_eq!(txs_7002.len(), 1);
    assert_eq!(txs_7002[0].id, 41);
    assert_eq!(txs_7002[0].amount, 1);
    assert_eq!(txs_7002[0].balance_after, 1);
    assert_eq!(txs_7002[0].description, "Starter PLEX Grant");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provision_starter_characters_returns_created_ids_and_enforces_edge_boundaries() {
    let ctx = fast_test_context(1_850_000_100);

    let uid_new_custom = ctx.db.provision_player_account("edge_new_1", "pw_1");
    let uid_new_suppressed = ctx.db.provision_player_account("edge_new_2", "pw_2");
    let uid_existing_none = ctx.db.provision_player_account("edge_ex_none", "pw_3");
    let uid_existing_equal = ctx.db.provision_player_account("edge_ex_eq", "pw_4");
    let uid_existing_legacy = ctx.db.provision_player_account("edge_ex_leg", "pw_5");

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters[&90_000_001].clone();
        for (cid, uid, name, legacy_plex) in [
            (90_088_101, uid_existing_none, "ExistingNone", 0),
            (90_088_102, uid_existing_equal, "ExistingEqual", 0),
            (90_088_103, uid_existing_legacy, "ExistingLegacy", 300),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.name = name.into();
            ch.plex_balance = legacy_plex;
            snap.characters.insert(cid, ch);
        }
        snap.account_plex_vault.insert(uid_existing_equal, 500);
        snap.account_plex_vault.insert(uid_existing_legacy, 100);
    });

    let non_jita_station = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id != 30_000_142)
        .expect("non-Jita station")
        .station_id;

    let mut cfg = ServerConfig::default();
    cfg.feature_flags = FeatureFlagsConfig {
        starter_plex_balance: 450,
        ..Default::default()
    };
    cfg.player_accounts = vec![
        // Missing starter_character -> skipped
        PlayerAccountConfig {
            username: "edge_new_1".into(),
            password_token: "pw_1".into(),
            starter_character: None,
        },
        // Whitespace-padded credentials still trim and create char #1 using cfg fallback PLEX + station + ISK
        PlayerAccountConfig {
            username: "  edge_new_1  ".into(),
            password_token: "  pw_1  ".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "EdgeCustomOne".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                solar_system_id: Some(30_000_142),
                station_id: Some(non_jita_station),
                initial_balance_isk: Some(7_654_321),
                initial_balance_plex: None,
            }),
        },
        // Explicit initial_balance_plex = Some(0) suppresses default starter PLEX grant
        PlayerAccountConfig {
            username: "edge_new_2".into(),
            password_token: "pw_2".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "EdgeSuppressedTwo".into(),
                bloodline_id: 3,
                gender: 0,
                ancestry_id: 19,
                school_id: 14,
                solar_system_id: None,
                station_id: None,
                initial_balance_isk: None,
                initial_balance_plex: Some(0),
            }),
        },
        // Existing character with initial_balance_plex = None -> no topup despite 0 balance
        PlayerAccountConfig {
            username: "edge_ex_none".into(),
            password_token: "pw_3".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "IgnoredExNone".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                initial_balance_plex: None,
                ..Default::default()
            }),
        },
        // Existing character with cur == target_plex (500 == 500) -> no topup
        PlayerAccountConfig {
            username: "edge_ex_eq".into(),
            password_token: "pw_4".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "IgnoredExEq".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                initial_balance_plex: Some(500),
                ..Default::default()
            }),
        },
        // Existing character with 100 vault + 300 legacy = 400 folded -> topup delta is 100 to hit 500
        PlayerAccountConfig {
            username: "edge_ex_leg".into(),
            password_token: "pw_5".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "IgnoredExLeg".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                initial_balance_plex: Some(500),
                ..Default::default()
            }),
        },
    ];

    let created = provision_starter_characters(&ctx, &cfg).await;
    let c1 = ctx.db.get_characters_for_user(uid_new_custom)[0].character_id;
    let c2 = ctx.db.get_characters_for_user(uid_new_suppressed)[0].character_id;
    assert_eq!(created, vec![c1, c2]);
    assert!(c1 > 1 && c2 > 1 && c1 != c2);

    ctx.db.with_snapshot(|snap| {
        let ch1 = &snap.characters[&c1];
        assert_eq!(ch1.station_id, Some(non_jita_station));
        assert_eq!(ch1.home_station_id, non_jita_station);
        assert_eq!(ch1.balance, eve_db::IskCents::from_isk(7_654_321));
        assert_eq!(snap.unified_plex_balance_for_user(uid_new_custom), 450);

        assert_eq!(snap.unified_plex_balance_for_user(uid_new_suppressed), 0);
        assert_eq!(snap.unified_plex_balance_for_user(uid_existing_none), 0);
        assert_eq!(snap.unified_plex_balance_for_user(uid_existing_equal), 500);
        assert_eq!(snap.unified_plex_balance_for_user(uid_existing_legacy), 500);
        assert_eq!(snap.characters[&90_088_103].plex_balance, 0);
    });

    let txs_new1: Vec<_> = ctx
        .db
        .with_snapshot(crate::services::quasar_payment_store::get_plex_transactions)
        .into_iter()
        .filter(|t| t.user_id == uid_new_custom)
        .collect();
    assert_eq!(txs_new1.len(), 1);
    assert_eq!(txs_new1[0].amount, 450);
    assert_eq!(txs_new1[0].balance_after, 450);
    assert_eq!(txs_new1[0].timestamp_unix, 1_850_000_100);

    let txs_suppressed: Vec<_> = ctx
        .db
        .with_snapshot(crate::services::quasar_payment_store::get_plex_transactions)
        .into_iter()
        .filter(|t| t.user_id == uid_new_suppressed)
        .collect();
    assert!(txs_suppressed.is_empty());

    let txs_legacy: Vec<_> = ctx
        .db
        .with_snapshot(crate::services::quasar_payment_store::get_plex_transactions)
        .into_iter()
        .filter(|t| t.user_id == uid_existing_legacy)
        .collect();
    assert_eq!(txs_legacy.len(), 1);
    assert_eq!(txs_legacy[0].amount, 100);
    assert_eq!(txs_legacy[0].balance_after, 500);
}
