//! Targeted unit and mutation-killing tests for `accounts.rs`.

use super::*;

#[test]
fn sha256_hmac_constant_time_and_base64url_primitives() {
    // Empty and multi-block SHA-256 vectors
    let empty_sha = sha256_bytes(b"");
    assert_eq!(
        empty_sha[..4],
        [0xe3, 0xb0, 0xc4, 0x42]
    );
    let long_msg = vec![b'a'; 128];
    let long_sha = sha256_bytes(&long_msg);
    assert_ne!(long_sha, empty_sha);

    // constant_time_eq
    assert!(constant_time_eq(b"evomu-secret", b"evomu-secret"));
    assert!(!constant_time_eq(b"evomu-secret", b"evomu-secreX"));
    assert!(!constant_time_eq(b"short", b"longer"));

    // HMAC-SHA256 with short key (<= 64B) and long key (> 64B)
    let mac_short = hmac_sha256(b"short-key", b"payload");
    let mac_long = hmac_sha256(&[0x42u8; 80], b"payload");
    assert_ne!(mac_short, mac_long);
    assert_eq!(mac_short, hmac_sha256(b"short-key", b"payload"));

    // Base64URL encode/decode roundtrips for 1, 2, 3, 4+ byte lengths
    for len in 0..=10 {
        let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 0xf3) as u8).collect();
        let encoded = base64url_encode_no_pad(&bytes);
        assert!(!encoded.contains('='));
        let decoded = base64url_decode_no_pad(&encoded).expect("decode valid base64url");
        assert_eq!(decoded, bytes);
    }
    // Standard base64 characters '+' and '/' and trailing '=' padding are also accepted on decode
    assert_eq!(base64url_decode_no_pad("ab+/==").unwrap().len(), 3);
    assert!(base64url_decode_no_pad("bad!char").is_err());
    assert!(base64url_decode_no_pad("a").is_err()); // chunk len 1 is invalid
}

#[test]
fn password_hashing_legacy_sha256_and_account_provisioning_roles_and_bans() {
    let repo = WorldRepository::with_demo_seed();

    // Argon2id salted password hash & legacy sha256$v1$ verification
    let uid = repo.provision_player_account("unit_pilot", "pw_alpha");
    assert!(uid > 0);
    assert_eq!(repo.provision_player_account("unit_pilot", "pw_alpha"), uid);
    assert_eq!(repo.find_user_id_by_username("unit_pilot"), Some(uid));
    assert!(repo.authenticate_at("unit_pilot", "wrong_pw", 1_700_000_000).is_err());

    // Legacy sha256$v1$ credential verification
    let legacy_hash = WorldRepository::hash_password_with_salt_sha256_legacy("unit_pilot", "0123456789abcdef", "legacy_pw");
    repo.with_mut_snapshot(|snap| {
        snap.accounts.get_mut("unit_pilot").unwrap().password_token = legacy_hash;
    });
    assert!(repo.authenticate_at("unit_pilot", "legacy_pw", 1_700_000_000).is_ok());
    assert!(repo.authenticate_at("unit_pilot", "wrong_pw", 1_700_000_000).is_err());

    // Provision GM account idempotently
    repo.provision_gm_account("gm_unit", "gm_pass", 90_999_001, "GM Unit");
    repo.provision_gm_account("gm_unit", "gm_pass", 90_999_001, "GM Unit");
    let gm_acc = repo.authenticate_at("gm_unit", "gm_pass", 1_700_000_000).unwrap();
    assert_eq!(gm_acc.role & service_roles::ROLE_GM_DEFAULT, service_roles::ROLE_GM_DEFAULT);
    // Refuses to ban, suspend, or kick an account holding ROLE_ADMIN
    assert!(repo.ban_account("gm_unit", 2_000_000_000, "test").is_err());
    assert!(repo.suspend_account("gm_unit", 2_000_000_000, "test").is_err());
    assert!(repo.kick_user(gm_acc.user_id, "test", 1_700_000_000).is_none());

    // Role bitmask update by username and by numeric ID
    let (r_uid, r_name, prev_role) = repo.set_account_role("unit_pilot", service_roles::ROLE_PLAYER_DEFAULT | service_roles::ROLE_GML).unwrap();
    assert_eq!(r_uid, uid);
    assert_eq!(r_name, "unit_pilot");
    assert_eq!(prev_role, service_roles::ROLE_PLAYER_DEFAULT);
    let (_, _, prev_role2) = repo.set_account_role(&uid.to_string(), service_roles::ROLE_PLAYER_DEFAULT).unwrap();
    assert_eq!(prev_role2, service_roles::ROLE_PLAYER_DEFAULT | service_roles::ROLE_GML);

    // Ban, Suspend, Expiration, and Unban by username or ID
    repo.ban_account_by_id_or_username(&uid.to_string(), 1_700_000_500, "RMT").unwrap();
    assert!(matches!(
        repo.authenticate_at("unit_pilot", "legacy_pw", 1_700_000_499),
        Err(DbError::AccountBanned { until_unix: 1_700_000_500, .. })
    ));
    // At exact expiry timestamp (now_unix >= until), clear_expired_ban lifts the ban
    assert!(repo.clear_expired_ban_by_id(uid, 1_700_000_500));
    assert!(repo.authenticate_at("unit_pilot", "legacy_pw", 1_700_000_500).is_ok());

    repo.suspend_account_by_id_or_username("unit_pilot", 1_700_000_900, "SUSPENDED:Spam").unwrap();
    assert!(matches!(
        repo.authenticate_at("unit_pilot", "legacy_pw", 1_700_000_600),
        Err(DbError::AccountSuspended { until_unix: 1_700_000_900, reason }) if reason == "Spam"
    ));
    repo.unsuspend_account_by_id_or_username(&uid.to_string()).unwrap();
    assert!(repo.authenticate_at("unit_pilot", "legacy_pw", 1_700_000_600).is_ok());
}

