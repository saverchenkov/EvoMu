//! Unit tests for `accounts`, `clock`, `types`, and `durable_domain_types`.

use super::*;
use crate::accounts::{service_roles, sha256_bytes, base64url_encode_no_pad};
use crate::clock::{ServerClock, VirtualServerClock};
use crate::durable_domain_types::PersistedCrimeWatchState;
use std::collections::HashMap;

#[test]
fn clock_isk_inventory_flag_corp_wallet_and_crimewatch_types() {
    // VirtualServerClock with fixed base: deterministic monotonic progression, accel multiplier, pause, pause_on_tick.
    let fixed_clk = VirtualServerClock::with_fixed_base(1_700_000_000);
    assert_eq!(fixed_clk.now_unix_secs(), 1_700_000_000);
    assert_eq!(fixed_clk.now_subsec_nanos(), 0);
    assert_eq!(
        fixed_clk.now_blueos_filetime_ticks(),
        (1_700_000_000_i64 + 11_644_473_600_i64) * 10_000_000
    );
    fixed_clk.advance_secs(42);
    assert_eq!(fixed_clk.now_unix_secs(), 1_700_000_042);

    fixed_clk.set_acceleration_multiplier(3);
    assert_eq!(fixed_clk.acceleration_multiplier(), 3);
    fixed_clk.set_pause_on_tick(true);
    assert!(fixed_clk.pause_on_tick());
    assert_eq!(fixed_clk.step_tick(2), 6);
    assert_eq!(fixed_clk.now_unix_secs(), 1_700_000_048);
    assert!(fixed_clk.is_paused());
    assert_eq!(fixed_clk.step_tick(5), 0);
    assert_eq!(fixed_clk.tick_wall_second(), 0);
    fixed_clk.set_paused(false);
    fixed_clk.set_pause_on_tick(false);
    assert_eq!(fixed_clk.tick_wall_second(), 3);
    assert_eq!(fixed_clk.now_unix_secs(), 1_700_000_051);

    // Wall-clock mode pause/resume & tick_wall_second.
    let wall_clk = VirtualServerClock::new();
    let _ = wall_clk.now_subsec_nanos();
    wall_clk.set_paused(true);
    let paused_t = wall_clk.now_unix_secs();
    assert_eq!(wall_clk.now_subsec_nanos(), 0);
    assert_eq!(wall_clk.tick_wall_second(), 0);
    wall_clk.set_paused(false);
    wall_clk.set_acceleration_multiplier(4);
    assert_eq!(wall_clk.tick_wall_second(), 4);
    assert!(wall_clk.now_unix_secs() >= paused_t + 3);

    // IskCents fixed-point arithmetic & saturating/checked bounds.
    let c = IskCents::from_isk(125);
    assert_eq!(c.raw_cents(), 12_500);
    assert_eq!(IskCents::from_raw_cents(12_500), c);
    assert!((c.to_wire_f64() - 125.0).abs() < 1e-9);
    assert!((c.to_f64_isk() - 125.0).abs() < 1e-9);
    assert_eq!(IskCents::from_f64_isk(f64::NAN), IskCents::ZERO);
    assert_eq!(IskCents::from_f64_isk(f64::INFINITY), IskCents::ZERO);
    assert_eq!(IskCents::from_f64_isk(f64::NEG_INFINITY), IskCents::ZERO);
    assert_eq!(IskCents::from_f64_isk(1e20), IskCents(i64::MAX / 2));
    assert_eq!(IskCents::from_f64_isk(-1e20), IskCents(i64::MIN / 2));
    assert_eq!(c.checked_add(IskCents(50)), Some(IskCents(12_550)));
    assert_eq!(IskCents(-1).checked_add(IskCents(1)), None);
    assert_eq!(c.checked_sub(IskCents(2_500)), Some(IskCents(10_000)));
    assert_eq!(c.checked_sub(IskCents(20_000)), None);
    assert_eq!(c.checked_mul_qty(4), Some(IskCents(50_000)));
    assert_eq!(c.checked_mul_qty(-2), None);

    // InventoryFlag roundtrips across all codes + unknown fallback.
    let known_flags = [
        InventoryFlag::Wardrobe,
        InventoryFlag::Hangar,
        InventoryFlag::Cargo,
        InventoryFlag::Skill,
        InventoryFlag::LoSlot0,
        InventoryFlag::MedSlot0,
        InventoryFlag::HiSlot0,
        InventoryFlag::Capsule,
        InventoryFlag::Pilot,
        InventoryFlag::CorpDeliveries,
        InventoryFlag::Locked,
        InventoryFlag::Unlocked,
        InventoryFlag::Impounded,
        InventoryFlag::DroneBay,
        InventoryFlag::Booster,
        InventoryFlag::Implant,
        InventoryFlag::ShipHangar,
        InventoryFlag::RigSlot0,
        InventoryFlag::CorpSAG1,
        InventoryFlag::CorpSAG7,
        InventoryFlag::SubSystemSlot0,
        InventoryFlag::SpecializedFuelBay,
        InventoryFlag::SpecializedOreHold,
        InventoryFlag::SpecializedGasHold,
        InventoryFlag::SpecializedMineralHold,
        InventoryFlag::SpecializedSalvageHold,
        InventoryFlag::SpecializedShipHold,
        InventoryFlag::SpecializedAmmoHold,
        InventoryFlag::SpecializedCommandCenterHold,
        InventoryFlag::SpecializedPlanetaryCommoditiesHold,
        InventoryFlag::SpecializedMaterialBay,
        InventoryFlag::FleetHangar,
        InventoryFlag::FighterBay,
        InventoryFlag::ServiceSlot0,
        InventoryFlag::ServiceSlot7,
        InventoryFlag::StructureFuel,
        InventoryFlag::Deliveries,
        InventoryFlag::FrigateEscapeBay,
        InventoryFlag::SpecializedMobileDepotHold,
        InventoryFlag::StructureDeedBay,
        InventoryFlag::SpecializedIceHold,
        InventoryFlag::SpecializedAsteroidHold,
        InventoryFlag::SpecializedInfrastructureHold,
    ];
    for flag in known_flags {
        assert_eq!(InventoryFlag::from_u16(flag as u16), flag);
    }
    assert_eq!(InventoryFlag::from_u16(9_999), InventoryFlag::None);
    assert!(InventoryFlag::ServiceSlot0.is_service_slot());
    assert!(InventoryFlag::ServiceSlot7.is_service_slot());
    assert!(!InventoryFlag::Hangar.is_service_slot());

    // CorporationRecord wallet divisions helpers.
    let mut corp = CorporationRecord {
        corporation_id: 98_000_001,
        name: "Test Corp".into(),
        ticker: "TSTC".into(),
        ceo_id: 90_000_001,
        tax_rate: 0.05,
        master_wallet: IskCents::from_isk(1_000_000),
        hq_station_id: Some(60_003_760),
        ..CorporationRecord::default()
    };
    assert_eq!(corp.division_balance(1), IskCents::from_isk(1_000_000));
    assert_eq!(corp.division_balance(3), IskCents::ZERO);
    corp.set_division_balance(2, IskCents::from_isk(250_000));
    assert_eq!(corp.wallet_divisions.len(), 7);
    assert_eq!(corp.division_balance(2), IskCents::from_isk(250_000));
    corp.set_division_balance(1, IskCents::from_isk(2_000_000));
    assert_eq!(corp.master_wallet, IskCents::from_isk(2_000_000));

    // PersistedCrimeWatchState boundary checks (exact equality `now == until` is expired).
    let cw = PersistedCrimeWatchState {
        character_id: 90_000_001,
        weapon_timer_until_unix: 100,
        pvp_timer_until_unix: 200,
        suspect_flag_until_unix: 300,
        criminal_flag_until_unix: 400,
        pending_concord_arrival: None,
    };
    assert!(cw.is_weapon_timer_active(99));
    assert!(!cw.is_weapon_timer_active(100));
    assert!(cw.is_pvp_active(199));
    assert!(!cw.is_pvp_active(200));
    assert!(cw.is_suspect(299));
    assert!(!cw.is_suspect(300));
    assert!(cw.is_criminal(399));
    assert!(!cw.is_criminal(400));

    // FromWalError conversions.
    assert_eq!(
        DbError::from_wal_error("disk full".into()),
        DbError::PersistenceFailure("disk full".into())
    );
    let rpc_err = eve_proto::BridgeRpcError::from_wal_error("io err".into());
    assert_eq!(rpc_err.code, "PersistenceFailure");
    assert_eq!(String::from_wal_error("raw".into()), "raw");
    assert_eq!(<(u32, String)>::from_wal_error("e".into()), (500, "e".into()));

    // tuple_map_serde deterministic roundtrip.
    let war_state = crate::durable_domain_types::PersistedWarState {
        next_negotiation_id: 7,
        mutual_invites: HashMap::from([((98_000_005, 1), 1500), ((98_000_006, 2), 1600)]),
        blocked_mutual_invites: HashMap::from([(98_000_009, true)]),
        ..crate::durable_domain_types::PersistedWarState::default()
    };
    let war_json = serde_json::to_string(&war_state).unwrap();
    let war_decoded: crate::durable_domain_types::PersistedWarState =
        serde_json::from_str(&war_json).unwrap();
    assert_eq!(war_decoded, war_state);
}

