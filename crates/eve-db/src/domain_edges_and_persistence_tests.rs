//! Edge-case coverage for accounts, characters, inventory, ledger, types, clock, and WAL recovery.

use super::*;
use crate::accounts::{
    base64url_decode_no_pad, base64url_encode_no_pad, hmac_sha256, sha256_bytes,
};
use crate::clock::{ServerClock, VirtualServerClock};
use crate::corp_roles::{has_role, ROLE_DIRECTOR};
use crate::ledger::{JournalContext, JournalMeta};

pub(super) fn load_test_sde() -> eve_sde::SdeRegistry {
    match std::env::var_os("EVOMU_SDE_PATH") {
        Some(p) => eve_sde::SdeRegistry::open(p).expect("open sde"),
        None => eve_sde::SdeRegistry::with_canonical_seed(),
    }
}

pub(super) fn temp_db_path(label: &str) -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("evomu_edge_{label}_{}_{n}.sqlite", std::process::id()))
}

pub(super) fn make_legacy_sha256_token(username: &str, salt_hex: &str, raw_pw: &str) -> String {
    let mut digest = sha256_bytes(format!("evomu-auth-v1:{salt_hex}:{username}:{raw_pw}").as_bytes());
    for round in 0..256u16 {
        let mut input = Vec::with_capacity(64 + salt_hex.len() + username.len() + raw_pw.len());
        input.extend_from_slice(&digest);
        input.extend_from_slice(&round.to_be_bytes());
        input.extend_from_slice(salt_hex.as_bytes());
        input.push(b':');
        input.extend_from_slice(username.as_bytes());
        input.push(b':');
        input.extend_from_slice(raw_pw.as_bytes());
        digest = sha256_bytes(&input);
    }
    let mut hex = format!("sha256$v1${salt_hex}$");
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

#[test]
fn accounts_legacy_hash_base64_and_online_lock_edges() {
    assert!(base64url_decode_no_pad("ab+d/w").is_ok());
    assert!(base64url_decode_no_pad("invalid!char").is_err());
    assert!(base64url_decode_no_pad("a").is_err());
    assert_eq!(base64url_decode_no_pad("AQI").unwrap(), vec![1, 2]);
    assert_eq!(base64url_decode_no_pad("AQ").unwrap(), vec![1]);

    let long_key = vec![0x42u8; 80];
    assert_eq!(hmac_sha256(&long_key, b"msg").len(), 32);

    std::env::set_var("EVOMU_SSO_JWT_SECRET", "custom_env_sso_jwt_secret_at_least_32_bytes");
    let secret = WorldRepository::generate_random_secret_32();
    assert_eq!(secret.len(), 32);
    std::env::remove_var("EVOMU_SSO_JWT_SECRET");

    let repo = WorldRepository::default();
    assert!(repo.instance_id() > 0);
    let uid = repo.provision_player_account("leg_user", "secret");
    assert_eq!(repo.provision_player_account("leg_user", "secret"), uid);

    let legacy_tok = make_legacy_sha256_token("leg_user", "00112233", "secret");
    assert!(legacy_tok.starts_with("sha256$v1$00112233$"));
    repo.with_mut_snapshot(|snap| {
        snap.accounts.get_mut("leg_user").unwrap().password_token = legacy_tok;
    });
    assert!(repo.authenticate_at("leg_user", "wrong", 1000).is_err());
    assert!(repo.authenticate_at("leg_user", "secret", 1000).is_ok());
    assert!(repo.unsuspend_account("leg_user").is_ok());

    let sde = load_test_sde();
    let cal_school = sde.schools.values().find(|s| s.race_id == 1).unwrap().school_id;
    let ch = repo.create_character(uid, "LegPilot", 1, 1, 11, cal_school, &sde).unwrap();

    assert!(repo.acquire_online_lock(uid, 10, None, false, 1000).unwrap().is_none());
    assert!(repo.acquire_online_lock_with_computer_hash(uid, 11, None, None, false, 1005).is_err());
    let displaced = repo
        .acquire_online_lock_with_computer_hash(
            uid,
            11,
            Some(ch.character_id),
            Some("hw1".into()),
            false,
            1005,
        )
        .unwrap();
    assert!(displaced.is_none());
    assert!(repo.release_online_lock(uid, 11));

    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = base64url_encode_no_pad(&sha256_bytes(verifier.as_bytes()));
    let code = repo
        .authorize_pkce(
            "leg_user",
            "secret",
            ch.character_id,
            "evomu-client",
            "eveauth://callback",
            vec!["publicData".into()],
            &challenge,
            "S256",
            2000,
        )
        .unwrap();
    let sso = repo
        .exchange_pkce_code(&code, "evomu-client", verifier, 2001)
        .unwrap();
    assert!(repo.verify_sso_access_token("bad.token", 2001).is_err());
    assert!(repo.verify_sso_access_token("a.b.", 2001).is_err());
    let bad_hdr = format!("{}.e30.c2ln", base64url_encode_no_pad(b"{\"alg\":\"none\"}"));
    assert!(repo.verify_sso_access_token(&bad_hdr, 2001).is_err());
    assert!(repo.verify_sso_access_token(&sso.access_token, 500).is_err());
    assert!(repo.verify_sso_access_token(&sso.access_token, 9_999_999).is_err());

    assert!(repo.revoke_sso_refresh_token(&sso.refresh_token));
    assert!(repo.verify_sso_access_token(&sso.access_token, 2005).is_err());

    let code2 = repo
        .authorize_pkce(
            "leg_user",
            "secret",
            ch.character_id,
            "evomu-client",
            "eveauth://callback",
            vec!["publicData".into()],
            &challenge,
            "S256",
            2500,
        )
        .unwrap();
    let sso2 = repo
        .exchange_pkce_code(&code2, "evomu-client", verifier, 2501)
        .unwrap();
    let _ = repo.kick_user(uid, "kicked_later", 3000);
    assert!(repo.verify_sso_access_token(&sso2.access_token, 3001).is_err());

    repo.ban_account("leg_user", 5000, "test_ban").unwrap();
    assert_eq!(
        repo.check_session_revoked(uid, 12, 3005).map(|(k, _)| k),
        Some("UserBanned".to_string())
    );
}

#[test]
fn characters_creation_respawn_and_inventory_transfers() {
    let sde = load_test_sde();
    let repo = WorldRepository::new_empty();
    let uid = repo.provision_player_account("pilot_acc", "pw");
    let school = sde.schools.values().find(|s| s.race_id == 1).unwrap().school_id;

    assert!(matches!(repo.select_character_for_user(uid, 99999), Err(DbError::CharacterNotFound(99999))));
    assert!(repo.create_character(uid, "  ", 1, 1, 11, school, &sde).is_err());
    assert!(repo.create_character(uid, "BadRace", 9999, 1, 11, school, &sde).is_err());
    let valid_bloodline = sde.bloodlines.values().find(|b| b.race_id == 1).unwrap().bloodline_id;
    let valid_anc = sde
        .ancestries
        .values()
        .find(|a| a.bloodline_id == valid_bloodline)
        .unwrap()
        .ancestry_id;
    assert!(repo.create_character(uid, "BadBlood", 1, 9999, valid_anc, school, &sde).is_err());
    assert!(repo.create_character(uid, "BadAnc", 1, valid_bloodline, 9999, school, &sde).is_err());
    assert!(repo.create_character(uid, "BadSchool", 1, valid_bloodline, valid_anc, 9999, &sde).is_err());

    let jita_sys = 30_000_142u32;
    let ch = repo
        .create_character_with_system(uid, "JitaRook", 1, valid_bloodline, valid_anc, school, &sde, Some(jita_sys))
        .unwrap();
    assert_eq!(ch.solar_system_id, jita_sys);
    assert!(repo.create_character_with_system(uid, "JitaRook", 1, valid_bloodline, valid_anc, school, &sde, None).is_err());
    assert_eq!(repo.get_characters_for_user(uid).len(), 1);

    // Respawn in operational cored Upwell structure with online Clone Bay
    let struct_id = 1_030_000_000_001i64;
    repo.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                solar_system_id: jita_sys,
                owner_corp_id: ch.corporation_id,
                type_id: 35832,
                name: "Clone Fort".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..UpwellStructureRecord::default()
            },
        );
        snap.items.insert(
            88001,
            InventoryItemRecord {
                item_id: 88001,
                owner_id: ch.corporation_id,
                location_id: struct_id,
                type_id: 35894,
                quantity: 1,
                flag: InventoryFlag::ServiceSlot0,
                singleton: true,
                ..InventoryItemRecord::default()
            },
        );
        snap.module_online_states.insert(88001, true);
        snap.characters.get_mut(&ch.character_id).unwrap().home_station_id = struct_id;
    });
    let (resp1, pod1) = repo.respawn_character_at_home_station(ch.character_id, ch.active_ship_id, &sde).unwrap();
    assert_eq!(resp1.structure_id, Some(struct_id));
    assert_eq!(pod1.location_id, struct_id);

    // Destroy structure -> fallback to school station & verify fuel-bay move + add_or_stack_item
    repo.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().state = "Destroyed".into();
        snap.characters.get_mut(&ch.character_id).unwrap().home_station_id = struct_id;
    });
    let (resp2, _pod2) = repo.respawn_character_at_home_station(ch.character_id, pod1.item_id, &sde).unwrap();
    let station_id = resp2.station_id.unwrap();
    assert_eq!(resp2.home_station_id, station_id);

    let _ = repo
        .add_or_stack_item(ch.character_id, station_id, InventoryFlag::Hangar, 4247, 5)
        .unwrap();
    let stacked = repo
        .add_or_stack_item(ch.character_id, station_id, InventoryFlag::Hangar, 4247, 5)
        .unwrap();
    assert_eq!(stacked.quantity, 10);
    let added = repo
        .add_or_stack_item(ch.character_id, station_id, InventoryFlag::Hangar, 34, 100)
        .unwrap();
    assert!(repo.get_item(added.item_id).is_some());
    assert!(!repo.get_items_for_character(ch.character_id).is_empty());

    // Live structure fuel bay move in same solar system
    let fuel_struct = 1_030_000_000_002i64;
    repo.with_mut_snapshot(|snap| {
        snap.structures.insert(
            fuel_struct,
            UpwellStructureRecord {
                structure_id: fuel_struct,
                solar_system_id: resp2.solar_system_id,
                owner_corp_id: ch.corporation_id,
                type_id: 35832,
                name: "Fuel Struct".into(),
                state: "ShieldVulnerable".into(),
                ..UpwellStructureRecord::default()
            },
        );
    });
    let moved = repo
        .move_item_transactional(
            ch.character_id,
            added.item_id,
            fuel_struct,
            InventoryFlag::StructureFuel,
            None,
            Some("fuel-key-1"),
        )
        .unwrap();
    assert_eq!(moved.location_id, fuel_struct);
}

