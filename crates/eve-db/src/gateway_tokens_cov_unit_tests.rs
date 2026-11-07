//! Comprehensive unit and mutation-killing tests for `crate::gateway_tokens`
//! (`issue_gateway_dev_token`, `verify_quasar_bearer_jwt`, canonical base64url validation,
//! strict positive integer subject parsing, and privilege-claim / scope injection defenses).

use crate::accounts::{base64url_encode_no_pad, hmac_sha256, sha256_bytes};
use crate::gateway_tokens::{payload_contains_forbidden_privilege_claim, QuasarVerifiedToken};
use crate::persistence::WorldRepository;
use crate::types::{CharacterRecord, DbError, SsoTokenClaims};
use serde_json::{json, Map, Value};

fn insert_char(repo: &WorldRepository, cid: i64, uid: u64, name: &str) -> CharacterRecord {
    let mut ch = repo
        .read_state()
        .characters
        .values()
        .next()
        .expect("demo seed character template")
        .clone();
    ch.character_id = cid;
    ch.user_id = uid;
    ch.name = name.to_string();
    repo.with_mut_snapshot(|s| {
        s.characters.insert(cid, ch.clone());
    });
    ch
}

fn sign_custom_jwt(secret: &[u8; 32], header: &Value, payload: &Value) -> String {
    let h_b64 = base64url_encode_no_pad(&serde_json::to_vec(header).unwrap());
    let p_b64 = base64url_encode_no_pad(&serde_json::to_vec(payload).unwrap());
    let input = format!("{h_b64}.{p_b64}");
    let sig_b64 = base64url_encode_no_pad(&hmac_sha256(secret, input.as_bytes()));
    format!("{input}.{sig_b64}")
}

#[test]
fn forbidden_privilege_claims_and_scope_tokens_kill_all_mutants() {
    let clean: Map<String, Value> = serde_json::from_value(json!({
        "iss": "https://login.eveonline.com",
        "sub": "USER:EVE:1"
    }))
    .unwrap();
    assert!(!payload_contains_forbidden_privilege_claim(&clean, false));
    assert!(!payload_contains_forbidden_privilege_claim(&clean, true));

    // Every single forbidden privilege claim key must be rejected regardless of `allow_sso_scp`.
    let forbidden_keys = [
        "role", "roles", "admin", "gm", "is_admin", "is_gm", "service_roles",
        "service_role_mask", "corp_role", "corp_roles", "permissions", "scope", "scopes",
    ];
    for key in forbidden_keys {
        let mut obj = Map::new();
        obj.insert(key.to_string(), Value::Bool(false));
        assert!(payload_contains_forbidden_privilege_claim(&obj, false), "key={key}");
        assert!(payload_contains_forbidden_privilege_claim(&obj, true), "key={key}");
    }

    // `scp` is always forbidden when `allow_sso_scp == false` (local-dev gateway tokens).
    for scp_val in [json!([]), json!(["publicData"]), json!("publicData")] {
        let obj: Map<String, Value> = serde_json::from_value(json!({ "scp": scp_val })).unwrap();
        assert!(payload_contains_forbidden_privilege_claim(&obj, false));
    }

    // When `allow_sso_scp == true`, `scp` must be an array of non-empty, whitespace-free, unprivileged strings.
    for bad_scp in [
        json!("publicData"), json!(null), json!(123), json!(true), json!({}),
        json!(["publicData", 99]), json!([""]), json!(["read write"]), json!(["publicData\t"]),
        json!(["\npublicData"]), json!(["admin"]), json!(["ADMIN"]), json!(["gm"]), json!(["GM"]),
        json!(["role"]), json!(["Role"]), json!(["roles"]), json!(["ROLES"]),
        json!(["superuser"]), json!(["SuperUser"]), json!(["root"]), json!(["ROOT"]),
        json!(["gm:spawn"]), json!(["GM:teleport"]), json!(["gm:"]),
        json!(["admin:kick"]), json!(["ADMIN:ban"]), json!(["admin:"]),
        json!(["role:gml"]), json!(["ROLE:worldmod"]), json!(["role:"]),
    ] {
        let obj: Map<String, Value> = serde_json::from_value(json!({ "scp": bad_scp })).unwrap();
        assert!(payload_contains_forbidden_privilege_claim(&obj, true), "bad_scp={bad_scp}");
    }

    // Valid unprivileged scopes (including near-prefix lookalikes) are accepted when `allow_sso_scp == true`.
    for good_scp in [
        json!([]),
        json!(["publicData", "esi-wallet.read_character_wallet.v1"]),
        json!(["admins", "gma", "rooter", "superusers", "rol:read", "adm:view"]),
    ] {
        let obj: Map<String, Value> = serde_json::from_value(json!({ "scp": good_scp })).unwrap();
        assert!(!payload_contains_forbidden_privilege_claim(&obj, true), "good_scp={good_scp}");
    }
}