fn load_test_sde() -> eve_sde::SdeRegistry {
    match std::env::var_os("EVOMU_SDE_PATH") {
        Some(p) => eve_sde::SdeRegistry::open(p).expect("open sde"),
        None => eve_sde::SdeRegistry::with_canonical_seed(),
    }
}

#[test]
fn account_bans_suspensions_online_sessions_and_oauth2_pkce_sso() {
    let repo = WorldRepository::new_empty();
    let alice_uid = repo.provision_player_account("alice", "alice-pw");
    assert_eq!(repo.provision_player_account("alice", "alice-pw"), alice_uid);

    repo.provision_gm_account("gm_bob", "gm-pw", 90_000_010, "GM Bob");
    let gm_uid = repo.find_user_id_by_username("gm_bob").expect("gm_bob uid");
    let gm_acc = repo.authenticate("gm_bob", "gm-pw").expect("auth gm_bob");
    assert_ne!(gm_acc.role & service_roles::ROLE_ADMIN, 0);
    assert_eq!(repo.find_user_id_by_username("alice"), Some(alice_uid));

    // Create a character owned by alice for SSO character selection (also verify school/race mismatch error).
    let sde = load_test_sde();
    let caldari_school = sde
        .schools
        .values()
        .find(|s| s.race_id == 1)
        .map(|s| s.school_id)
        .unwrap_or(11);
    assert!(matches!(
        repo.create_character(alice_uid, "Bad School", 1, 1, 11, 14, &sde),
        Err(DbError::ValidationError(_))
    ));
    let ch = repo
        .create_character(alice_uid, "Alice Main", 1, 1, 11, caldari_school, &sde)
        .expect("create character");

    // Admin accounts cannot be banned or suspended or kicked.
    assert!(repo.suspend_account("gm_bob", 2_000_000_000, "test").is_err());
    assert!(repo.ban_account_by_id_or_username("gm_bob", 2_000_000_000, "test").is_err());
    assert!(repo.kick_user(gm_uid, "nope", 1_700_000_000).is_none());

    // Role update & suspend/unsuspend by both username and numeric ID string.
    repo.set_account_role("alice", service_roles::ROLE_PLAYER_DEFAULT | service_roles::ROLE_GMS)
        .expect("set role");
    assert_eq!(
        repo.suspend_account("alice", 1_700_001_000, "SUSPENDED:timeout")
            .unwrap(),
        alice_uid
    );
    assert!(matches!(
        repo.authenticate_at("alice", "alice-pw", 1_700_000_500),
        Err(DbError::AccountSuspended { .. })
    ));
    assert_eq!(
        repo.unsuspend_account_by_id_or_username(&alice_uid.to_string())
            .unwrap()
            .0,
        alice_uid
    );
    repo.ban_account_by_id_or_username(&alice_uid.to_string(), 1_700_002_000, "ban test")
        .unwrap();
    assert!(!repo.clear_expired_ban_by_id(alice_uid, 1_700_001_999));
    assert!(repo.clear_expired_ban_by_id(alice_uid, 1_700_002_000));
    repo.unban_account_by_id_or_username("alice").unwrap();
    assert!(repo.authenticate_at("alice", "alice-pw", 1_700_000_500).is_ok());

    // Online locks, computer hash conflict check, kick & session revocation.
    assert!(repo
        .acquire_online_lock_with_computer_hash(
            alice_uid,
            501,
            Some(ch.character_id),
            Some("mach-xyz".into()),
            false,
            1_700_000_500
        )
        .is_ok());
    assert_eq!(repo.online_session_count(), 1);
    assert_eq!(repo.online_sessions_snapshot(), vec![(alice_uid, 501)]);
    assert_eq!(repo.get_online_session(alice_uid).unwrap().session_id, 501);
    assert!(repo.check_session_revoked(alice_uid, 501, 1_700_000_505).is_none());
    // Different session_id sees MultiLoginBlocked.
    let (code, _) = repo.check_session_revoked(alice_uid, 999, 1_700_000_505).unwrap();
    assert_eq!(code, "MultiLoginBlocked");

    // Alpha client computer hash conflict when two different alpha accounts share a machine hash or empty hash.
    assert!(repo.has_alpha_computer_hash_conflict(alice_uid, 502, ""));
    assert!(!repo.has_alpha_computer_hash_conflict(gm_uid, 502, ""));
    assert!(!repo.has_alpha_computer_hash_conflict(gm_uid, 502, "other-mach"));
    assert!(repo.has_alpha_computer_hash_conflict(gm_uid, 502, "mach-xyz"));
    // Wrong session_id cannot release lock; session_id == 0 force-releases.
    assert!(!repo.release_online_lock(alice_uid, 999));
    assert!(repo.release_online_lock(alice_uid, 0));
    assert_eq!(repo.online_session_count(), 0);

    // JWT signing secret validation and OAuth2 S256 PKCE full flow.
    assert!(repo.set_jwt_signing_secret(&[1, 2, 3]).is_err());
    repo.set_jwt_signing_secret(&[0x5Au8; 32]).expect("set valid 32B secret");
    assert_eq!(repo.jwt_key_id().len(), 16);

    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = base64url_encode_no_pad(&sha256_bytes(verifier.as_bytes()));

    // Non-S256 PKCE method rejected.
    assert!(repo
        .authorize_pkce(
            "alice",
            "alice-pw",
            ch.character_id,
            "evomu-client",
            "eveauth://callback",
            vec!["publicData".into()],
            &challenge,
            "plain",
            1_700_000_500
        )
        .is_err());

    let code_bad = repo
        .authorize_pkce(
            "alice",
            "alice-pw",
            ch.character_id,
            "evomu-client",
            "eveauth://callback",
            vec!["esi-wallet.read_character_wallet.v1".into()],
            &challenge,
            "S256",
            1_700_000_500,
        )
        .expect("authorize_pkce bad");
    // Wrong verifier consumes code_bad and fails exchange.
    assert!(repo
        .exchange_pkce_code(&code_bad, "evomu-client", "wrong-verifier", 1_700_000_510)
        .is_err());

    let code = repo
        .authorize_pkce(
            "alice",
            "alice-pw",
            ch.character_id,
            "evomu-client",
            "eveauth://callback",
            vec!["esi-wallet.read_character_wallet.v1".into()],
            &challenge,
            "S256",
            1_700_000_500,
        )
        .expect("authorize_pkce");

    let tok1 = repo
        .exchange_pkce_code(&code, "evomu-client", verifier, 1_700_000_510)
        .expect("exchange_pkce_code");
    let claims1 = repo
        .verify_sso_access_token(&tok1.access_token, 1_700_000_515)
        .expect("verify_sso_access_token");
    assert_eq!(claims1.user_id, alice_uid);
    assert_eq!(claims1.character_id, ch.character_id);

    // Expired token (`now > exp`) fails verification.
    assert!(repo
        .verify_sso_access_token(&tok1.access_token, 1_700_000_510 + tok1.expires_in + 1)
        .is_err());

    // Refresh token rotation & revocation.
    let tok2 = repo
        .refresh_sso_token(&tok1.refresh_token, "evomu-client", 1_700_000_600)
        .expect("refresh_sso_token");
    assert!(repo.verify_sso_access_token(&tok2.access_token, 1_700_000_605).is_ok());

    // Kick user revokes all active SSO tokens & sessions.
    assert!(repo
        .acquire_online_lock(alice_uid, 601, Some(ch.character_id), false, 1_700_000_605)
        .is_ok());
    let kicked = repo.kick_user(alice_uid, "admin kick", 1_700_000_610);
    assert!(kicked.is_some());
    assert_eq!(repo.kicked_reason(alice_uid).as_deref(), Some("admin kick"));
    assert!(repo.verify_sso_access_token(&tok2.access_token, 1_700_000_615).is_err());
    assert!(!repo.revoke_sso_refresh_token("nonexistent-token"));
    repo.clear_revocation_on_fresh_login(alice_uid, 701);
    assert!(repo.kicked_reason(alice_uid).is_none());
}
