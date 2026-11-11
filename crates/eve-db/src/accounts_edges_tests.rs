use super::*;
use crate::types::{DbError, SsoRefreshTokenRecord, SsoTokenClaims};
use std::net::{IpAddr, Ipv4Addr};

fn sign_test_jwt(secret: &[u8; 32], claims: &SsoTokenClaims) -> String {
    let header_b64 = base64url_encode_no_pad(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload_b64 =
        base64url_encode_no_pad(&serde_json::to_vec(claims).expect("serialize claims"));
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    format!("{signing_input}.{}", base64url_encode_no_pad(&sig))
}

#[test]
fn crypto_and_base64url_edge_cases() {
    // Empty, short, 55/56/64/65/128-byte SHA-256 block padding boundaries.
    for len in [0usize, 1, 55, 56, 63, 64, 65, 127, 128, 250] {
        let data = vec![0xA5u8; len];
        let d1 = sha256_bytes(&data);
        let d2 = sha256_bytes(&data);
        assert_eq!(d1, d2);
        let mut flipped = data.clone();
        if let Some(last) = flipped.last_mut() {
            *last ^= 0x01;
            assert_ne!(d1, sha256_bytes(&flipped));
        }
    }

    // Known SHA-256 empty vector.
    assert_eq!(
        sha256_bytes(b""),
        [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55
        ]
    );

    // HMAC-SHA256 with >64-byte key hashes the key first and equals HMAC of sha256(key).
    let long_key = vec![0x5Au8; 100];
    let hashed_key = sha256_bytes(&long_key);
    assert_eq!(
        hmac_sha256(&long_key, b"payload"),
        hmac_sha256(&hashed_key, b"payload")
    );
    assert_ne!(
        hmac_sha256(&[1u8; 64], b"payload"),
        hmac_sha256(&[2u8; 64], b"payload")
    );

    assert!(constant_time_eq(b"same", b"same"));
    assert!(!constant_time_eq(b"same", b"samf"));
    assert!(!constant_time_eq(b"same", b"shorter"));

    // base64url roundtrip for lengths 0..=8 and trailing '=' stripping.
    for len in 0..=8 {
        let bytes: Vec<u8> = (0..len).map(|i| (i * 37) as u8).collect();
        let enc = base64url_encode_no_pad(&bytes);
        assert_eq!(base64url_decode_no_pad(&enc).expect("decode"), bytes);
        let padded = format!("{enc}==");
        assert_eq!(base64url_decode_no_pad(&padded).expect("decode padded"), bytes);
    }
    assert!(base64url_decode_no_pad("a").is_err());
    assert!(base64url_decode_no_pad("ab+d").is_ok());
    assert!(base64url_decode_no_pad("ab/d").is_ok());
    assert!(base64url_decode_no_pad("ab!d").is_err());
}

#[test]
fn provisioning_password_update_and_gm_upgrade_edges() {
    let repo = WorldRepository::new_empty();
    let uid = repo.provision_player_account("pilot1", "pass-one");
    assert!(uid > 0);
    // Re-provisioning with identical password returns same user_id.
    assert_eq!(repo.provision_player_account("pilot1", "pass-one"), uid);
    // Re-provisioning with different password updates password token.
    assert_eq!(repo.provision_player_account("pilot1", "pass-two"), uid);
    assert!(repo.authenticate_at("pilot1", "pass-one", 1_700_000_000).is_err());
    assert!(repo.authenticate_at("pilot1", "pass-two", 1_700_000_000).is_ok());

    // Provisioning GM account upgrades existing player account and preserves role bits.
    repo.provision_gm_account("pilot1", "gm-pass", 55_000_001, "GM Pilot");
    let acc = repo
        .authenticate_at("pilot1", "gm-pass", 1_700_000_000)
        .expect("auth upgraded gm");
    assert_eq!(acc.role & service_roles::ROLE_GM_MASK, service_roles::ROLE_GM_MASK);
    assert_eq!(repo.get_characters_for_user(uid).len(), 1);

    // Provisioning a brand-new GM account when max user_id < 99 jumps to >= 100.
    repo.provision_gm_account("gm_fresh", "gm-secret", 55_000_002, "GM Fresh");
    let gm_uid = repo.find_user_id_by_username("gm_fresh").expect("gm uid");
    assert!(gm_uid >= 100);
}

#[test]
fn authenticate_at_sso_jwt_and_malformed_hashes() {
    let repo = WorldRepository::with_demo_seed();
    let raw_secret = [0x44u8; 32];
    repo.set_jwt_signing_secret(&raw_secret).expect("set secret");
    let secret = sha256_bytes(&raw_secret);
    let now = 1_700_001_000u64;
    let uid = repo.find_user_id_by_username("demo").expect("demo uid");

    let mut claims = SsoTokenClaims {
        sub: format!("USER:EVE:{uid}"),
        name: "demo".to_string(),
        owner: "owner".to_string(),
        user_id: uid,
        character_id: 0,
        scp: vec!["publicData".to_string()],
        iss: "https://login.eveonline.com".to_string(),
        aud: vec!["EVE Online".to_string(), "eveLauncherTQ".to_string()],
        iat: now,
        exp: now + 600,
        jti: "jti-good-1".to_string(),
    };
    let good_jwt = sign_test_jwt(&secret, &claims);
    assert_eq!(
        repo.authenticate_at("", &good_jwt, now + 10)
            .expect("empty user jwt")
            .user_id,
        uid
    );
    assert_eq!(
        repo.authenticate_at("EVE-SSO-CONNECTION", &good_jwt, now + 10)
            .expect("sentinel user jwt")
            .user_id,
        uid
    );
    assert_eq!(
        repo.authenticate_at("demo", &good_jwt, now + 10)
            .expect("matching user jwt")
            .user_id,
        uid
    );
    assert_eq!(
        repo.authenticate_at("other_user", &good_jwt, now + 10),
        Err(DbError::InvalidCredentials)
    );

    // Wrong audience rejected even though signature is valid.
    claims.aud = vec!["wrong-audience".to_string()];
    let bad_aud_jwt = sign_test_jwt(&secret, &claims);
    assert_eq!(
        repo.authenticate_at("demo", &bad_aud_jwt, now + 10),
        Err(DbError::InvalidCredentials)
    );

    // Unknown claims.user_id rejected.
    claims.aud = vec!["EVE Online".to_string(), "evomu-launcher".to_string()];
    claims.user_id = 999_888;
    let bad_uid_jwt = sign_test_jwt(&secret, &claims);
    assert_eq!(
        repo.authenticate_at("", &bad_uid_jwt, now + 10),
        Err(DbError::InvalidCredentials)
    );

    // Suspended / banned rejection and expired ban cleanup via JWT auth.
    let now_live = repo.now_unix_secs().max(now) + 100;
    let mut active_claims = claims.clone();
    active_claims.user_id = uid;
    active_claims.iat = now_live;
    active_claims.exp = now_live + 1000;
    let user_jwt = sign_test_jwt(&secret, &active_claims);
    repo.suspend_account("demo", now_live + 200, "cooling off")
        .expect("suspend");
    // Empty username falls through verify_sso_access_token's AccountSuspended error to InvalidCredentials;
    // passing the account username surfaces AccountSuspended from password-path restriction check.
    assert_eq!(
        repo.authenticate_at("", &user_jwt, now_live + 10),
        Err(DbError::InvalidCredentials)
    );
    assert!(matches!(
        repo.authenticate_at("demo", &user_jwt, now_live + 10),
        Err(DbError::AccountSuspended { .. })
    ));
    // Once ban/suspension expires, JWT auth clears it cleanly (with iat after kick timestamp).
    repo.unban_account("demo").expect("clear kick/ban");
    let mut later_claims = active_claims;
    later_claims.iat = now_live + 210;
    later_claims.exp = now_live + 1200;
    let later_jwt = sign_test_jwt(&secret, &later_claims);
    assert!(repo.authenticate_at("", &later_jwt, now_live + 250).is_ok());

    // Malformed password hashes stored on account.
    for bad_token in [
        "$argon2id$v=19$only_params",
        "$argon2id$v=19$m=bad,t=2,p=1$00$00",
        "$argon2id$v=19$m=19456,t=bad,p=1$00$00",
        "$argon2id$v=19$m=19456,t=2,p=bad$00$00",
        "$argon2id$v=19$m=19456,t=2,p=1$not-a-hash",
        "sha256$v1$no-dollar-delimiter",
        "$unknown_scheme$abc$def",
    ] {
        repo.try_with_mut_snapshot(|snap| {
            snap.accounts
                .get_mut("demo")
                .expect("demo")
                .password_token = bad_token.to_string();
            Ok::<_, DbError>(())
        })
        .expect("corrupt stored token");
        assert_eq!(
            repo.authenticate_at("demo", "anything", now),
            Err(DbError::InvalidCredentials),
            "malformed stored token {bad_token} must fail"
        );
    }
    assert_eq!(
        repo.authenticate_at("nonexistent_user", "pw", now),
        Err(DbError::InvalidCredentials)
    );
}

#[test]
fn moderation_ban_suspend_role_and_target_resolution_edges() {
    let repo = WorldRepository::with_demo_seed();
    let uid = repo.find_user_id_by_username("demo").expect("uid");
    let now = 1_700_002_000u64;

    // Unknown targets produce ValidationError.
    assert!(matches!(
        repo.ban_account("missing", now + 100, "r"),
        Err(DbError::ValidationError(_))
    ));
    assert!(matches!(
        repo.suspend_account("missing", now + 100, "r"),
        Err(DbError::ValidationError(_))
    ));
    assert!(matches!(
        repo.unban_account("missing"),
        Err(DbError::ValidationError(_))
    ));
    assert!(matches!(
        repo.unsuspend_account("missing"),
        Err(DbError::ValidationError(_))
    ));
    assert!(matches!(
        repo.ban_account_by_id_or_username("missing", now + 100, "r"),
        Err(DbError::ValidationError(_))
    ));
    assert!(matches!(
        repo.unban_account_by_id_or_username("999999"),
        Err(DbError::ValidationError(_))
    ));
    assert!(matches!(
        repo.suspend_account_by_id_or_username("999999", now + 100, "r"),
        Err(DbError::ValidationError(_))
    ));
    assert!(matches!(
        repo.set_account_role("999999", 0),
        Err(DbError::ValidationError(_))
    ));

    // Numeric username lookup works for suspend (falls back to username), while unban treats numeric strings strictly as user_id.
    let num_uid = repo.provision_player_account("123456", "pw-num");
    assert_eq!(
        repo.suspend_account_by_id_or_username("123456", now + 300, "SUSPENDED:already-prefixed")
            .expect("suspend numeric username"),
        num_uid
    );
    assert!(matches!(
        repo.unsuspend_account_by_id_or_username("123456"),
        Err(DbError::ValidationError(_))
    ));
    assert_eq!(
        repo.unsuspend_account_by_id_or_username(&num_uid.to_string())
            .expect("unsuspend numeric uid")
            .0,
        num_uid
    );
    assert_eq!(
        repo.ban_account_by_id_or_username(&uid.to_string(), now + 500, "ban by id")
            .expect("ban by numeric uid"),
        uid
    );
    assert!(!repo.clear_expired_ban("demo", now + 100));
    assert!(!repo.clear_expired_ban("nobody", now + 100));
    assert!(repo.clear_expired_ban("demo", now + 600));
    assert!(!repo.clear_expired_ban("demo", now + 601));

    // unban_account clears kicked status as well.
    repo.kick_user(uid, "gm kick", now);
    assert!(repo.kicked_reason(uid).is_some());
    repo.unban_account("demo").expect("unban");
    assert!(repo.kicked_reason(uid).is_none());
    repo.unsuspend_account("demo").expect("unsuspend direct");

    // set_account_role by numeric ID and by username returns previous role.
    let (id_res, uname_res, prev_role) = repo
        .set_account_role(&uid.to_string(), service_roles::ROLE_ADMIN)
        .expect("set role by id");
    assert_eq!(id_res, uid);
    assert_eq!(uname_res, "demo");
    let (_, _, prev_after_admin) = repo
        .set_account_role("demo", prev_role)
        .expect("restore role by username");
    assert_eq!(prev_after_admin, service_roles::ROLE_ADMIN);
}

#[test]
fn online_locks_alpha_conflict_revocation_kick_and_peer_ip_edges() {
    let repo = WorldRepository::with_demo_seed();
    let now = 1_700_003_000u64;
    let uid1 = repo.find_user_id_by_username("demo").expect("uid1");
    let uid2 = repo.provision_player_account("alpha_two", "pw2");
    let c1_rec = repo.get_characters_for_user(uid1)[0].clone();
    let c1 = c1_rec.character_id;
    let c2 = 91_000_002i64;
    let c3 = 91_000_003i64;
    // Ensure uid1 character is Omega (clone_grade 2) and uid2 character is Alpha (clone_grade 1).
    repo.try_with_mut_snapshot(|snap| {
        snap.characters.get_mut(&c1).expect("c1").clone_grade = 2;
        let mut r2 = c1_rec.clone();
        r2.character_id = c2;
        r2.user_id = uid2;
        r2.name = "Alpha Two".to_string();
        r2.clone_grade = 1;
        snap.characters.insert(c2, r2);
        Ok::<_, DbError>(())
    })
    .expect("set clone grades");

    // Unknown user -> InvalidCredentials.
    assert_eq!(
        repo.acquire_online_lock_with_computer_hash(888_888, 1, None, None, false, now),
        Err(DbError::InvalidCredentials)
    );

    // Unclaimed login session (computer_hash=None, character_id=None) gets claimed by a new session
    // with computer_hash=Some(...) without displacing the session id.
    repo.acquire_online_lock_with_computer_hash(uid1, 10, None, None, false, now)
        .expect("unclaimed lock");
    repo.acquire_online_lock_with_computer_hash(
        uid1,
        11,
        Some(c1),
        Some("hw-hash-1".to_string()),
        false,
        now + 1,
    )
    .expect("claim unclaimed lock");
    assert_eq!(repo.check_session_revoked(uid1, 10, now + 2).expect("old displaced").0, "MultiLoginBlocked");
    // Re-acquiring with same session_id and computer_hash=None preserves prior computer_hash.
    repo.acquire_online_lock_with_computer_hash(uid1, 11, Some(c1), None, false, now + 3)
        .expect("refresh same session");
    assert_eq!(
        repo.get_online_session(uid1)
            .expect("online")
            .computer_hash
            .as_deref(),
        Some("hw-hash-1")
    );

    // Displaced session blocked from re-acquiring even with allow_displace = true.
    assert!(repo
        .acquire_online_lock_with_computer_hash(uid1, 12, Some(c1), None, true, now + 4)
        .is_ok());
    assert!(matches!(
        repo.acquire_online_lock_with_computer_hash(uid1, 11, Some(c1), None, true, now + 5),
        Err(DbError::ValidationError(_))
    ));
    assert_eq!(
        repo.check_session_revoked(uid1, 11, now + 5)
            .expect("displaced")
            .0,
        "SessionKicked"
    );

    // Alpha computer hash conflict cases:
    // Empty/whitespace hash conflicts for Alpha (uid2) but NOT for Omega (uid1).
    assert!(!repo.has_alpha_computer_hash_conflict(uid1, 12, "   "));
    assert!(repo.has_alpha_computer_hash_conflict(uid2, 20, "   "));
    // uid1 is online with hw-hash-1 (now let's bind hw-hash-1 on session 12).
    repo.acquire_online_lock_with_computer_hash(
        uid1,
        12,
        Some(c1),
        Some("hw-hash-1".to_string()),
        false,
        now + 6,
    )
    .expect("bind hash");
    // Alpha uid2 logging in with same hw-hash-1 as uid1 is blocked!
    assert!(repo.has_alpha_computer_hash_conflict(uid2, 20, "hw-hash-1"));
    // Two Omega accounts sharing the same hash do not conflict.
    let uid3 = repo.provision_player_account("omega_three", "pw3");
    repo.try_with_mut_snapshot(|snap| {
        let mut r3 = c1_rec.clone();
        r3.character_id = c3;
        r3.user_id = uid3;
        r3.name = "Omega Three".to_string();
        r3.clone_grade = 2;
        snap.characters.insert(c3, r3);
        Ok::<_, DbError>(())
    })
    .expect("omega c3");
    assert!(!repo.has_alpha_computer_hash_conflict(uid3, 30, "hw-hash-1"));

    // release_online_lock mismatch vs force 0 vs matching session.
    assert!(!repo.release_online_lock(uid1, 999));
    assert!(repo.release_online_lock(uid1, 12));
    repo.acquire_online_lock(uid1, 13, Some(c1), false, now + 7)
        .expect("relock");
    assert!(repo.release_online_lock(uid1, 0));

    // set_online_session_peer_ip edge cases: empty, invalid, SocketAddr v4, IPv4-mapped v6, bare IP, wrong session, offline.
    repo.acquire_online_lock(uid1, 14, Some(c1), false, now + 8)
        .expect("lock 14");
    repo.set_online_session_peer_ip(uid1, 14, "   ");
    repo.set_online_session_peer_ip(uid1, 14, "not-an-ip");
    repo.set_online_session_peer_ip(uid1, 999, "10.0.0.1:26000");
    repo.set_online_session_peer_ip(uid3, 30, "10.0.0.1:26000"); // offline uid3
    repo.set_online_session_peer_ip(uid1, 14, "[::ffff:127.0.0.2]:26000");
    {
        let rt = repo.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            rt.online_session_ips.get(&uid1),
            Some(&(14, IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2))))
        );
    }
    repo.set_online_session_peer_ip(uid1, 14, "192.168.1.55");
    {
        let rt = repo.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            rt.online_session_ips.get(&uid1),
            Some(&(14, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 55))))
        );
    }

    // kick_user revokes refresh tokens (both with and without last_access_token_jti) and blocks acquire/check_session_revoked.
    {
        let mut rt = repo.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
        rt.refresh_tokens.insert(
            "tok-with-jti".to_string(),
            SsoRefreshTokenRecord {
                refresh_token: "tok-with-jti".to_string(),
                user_id: uid1,
                character_id: c1,
                client_id: "eveLauncherTQ".to_string(),
                scopes: vec![],
                expires_at_unix: now + 1000,
                revoked: false,
                last_access_token_jti: "jti-revoke-me".to_string(),
                last_access_token_exp_unix: now + 500,
            },
        );
        rt.refresh_tokens.insert(
            "tok-no-jti".to_string(),
            SsoRefreshTokenRecord {
                refresh_token: "tok-no-jti".to_string(),
                user_id: uid1,
                character_id: c1,
                client_id: "eveLauncherTQ".to_string(),
                scopes: vec![],
                expires_at_unix: now + 1000,
                revoked: false,
                last_access_token_jti: String::new(),
                last_access_token_exp_unix: 0,
            },
        );
    }
    let kicked_rec = repo.kick_user(uid1, "policy", now + 10).expect("kicked online");
    assert_eq!(kicked_rec.session_id, 14);
    assert_eq!(
        repo.check_session_revoked(uid1, 14, now + 11)
            .expect("revoked after kick")
            .0,
        "SessionKicked"
    );
    assert!(matches!(
        repo.acquire_online_lock(uid1, 15, Some(c1), false, now + 12),
        Err(DbError::ValidationError(_))
    ));
    {
        let rt = repo.auth_runtime.read().unwrap_or_else(|e| e.into_inner());
        assert!(rt.revoked_jtis.contains("jti-revoke-me"));
        assert!(rt.refresh_tokens.get("tok-with-jti").expect("tok1").revoked);
        assert!(rt.refresh_tokens.get("tok-no-jti").expect("tok2").revoked);
    }

    // Ban / suspend session revocation check + ban acquire block.
    repo.clear_revocation_on_fresh_login(uid1, 15);
    repo.ban_account("demo", now + 500, "rwt").expect("ban");
    assert!(matches!(
        repo.acquire_online_lock(uid1, 15, Some(c1), false, now + 20),
        Err(DbError::AccountBanned { .. })
    ));
    assert_eq!(
        repo.check_session_revoked(uid1, 15, now + 20)
            .expect("banned")
            .0,
        "UserBanned"
    );
    repo.suspend_account("demo", now + 500, "cooldown")
        .expect("suspend");
    assert_eq!(
        repo.check_session_revoked(uid1, 15, now + 20)
            .expect("suspended")
            .0,
        "AccountSuspended"
    );
}