#[test]
fn online_session_locks_alpha_conflict_kick_and_sso_pkce_lifecycle() {
    let repo = WorldRepository::with_demo_seed();
    // JWT signing secret validation (< 32 bytes rejected)
    assert!(repo.set_jwt_signing_secret(b"too-short").is_err());
    let kid_before = repo.jwt_key_id();
    repo.set_jwt_signing_secret(&[0x55u8; 32]).unwrap();
    assert_ne!(repo.jwt_key_id(), kid_before);

    // Online session lock acquisition, displacement, and Alpha computer_hash conflict
    assert_eq!(repo.online_session_count(), 0);
    let displaced = repo
        .acquire_online_lock_with_computer_hash(1, 101, Some(90_000_001), Some("hw-1".into()), false, 1_700_000_000)
        .unwrap();
    assert!(displaced.is_none());
    assert_eq!(repo.online_session_count(), 1);
    assert_eq!(repo.online_sessions_snapshot(), vec![(1, 101)]);
    assert!(repo.get_online_session(1).is_some());

    // Competing session without allow_displace is rejected with AlreadyLoggedIn
    assert_eq!(
        repo.acquire_online_lock(1, 102, Some(90_000_001), false, 1_700_000_005),
        Err(DbError::AlreadyLoggedIn { user_id: 1, active_session_id: 101 })
    );
    assert_eq!(
        repo.check_session_revoked(1, 102, 1_700_000_005).map(|(code, _)| code),
        Some("MultiLoginBlocked".into())
    );

    // Second Alpha account on the same computer_hash triggers conflict
    let u2 = repo.provision_player_account("alpha_two", "pw2");
    assert!(repo.has_alpha_computer_hash_conflict(u2, 202, "hw-1"));
    assert!(!repo.has_alpha_computer_hash_conflict(u2, 202, "hw-2"));

    // Displacing session 101 marks 101 as displaced
    let disp = repo.acquire_online_lock(1, 102, Some(90_000_001), true, 1_700_000_010).unwrap();
    assert_eq!(disp.unwrap().session_id, 101);
    assert_eq!(
        repo.check_session_revoked(1, 101, 1_700_000_010).map(|(code, _)| code),
        Some("SessionKicked".into())
    );

    // PKCE + SSO token exchange, refresh, launcher authentication, and kick revocation
    let verifier = "pkce-secret-verifier-0123456789abcdef";
    let challenge = base64url_encode_no_pad(&sha256_bytes(verifier.as_bytes()));
    assert!(repo.authorize_pkce("demo", "demo-secret", 90_000_001, "evomu-launcher", "https://localhost/cb", vec!["esi-mail".into()], &challenge, "plain", 1_700_000_000).is_err());
    let code = repo
        .authorize_pkce("demo", "demo-secret", 90_000_001, "evomu-launcher", "https://localhost/cb", vec!["esi-mail".into()], &challenge, "S256", 1_700_000_000)
        .unwrap();
    let bundle = repo.exchange_pkce_code(&code, "evomu-launcher", verifier, 1_700_000_010).unwrap();
    let claims = repo.verify_sso_access_token(&bundle.access_token, 1_700_000_020).unwrap();
    assert_eq!(claims.character_id, 90_000_001);

    // Authenticate directly using the SSO JWT access token
    let sso_acc = repo.authenticate_at("__sso__", &bundle.access_token, 1_700_000_020).unwrap();
    assert_eq!(sso_acc.user_id, 1);

    // Rotate refresh token
    let refreshed = repo.refresh_sso_token(&bundle.refresh_token, "evomu-launcher", 1_700_000_030).unwrap();
    assert!(repo.refresh_sso_token(&bundle.refresh_token, "evomu-launcher", 1_700_000_031).is_err());

    // Kick user revokes active session and SSO tokens
    let kicked_sess = repo.kick_user(1, "GM Kick", 1_700_000_040);
    assert!(kicked_sess.is_some());
    assert_eq!(repo.kicked_reason(1), Some("GM Kick".into()));
    assert!(repo.verify_sso_access_token(&refreshed.access_token, 1_700_000_040).is_err());
    repo.clear_revocation_on_fresh_login(1, 102);
    assert!(repo.kicked_reason(1).is_none());
    assert!(!repo.release_online_lock(1, 102));
}