#[test]
fn issue_gateway_dev_token_validation_restrictions_and_payload_contract() {
    let repo = WorldRepository::with_demo_seed();
    let secret = [0x42u8; 32];
    repo.set_jwt_signing_secret(&secret).unwrap();

    let alice_uid = repo.provision_player_account("alice_gw", "pw-alice");
    let bob_uid = repo.provision_player_account("bob_gw", "pw-bob");
    let alice_ch = insert_char(&repo, 91_100_001, alice_uid, "Alice Gw");
    let bob_ch = insert_char(&repo, 91_100_002, bob_uid, "Bob Gw");

    let now = repo.now_unix_secs() + 10_000;

    // Invalid user_id or non-future expiration boundaries (`exp <= now`).
    assert_eq!(
        repo.issue_gateway_dev_token(0, None, "tranquility", now + 60, now),
        Err(DbError::InvalidCredentials)
    );
    assert_eq!(
        repo.issue_gateway_dev_token(alice_uid, None, "tranquility", now - 1, now),
        Err(DbError::InvalidCredentials)
    );
    assert_eq!(
        repo.issue_gateway_dev_token(alice_uid, None, "tranquility", now, now),
        Err(DbError::InvalidCredentials)
    );
    assert_eq!(
        repo.issue_gateway_dev_token(999_999, None, "tranquility", now + 60, now),
        Err(DbError::InvalidCredentials)
    );

    // Invalid or unowned character IDs (`<= 0`, missing, or owned by another user).
    for bad_cid in [0i64, -1, 999_999_999, bob_ch.character_id] {
        assert_eq!(
            repo.issue_gateway_dev_token(alice_uid, Some(bad_cid), "tranquility", now + 60, now),
            Err(DbError::InvalidCredentials),
            "bad character_id {bad_cid} must fail"
        );
    }

    // Exact +1s expiry boundary succeeds; empty tenant omits `tenant` claim; `USER:EVE:<uid>` subject.
    let user_tok_1 = repo
        .issue_gateway_dev_token(alice_uid, None, "", now + 1, now)
        .expect("issue user gateway token");
    let user_tok_2 = repo
        .issue_gateway_dev_token(alice_uid, None, "tranquility", now + 300, now)
        .expect("issue second user gateway token");
    assert_ne!(user_tok_1, user_tok_2, "monotonic jti must make tokens distinct");

    let parts1: Vec<&str> = user_tok_1.split('.').collect();
    assert_eq!(parts1.len(), 3);
    let p1: Value = serde_json::from_slice(
        &crate::accounts::base64url_decode_no_pad(parts1[1]).unwrap(),
    )
    .unwrap();
    assert_eq!(p1["iss"], "https://login.eveonline.com");
    assert_eq!(p1["sub"], format!("USER:EVE:{alice_uid}"));
    assert_eq!(p1["iat"], now);
    assert_eq!(p1["exp"], now + 1);
    assert!(p1["jti"]
        .as_str()
        .unwrap()
        .starts_with(&format!("gwdev-{now}-{alice_uid}-")));
    assert!(p1.get("tenant").is_none());

    let parts2: Vec<&str> = user_tok_2.split('.').collect();
    let p2: Value = serde_json::from_slice(
        &crate::accounts::base64url_decode_no_pad(parts2[1]).unwrap(),
    )
    .unwrap();
    assert_eq!(p2["tenant"], "tranquility");

    assert_eq!(
        repo.verify_quasar_bearer_jwt(&user_tok_1, now),
        Ok(QuasarVerifiedToken {
            user_id: alice_uid,
            character_id: None,
            is_full_sso: false,
        })
    );
    // At `now + 1 == exp`, token is expired (`now_unix >= exp`).
    assert_eq!(
        repo.verify_quasar_bearer_jwt(&user_tok_1, now + 1),
        Err(DbError::InvalidCredentials)
    );

    // Character-scoped dev token sets `CHARACTER:EVE:<cid>` and verifies with `is_full_sso == false`.
    let char_tok = repo
        .issue_gateway_dev_token(
            alice_uid,
            Some(alice_ch.character_id),
            "tranquility",
            now + 120,
            now,
        )
        .expect("issue char gateway token");
    assert_eq!(
        repo.verify_quasar_bearer_jwt(&char_tok, now + 119),
        Ok(QuasarVerifiedToken {
            user_id: alice_uid,
            character_id: Some(alice_ch.character_id),
            is_full_sso: false,
        })
    );

    // Suspended and banned accounts cannot mint OR verify dev tokens while restriction is active.
    repo.suspend_account("alice_gw", now + 50, "SUSPENDED:cooldown")
        .unwrap();
    assert!(matches!(
        repo.issue_gateway_dev_token(alice_uid, None, "tranquility", now + 120, now + 10),
        Err(DbError::AccountSuspended { .. })
    ));
    assert!(matches!(
        repo.verify_quasar_bearer_jwt(&char_tok, now + 10),
        Err(DbError::AccountSuspended { .. })
    ));
    // Once `now >= banned_until_unix`, restriction has expired.
    assert!(repo.verify_quasar_bearer_jwt(&char_tok, now + 50).is_ok());
    repo.unsuspend_account_by_id_or_username("alice_gw").unwrap();

    repo.ban_account_by_id_or_username("alice_gw", now + 80, "rwt")
        .unwrap();
    assert!(matches!(
        repo.issue_gateway_dev_token(alice_uid, None, "tranquility", now + 120, now + 10),
        Err(DbError::AccountBanned { .. })
    ));
    assert!(matches!(
        repo.verify_quasar_bearer_jwt(&char_tok, now + 10),
        Err(DbError::AccountBanned { .. })
    ));
    repo.unban_account_by_id_or_username("alice_gw").unwrap();
}

