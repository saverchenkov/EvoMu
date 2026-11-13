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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
                initial_balance_plex: None,
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
                initial_balance_plex: None,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starter_plex_grant_is_one_time_per_account_across_creations_and_deletions() {
    let ctx = fast_test_context();
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .starter_plex_balance = 500;
    let uid = ctx.db.provision_player_account("plex_pilot", "plex_pass");

    let (bloodline_id, ancestry_id, school_id) = ctx
        .sde
        .races
        .values()
        .next()
        .map(|_| (1_u32, 11_u32, 17_u32))
        .unwrap_or((1, 11, 17));

    let mut session = eve_machonet::SessionState::new(901);
    session.user_id = Some(uid);
    session.address = "127.0.0.1:26000".into();

    let non_empty_info = || {
        eve_proto::PyRep::Dict(vec![(
            eve_proto::PyRep::String("head".into()),
            eve_proto::PyRep::Integer(1),
        )])
    };
    let c1 = ctx
        .dispatch_rpc(
            &mut session,
            "charUnboundMgr",
            None,
            "CreateCharacterWithDoll",
            &[
                eve_proto::PyRep::String("FirstStarterPlex".into()),
                eve_proto::PyRep::Integer(i64::from(bloodline_id)),
                eve_proto::PyRep::Integer(1),
                eve_proto::PyRep::Integer(i64::from(ancestry_id)),
                non_empty_info(),
                non_empty_info(),
                eve_proto::PyRep::Integer(i64::from(school_id)),
            ],
            &std::collections::HashMap::new(),
            None,
        )
        .await
        .expect("first char")
        .as_i64()
        .expect("char id");
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(c1)), 500);

    let c2 = ctx
        .dispatch_rpc(
            &mut session,
            "charUnboundMgr",
            None,
            "CreateCharacterWithDoll",
            &[
                eve_proto::PyRep::String("SecondStarterPlex".into()),
                eve_proto::PyRep::Integer(i64::from(bloodline_id)),
                eve_proto::PyRep::Integer(1),
                eve_proto::PyRep::Integer(i64::from(ancestry_id)),
                non_empty_info(),
                non_empty_info(),
                eve_proto::PyRep::Integer(i64::from(school_id)),
            ],
            &std::collections::HashMap::new(),
            None,
        )
        .await
        .expect("second char")
        .as_i64()
        .expect("char id");
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(c2)), 500);

    // Delete second character and recreate: one-time grant marker persists per user_id.
    let _ = ctx
        .dispatch_rpc(
            &mut session,
            "charUnboundMgr",
            None,
            "DeleteCharacter",
            &[eve_proto::PyRep::Integer(c2)],
            &std::collections::HashMap::new(),
            None,
        )
        .await;
    let c3 = ctx
        .dispatch_rpc(
            &mut session,
            "charUnboundMgr",
            None,
            "CreateCharacterWithDoll",
            &[
                eve_proto::PyRep::String("ThirdStarterPlex".into()),
                eve_proto::PyRep::Integer(i64::from(bloodline_id)),
                eve_proto::PyRep::Integer(1),
                eve_proto::PyRep::Integer(i64::from(ancestry_id)),
                non_empty_info(),
                non_empty_info(),
                eve_proto::PyRep::Integer(i64::from(school_id)),
            ],
            &std::collections::HashMap::new(),
            None,
        )
        .await
        .expect("third char")
        .as_i64()
        .expect("char id");
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(c3)), 500);

    let txs: Vec<_> = ctx
        .db
        .with_snapshot(crate::services::quasar_payment_store::get_plex_transactions)
        .into_iter()
        .filter(|t| t.user_id == uid)
        .collect();
    assert_eq!(txs.len(), 1);
    assert!(txs[0].id > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starter_station_transition_resolves_system_when_none_moves_ship_and_skips_validation() {
    let ctx = fast_test_context();

    // Pick a non-Jita SDE station so resolving solar_system_id / constellation_id / region_id is observable.
    let target_station = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id != 30_000_142)
        .expect("non-Jita SDE station")
        .clone();
    let expected_sys = ctx
        .sde
        .solar_systems
        .get(&target_station.solar_system_id)
        .expect("station solar system in SDE");

    let uid_none_sys = ctx
        .db
        .provision_player_account("pilot_station_none_sys", "pw_1");
    let uid_other_sys = ctx
        .db
        .provision_player_account("pilot_station_other_sys", "pw_2");

    let mut cfg = ServerConfig::default();
    cfg.player_accounts = vec![
        PlayerAccountConfig {
            username: "pilot_station_none_sys".into(),
            password_token: "pw_1".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "StationNoneSys".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                solar_system_id: None,
                station_id: Some(target_station.station_id),
                initial_balance_isk: None,
                initial_balance_plex: None,
            }),
        },
        PlayerAccountConfig {
            username: "pilot_station_other_sys".into(),
            password_token: "pw_2".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "StationOtherSys".into(),
                bloodline_id: 3,
                gender: 0,
                ancestry_id: 19,
                school_id: 14,
                solar_system_id: Some(30_000_142),
                station_id: Some(target_station.station_id),
                initial_balance_isk: None,
                initial_balance_plex: None,
            }),
        },
    ];

    let created = provision_starter_characters(&ctx, &cfg).await;
    assert_eq!(created.len(), 2);

    let c_none = created[0];
    let c_other = created[1];
    ctx.db.with_snapshot(|snap| {
        for (cid, label) in [(c_none, "solar_system_id=None"), (c_other, "solar_system_id=Jita")] {
            let ch = &snap.characters[&cid];
            assert_eq!(ch.user_id, if cid == c_none { uid_none_sys } else { uid_other_sys });
            assert_eq!(ch.station_id, Some(target_station.station_id), "{label}");
            assert_eq!(ch.home_station_id, target_station.station_id, "{label}");
            assert_eq!(ch.solar_system_id, target_station.solar_system_id, "{label}");
            assert_eq!(ch.constellation_id, expected_sys.constellation_id, "{label}");
            assert_eq!(ch.region_id, expected_sys.region_id, "{label}");
            assert!(ch.structure_id.is_none(), "{label}");
            let ship_item = &snap.items[&ch.active_ship_id];
            assert_eq!(
                ship_item.location_id, target_station.station_id,
                "move_ship_item=true must place starter ship in station for {label}"
            );
        }
    });

    // Directly verify TransitionLocation::Station validate_destination = false vs true with a synthetic station ID.
    let synthetic_station_id = 69_999_123_i64;
    ctx.db
        .try_with_mut_snapshot(|snap| {
            let err = crate::router::apply_character_transition_in_snap(
                snap,
                &ctx.sde,
                c_none,
                crate::router::TransitionLocation::Station {
                    solar_system_id: Some(30_000_142),
                    station_id: synthetic_station_id,
                },
                None,
                true,
                true,
            )
            .expect_err("validate_destination=true must reject non-SDE station");
            assert!(matches!(err, eve_db::DbError::InvalidDestinationLocation { .. }));

            crate::router::apply_character_transition_in_snap(
                snap,
                &ctx.sde,
                c_none,
                crate::router::TransitionLocation::Station {
                    solar_system_id: Some(30_000_142),
                    station_id: synthetic_station_id,
                },
                None,
                true,
                false,
            )?;
            let ch = &snap.characters[&c_none];
            assert_eq!(ch.station_id, Some(synthetic_station_id));
            assert_eq!(ch.solar_system_id, 30_000_142);
            assert_eq!(snap.items[&ch.active_ship_id].location_id, synthetic_station_id);
            Ok::<(), eve_db::DbError>(())
        })
        .expect("validate_destination=false transition succeeds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn existing_character_tops_up_starter_plex_when_below_target_and_skips_when_at_or_above() {
    let ctx = fast_test_context();
    let uid_low = ctx.db.provision_player_account("existing_low_plex", "pw_low");
    let uid_full = ctx.db.provision_player_account("existing_full_plex", "pw_full");
    let uid_zero_target = ctx.db.provision_player_account("existing_zero_target", "pw_zero");

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().expect("demo char").clone();

        let mut ch_low = base_ch.clone();
        ch_low.character_id = 90_088_001;
        ch_low.user_id = uid_low;
        ch_low.name = "ExistingLowPlex".into();
        snap.characters.insert(ch_low.character_id, ch_low);
        snap.account_plex_vault.insert(uid_low, 150);

        let mut ch_full = base_ch.clone();
        ch_full.character_id = 90_088_002;
        ch_full.user_id = uid_full;
        ch_full.name = "ExistingFullPlex".into();
        snap.characters.insert(ch_full.character_id, ch_full);
        snap.account_plex_vault.insert(uid_full, 500);

        let mut ch_zero = base_ch;
        ch_zero.character_id = 90_088_003;
        ch_zero.user_id = uid_zero_target;
        ch_zero.name = "ExistingZeroTarget".into();
        snap.characters.insert(ch_zero.character_id, ch_zero);
        snap.account_plex_vault.insert(uid_zero_target, 25);
    });

    let mut cfg = ServerConfig::default();
    cfg.player_accounts = vec![
        PlayerAccountConfig {
            username: "existing_low_plex".into(),
            password_token: "pw_low".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "ExistingLowPlex".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                initial_balance_plex: Some(500),
                ..Default::default()
            }),
        },
        PlayerAccountConfig {
            username: "existing_full_plex".into(),
            password_token: "pw_full".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "ExistingFullPlex".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                initial_balance_plex: Some(400),
                ..Default::default()
            }),
        },
        PlayerAccountConfig {
            username: "existing_zero_target".into(),
            password_token: "pw_zero".into(),
            starter_character: Some(StarterCharacterConfig {
                name: "ExistingZeroTarget".into(),
                bloodline_id: 3,
                gender: 1,
                ancestry_id: 19,
                school_id: 14,
                initial_balance_plex: Some(0),
                ..Default::default()
            }),
        },
    ];

    let created = provision_starter_characters(&ctx, &cfg).await;
    assert!(created.is_empty(), "existing accounts must not create duplicate characters");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.unified_plex_balance_for_user(uid_low), 500);
        assert_eq!(snap.unified_plex_balance_for_user(uid_full), 500);
        assert_eq!(snap.unified_plex_balance_for_user(uid_zero_target), 25);
    });

    let txs_low: Vec<_> = ctx
        .db
        .with_snapshot(crate::services::quasar_payment_store::get_plex_transactions)
        .into_iter()
        .filter(|t| t.user_id == uid_low)
        .collect();
    assert_eq!(txs_low.len(), 1);
    assert_eq!(txs_low[0].amount, 350);
    assert_eq!(txs_low[0].balance_after, 500);
    assert_eq!(txs_low[0].description, "Starter PLEX Grant");
}
