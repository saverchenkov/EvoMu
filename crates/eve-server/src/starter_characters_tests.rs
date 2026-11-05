//! Unit tests for `starter_characters` startup character provisioning.

#![forbid(unsafe_code)]

use super::*;
use crate::config::{PlayerAccountConfig, ServerConfig, StarterCharacterConfig};
use crate::router::ServerContext;
use eve_db::{IskCents, WorldRepository};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        Arc::new(SdeRegistry::open(SdeRegistry::default_db_path()).expect("open default SDE"))
    }))
}

fn fast_test_context() -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(shared_sde(), db)
}

#[tokio::test]
async fn provision_starter_characters_creates_overrides_and_skips_invalid_or_existing() {
    let ctx = fast_test_context();

    // Provision three accounts:
    // 1) "pilot_custom": no characters yet, with custom system/station/balance overrides.
    // 2) "pilot_default": no characters yet, default school station & balance, plus invalid char config.
    // 3) "pilot_existing": already owns a character, must be skipped.
    let uid_custom = ctx.db.provision_player_account("pilot_custom", "pass_custom");
    let uid_default = ctx.db.provision_player_account("pilot_default", "pass_default");
    let uid_existing = ctx.db.provision_player_account("pilot_existing", "pass_existing");

    ctx.db.with_mut_snapshot(|snap| {
        let mut existing_ch = snap.characters.values().next().expect("demo char").clone();
        existing_ch.character_id = 90_055_001;
        existing_ch.user_id = uid_existing;
        existing_ch.name = "AlreadyExists".to_string();
        snap.characters.insert(90_055_001, existing_ch);
    });

    let mut cfg = ServerConfig::default();
    cfg.player_accounts = vec![
        // No starter_character configured -> skipped
        PlayerAccountConfig {
            username: "pilot_custom".into(),
            password_token: "pass_custom".into(),
            starter_character: None,
        },
        // Wrong password -> authenticate fails -> skipped
        PlayerAccountConfig {
            username: "pilot_custom".into(),
            password_token: "wrong_pass".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "ShouldNotCreate".into(),
                bloodline_id: 1,
                gender: 1,
                ancestry_id: 1,
                school_id: 11,
                ..Default::default()
            }),
        },
        // Already owns a character -> skipped
        PlayerAccountConfig {
            username: "pilot_existing".into(),
            password_token: "pass_existing".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "SecondCharIgnored".into(),
                bloodline_id: 1,
                gender: 1,
                ancestry_id: 1,
                school_id: 11,
                ..Default::default()
            }),
        },
        // Valid starter character with explicit station, solar system, and initial balance overrides
        PlayerAccountConfig {
            username: "pilot_custom".into(),
            password_token: "pass_custom".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "CustomStarter".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                solar_system_id: Some(30_000_142),
                station_id: Some(60_008_494),
                initial_balance_isk: Some(12_345_678),
            }),
        },
        // Valid starter character with default station & balance (None overrides)
        PlayerAccountConfig {
            username: "pilot_default".into(),
            password_token: "pass_default".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "DefaultStarter".into(),
                bloodline_id: 3,
                gender: 0,
                ancestry_id: 19,
                school_id: 14,
                solar_system_id: None,
                station_id: None,
                initial_balance_isk: None,
            }),
        },
    ];

    let created = provision_starter_characters(&ctx, &cfg).await;
    assert_eq!(created.len(), 2);

    let custom_chars = ctx.db.get_characters_for_user(uid_custom);
    assert_eq!(custom_chars.len(), 1);
    let custom_ch = &custom_chars[0];
    assert_eq!(custom_ch.character_id, created[0]);
    assert_eq!(custom_ch.name, "CustomStarter");
    assert_eq!(custom_ch.station_id, Some(60_008_494));
    assert_eq!(custom_ch.home_station_id, 60_008_494);
    assert_eq!(custom_ch.balance, IskCents::from_isk(12_345_678));

    let ship_loc = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&custom_ch.active_ship_id)
            .map(|it| it.location_id)
    });
    assert_eq!(ship_loc, Some(60_008_494));

    let default_chars = ctx.db.get_characters_for_user(uid_default);
    assert_eq!(default_chars.len(), 1);
    assert_eq!(default_chars[0].character_id, created[1]);
    assert_eq!(default_chars[0].name, "DefaultStarter");
    assert_ne!(default_chars[0].balance, IskCents::from_isk(12_345_678));

    // Re-running is idempotent: both accounts now own characters, so 0 new characters are created.
    assert!(provision_starter_characters(&ctx, &cfg).await.is_empty());

    // Error path when CreateCharacterWithDoll rejects invalid bloodline/ancestry/school on a fresh account.
    let uid_bad = ctx.db.provision_player_account("pilot_bad", "pass_bad");
    let mut bad_cfg = ServerConfig::default();
    bad_cfg.player_accounts.push(PlayerAccountConfig {
        username: "pilot_bad".into(),
        password_token: "pass_bad".into(),
        starter_character: Some(StarterCharacterConfig {
            name: "BadSchoolStarter".into(),
            bloodline_id: 999_999,
            gender: 1,
            ancestry_id: 999_999,
            school_id: 999_999,
            ..Default::default()
        }),
    });
    let created_bad = provision_starter_characters(&ctx, &bad_cfg).await;
    assert!(created_bad.is_empty());
    assert!(ctx.db.get_characters_for_user(uid_bad).is_empty());
}