#[test]
fn verify_quasar_bearer_jwt_full_sso_vs_reassigned_or_nonpositive_character() {
    let repo = WorldRepository::with_demo_seed();
    let secret = [0x77u8; 32];
    repo.set_jwt_signing_secret(&secret).unwrap();
    let signing_key = repo.auth_runtime.read().unwrap().jwt_signing_secret;

    let alice_uid = repo.provision_player_account("alice_sso", "pw-alice");
    let bob_uid = repo.provision_player_account("bob_sso", "pw-bob");
    let alice_ch = insert_char(&repo, 91_200_001, alice_uid, "Alice Sso");

    let now = 1_700_200_000u64;
    let verifier = "v9f0A_pkce_verifier_string_with_high_entropy_12345";
    let challenge = base64url_encode_no_pad(&sha256_bytes(verifier.as_bytes()));
    let code = repo
        .authorize_pkce(
            "alice_sso",
            "pw-alice",
            alice_ch.character_id,
            "evomu-launcher",
            "eveauth://callback",
            vec!["publicData".into()],
            &challenge,
            "S256",
            now,
        )
        .unwrap();
    let bundle = repo
        .exchange_pkce_code(&code, "evomu-launcher", verifier, now + 2)
        .unwrap();

    assert_eq!(
        repo.verify_quasar_bearer_jwt(&bundle.access_token, now + 5),
        Ok(QuasarVerifiedToken {
            user_id: alice_uid,
            character_id: Some(alice_ch.character_id),
            is_full_sso: true,
        })
    );

    // If the character is reassigned to Bob after SSO issuance, `verify_quasar_bearer_jwt` rejects it.
    repo.with_mut_snapshot(|s| {
        s.characters
            .get_mut(&alice_ch.character_id)
            .unwrap()
            .user_id = bob_uid;
    });
    assert_eq!(
        repo.verify_quasar_bearer_jwt(&bundle.access_token, now + 5),
        Err(DbError::InvalidCredentials)
    );
    repo.with_mut_snapshot(|s| {
        s.characters
            .get_mut(&alice_ch.character_id)
            .unwrap()
            .user_id = alice_uid;
    });

    // Crafted SSO claims with `character_id <= 0` (`0` and `-1`) pass `verify_sso_access_token`
    // syntax checks if signed, and must be rejected by `claims.character_id > 0` even if key `0` exists.
    insert_char(&repo, 0, alice_uid, "Synthetic Zero");
    for bad_cid in [0i64, -1i64] {
        let claims = SsoTokenClaims {
            sub: format!("CHARACTER:EVE:{bad_cid}"),
            name: "Zero".into(),
            owner: "owner-hash".into(),
            user_id: alice_uid,
            character_id: bad_cid,
            scp: vec!["publicData".into()],
            iss: "https://login.eveonline.com".into(),
            aud: vec!["EVE Online".into()],
            iat: now,
            exp: now + 300,
            jti: format!("sso-bad-{bad_cid}"),
        };
        let bad_sso = sign_custom_jwt(
            &signing_key,
            &json!({"alg": "HS256", "typ": "JWT"}),
            &serde_json::to_value(&claims).unwrap(),
        );
        assert_eq!(
            repo.verify_quasar_bearer_jwt(&bad_sso, now + 1),
            Err(DbError::InvalidCredentials)
        );
    }
}