#[test]
fn wal_recovery_fault_modes_and_domain_serializers() {
    let clk = VirtualServerClock::default();
    assert!(clk.now_subsec_nanos() < 1_000_000_000);
    assert!(clk.tick_wall_second() >= 1);

    assert_eq!(NeuralAttributes::default().get_attribute_by_id(165), 20.0);
    assert_eq!(NeuralAttributes::default().get_attribute_by_id(166), 20.0);
    assert_eq!(NeuralAttributes::default().get_attribute_by_id(167), 20.0);
    assert_eq!(NeuralAttributes::default().get_attribute_by_id(999), 20.0);
    assert!(has_role(ROLE_DIRECTOR, ROLE_DIRECTOR));

    let meta = JournalMeta::new(10, 1, 2, 100)
        .with_context(JournalContext::Character, 1)
        .with_account_key(1002);
    assert_eq!(meta.account_key, 1002);

    let json_path = temp_db_path("wal_recover_test").with_extension("json");
    let wal_path = WorldRepository::wal_path_for(&json_path);
    let _ = std::fs::remove_file(&json_path);
    let _ = std::fs::remove_file(&wal_path);

    let repo = WorldRepository::new_empty_with_snapshot_path(Some(json_path.clone())).unwrap();
    repo.spawn_asteroid(AsteroidRecord {
        asteroid_id: 77001,
        solar_system_id: 30000142,
        belt_id: 1,
        type_id: 1230,
        quantity: 500,
        initial_quantity: 500,
        position: (1.0, 2.0, 3.0),
        radius: 100.0,
    });
    repo.wal_fault_mode.store(1, std::sync::atomic::Ordering::SeqCst);
    assert!(repo
        .try_with_mut_snapshot(|snap| {
            snap.next_entity_id += 10;
            Ok::<(), DbError>(())
        })
        .is_err());

    repo.wal_fault_mode.store(2, std::sync::atomic::Ordering::SeqCst);
    assert!(repo
        .try_with_mut_snapshot(|snap| {
            snap.next_entity_id += 10;
            Ok::<(), DbError>(())
        })
        .is_err());

    repo.with_mut_snapshot(|snap| {
        snap.wal_seq = 12;
        snap.account_plex_vault.insert(1, 500);
    });
    repo.with_mut_snapshot(|snap| {
        snap.account_plex_vault.remove(&1);
    });
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new().append(true).open(&wal_path).unwrap();
    f.write_all(b"{\"seq\":999,\"truncated_tail\":").unwrap();
    drop(repo);

    let repo2 = WorldRepository::new_empty_with_snapshot_path(Some(json_path.clone())).unwrap();
    repo2.with_snapshot(|snap| {
        assert!(snap.asteroids.contains_key(&77001));
        assert!(snap.account_plex_vault.is_empty());
    });
    let _ = std::fs::remove_file(&json_path);
    let _ = std::fs::remove_file(&wal_path);
}