#[test]
fn verify_quasar_bearer_jwt_malformed_segments_headers_temporal_and_sub_cross_claims() {
    let repo = WorldRepository::with_demo_seed();
    repo.set_jwt_signing_secret(&[0x33u8; 32]).unwrap();
    let secret = repo.auth_runtime.read().unwrap().jwt_signing_secret;

    let alice_uid = repo.provision_player_account("alice_edge", "pw-alice");
    let bob_uid = repo.provision_player_account("bob_edge", "pw-bob");
    let alice_c1 = insert_char(&repo, 91_300_001, alice_uid, "Alice One");
    let alice_c2 = insert_char(&repo, 91_300_002, alice_uid, "Alice Two");
    let bob_c1 = insert_char(&repo, 91_300_003, bob_uid, "Bob One");

    let now = 1_700_300_000u64;
    let std_hdr = json!({"alg": "HS256", "typ": "JWT"});
    let valid_hdr_b64 = base64url_encode_no_pad(b"{\"alg\":\"HS256\"}");
    let valid_pay_b64 = base64url_encode_no_pad(
        format!("{{\"sub\":\"USER:EVE:{alice_uid}\",\"exp\":{}}}", now + 60).as_bytes(),
    );

    // 1. Structural & canonical base64url checks across all 3 segments.
    for malformed in [
        "".to_string(),
        "onlyone".to_string(),
        "two.parts".to_string(),
        "a.b.c.d".to_string(),
        format!(".{valid_pay_b64}.AA"),
        format!("{valid_hdr_b64}..AA"),
        format!("{valid_hdr_b64}.{valid_pay_b64}."),
        // Standard base64 chars ('+', '/', '=') and mod-4==1 lengths ("A") and non-canonical low bits ("AB", "AAB")
        format!("ab+c.{valid_pay_b64}.AA"),
        format!("ab/c.{valid_pay_b64}.AA"),
        format!("YQ==.{valid_pay_b64}.AA"),
        format!("A.{valid_pay_b64}.AA"),
        format!("AB.{valid_pay_b64}.AA"),
        format!("AAB.{valid_pay_b64}.AA"),
        format!("{valid_hdr_b64}.AB.AA"),
        format!("{valid_hdr_b64}.{valid_pay_b64}.AB"),
        // Non-JSON or non-object JSON header / payload
        format!("{}.{valid_pay_b64}.AA", base64url_encode_no_pad(b"not-json")),
        format!("{}.{valid_pay_b64}.AA", base64url_encode_no_pad(b"[1,2]")),
        format!(
            "{valid_hdr_b64}.{}.AA",
            base64url_encode_no_pad(b"not-json")
        ),
        format!("{valid_hdr_b64}.{}.AA", base64url_encode_no_pad(b"[1,2]")),
    ] {
        assert_eq!(
            repo.verify_quasar_bearer_jwt(&malformed, now),
            Err(DbError::InvalidCredentials),
            "malformed token must fail: {malformed}"
        );
    }

    // 2. Bad header `alg` or `crit` extension rejection, and wrong signature rejection.
    for bad_hdr in [
        json!({"typ": "JWT"}),
        json!({"alg": "none", "typ": "JWT"}),
        json!({"alg": "RS256", "typ": "JWT"}),
        json!({"alg": "HS256", "crit": ["exp"]}),
    ] {
        let tok = sign_custom_jwt(
            &secret,
            &bad_hdr,
            &json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": now + 60}),
        );
        assert_eq!(
            repo.verify_quasar_bearer_jwt(&tok, now),
            Err(DbError::InvalidCredentials)
        );
    }
    let wrong_secret_tok = sign_custom_jwt(
        &[0x99u8; 32],
        &std_hdr,
        &json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": now + 60}),
    );
    assert_eq!(
        repo.verify_quasar_bearer_jwt(&wrong_secret_tok, now),
        Err(DbError::InvalidCredentials)
    );

    // 3. Temporal (`exp`, `nbf`, `iat`), `jti`, and privilege-claim payload checks.
    for bad_payload in [
        json!({"sub": format!("USER:EVE:{alice_uid}")}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": 0}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": "9999999999"}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": now}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": now + 60, "scp": ["publicData"]}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": now + 60, "gm": true}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": now + 60, "nbf": "bad"}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": now + 60, "nbf": now + 1}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": now + 60, "iat": "bad"}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": now + 60, "iat": now + 1}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": now + 60, "jti": ""}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": now + 60, "jti": 42}),
        json!({"exp": now + 60}),
        json!({"sub": 12345, "exp": now + 60}),
    ] {
        let tok = sign_custom_jwt(&secret, &std_hdr, &bad_payload);
        assert_eq!(
            repo.verify_quasar_bearer_jwt(&tok, now),
            Err(DbError::InvalidCredentials),
            "bad payload must fail: {bad_payload}"
        );
    }

    // Exact boundaries `nbf == now`, `iat == now`, `exp == now + 1`, and omitted `iat`/`jti` succeed!
    let boundary_ok = sign_custom_jwt(
        &secret,
        &std_hdr,
        &json!({
            "sub": format!("USER:EVE:{alice_uid}"),
            "nbf": now,
            "iat": now,
            "exp": now + 1
        }),
    );
    assert!(repo.verify_quasar_bearer_jwt(&boundary_ok, now).is_ok());

    let minimal_ok = sign_custom_jwt(
        &secret,
        &std_hdr,
        &json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": now + 60}),
    );
    assert!(repo.verify_quasar_bearer_jwt(&minimal_ok, now).is_ok());

    // Revoked `jti` fails while unrevoked `jti` succeeds.
    repo.auth_runtime
        .write()
        .unwrap()
        .revoked_jtis
        .insert("revoked-jti-1".into());
    let revoked_tok = sign_custom_jwt(
        &secret,
        &std_hdr,
        &json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": now + 60, "jti": "revoked-jti-1"}),
    );
    assert_eq!(
        repo.verify_quasar_bearer_jwt(&revoked_tok, now),
        Err(DbError::InvalidCredentials)
    );

    // 4. Strict `sub` parsing & orphan character (`user_id == 0`) & missing account checks.
    let mut orphan_char = alice_c1.clone();
    orphan_char.character_id = 90_999_991;
    orphan_char.user_id = 0;
    let mut ghost_acc_char = alice_c1.clone();
    ghost_acc_char.character_id = 90_999_992;
    ghost_acc_char.user_id = 888_888;
    repo.with_mut_snapshot(|g| {
        g.characters.insert(orphan_char.character_id, orphan_char);
        g.characters
            .insert(ghost_acc_char.character_id, ghost_acc_char);
    });

    for bad_sub in [
        "CORPORATION:EVE:98000001".to_string(),
        "USER:EVE:".to_string(),
        "USER:EVE:0".to_string(),
        format!("USER:EVE:0{alice_uid}"),
        format!("USER:EVE:+{alice_uid}"),
        "USER:EVE:notanumber".to_string(),
        "USER:EVE:999999".to_string(),
        "CHARACTER:EVE:".to_string(),
        "CHARACTER:EVE:0".to_string(),
        "CHARACTER:EVE:-1".to_string(),
        format!("CHARACTER:EVE:0{}", alice_c1.character_id),
        format!("CHARACTER:EVE:+{}", alice_c1.character_id),
        "CHARACTER:EVE:999999999".to_string(),
        "CHARACTER:EVE:90999991".to_string(),
        "CHARACTER:EVE:90999992".to_string(),
    ] {
        let tok = sign_custom_jwt(&secret, &std_hdr, &json!({"sub": bad_sub, "exp": now + 60}));
        assert_eq!(
            repo.verify_quasar_bearer_jwt(&tok, now),
            Err(DbError::InvalidCredentials),
            "bad sub must fail: {bad_sub}"
        );
    }

    // 5. Cross-claim consistency (`user_id`/`uid` and `character_id`/`cid`).
    insert_char(&repo, 0, alice_uid, "Synthetic Zero Cross");
    for bad_cross in [
        json!({"sub": format!("USER:EVE:{alice_uid}"), "user_id": bob_uid, "exp": now + 60}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "uid": bob_uid, "exp": now + 60}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "user_id": "bad", "exp": now + 60}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "character_id": 0, "exp": now + 60}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "character_id": -1, "exp": now + 60}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "cid": "bad", "exp": now + 60}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "cid": bob_c1.character_id, "exp": now + 60}),
        json!({"sub": format!("USER:EVE:{alice_uid}"), "cid": 999_999_999i64, "exp": now + 60}),
        // Both characters belong to Alice, but `character_id` disagrees with `CHARACTER:EVE:<sub_cid>`!
        json!({
            "sub": format!("CHARACTER:EVE:{}", alice_c1.character_id),
            "character_id": alice_c2.character_id,
            "exp": now + 60
        }),
    ] {
        let tok = sign_custom_jwt(&secret, &std_hdr, &bad_cross);
        assert_eq!(
            repo.verify_quasar_bearer_jwt(&tok, now),
            Err(DbError::InvalidCredentials),
            "bad cross-claim must fail: {bad_cross}"
        );
    }

    // Matching cross-claims succeed (`uid` + `cid` on USER sub, and `user_id` + `character_id` on CHARACTER sub).
    let ok_user_cross = sign_custom_jwt(
        &secret,
        &std_hdr,
        &json!({
            "sub": format!("USER:EVE:{alice_uid}"),
            "uid": alice_uid,
            "cid": alice_c1.character_id,
            "exp": now + 60
        }),
    );
    assert_eq!(
        repo.verify_quasar_bearer_jwt(&ok_user_cross, now),
        Ok(QuasarVerifiedToken {
            user_id: alice_uid,
            character_id: None,
            is_full_sso: false,
        })
    );
    let ok_char_cross = sign_custom_jwt(
        &secret,
        &std_hdr,
        &json!({
            "sub": format!("CHARACTER:EVE:{}", alice_c1.character_id),
            "user_id": alice_uid,
            "character_id": alice_c1.character_id,
            "exp": now + 60
        }),
    );
    assert_eq!(
        repo.verify_quasar_bearer_jwt(&ok_char_cross, now),
        Ok(QuasarVerifiedToken {
            user_id: alice_uid,
            character_id: Some(alice_c1.character_id),
            is_full_sso: false,
        })
    );

    // 6. Kicked user timestamp boundaries: missing `iat` -> Err, `iat <= kicked_at` -> Err, `iat > kicked_at` -> Ok.
    repo.kick_user(alice_uid, "security kick", now);
    for bad_iat in [None, Some(now - 1), Some(now)] {
        let mut p = json!({"sub": format!("USER:EVE:{alice_uid}"), "exp": now + 60});
        if let Some(iat) = bad_iat {
            p["iat"] = json!(iat);
        }
        let tok = sign_custom_jwt(&secret, &std_hdr, &p);
        assert_eq!(
            repo.verify_quasar_bearer_jwt(&tok, now + 5),
            Err(DbError::InvalidCredentials),
            "token with iat={bad_iat:?} at kick={now} must fail"
        );
    }
    let post_kick_tok = sign_custom_jwt(
        &secret,
        &std_hdr,
        &json!({"sub": format!("USER:EVE:{alice_uid}"), "iat": now + 1, "exp": now + 60}),
    );
    assert!(repo.verify_quasar_bearer_jwt(&post_kick_tok, now + 5).is_ok());
}
