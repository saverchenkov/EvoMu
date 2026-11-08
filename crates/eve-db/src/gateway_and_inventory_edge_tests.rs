//! Unit and mutation-killing tests for `snapshot_ext`, `inventory` split stack,
//! and `gateway_tokens` Quasar / dev token verification edge cases.

#![cfg(test)]

use crate::accounts::{base64url_encode_no_pad, hmac_sha256};
use crate::gateway_tokens::QuasarVerifiedToken;
use crate::persistence::WorldRepository;
use crate::types::*;
use serde_json::json;
use std::net::IpAddr;

fn sign_dev_jwt(header: &serde_json::Value, payload: &serde_json::Value) -> String {
    let h_b64 = base64url_encode_no_pad(&serde_json::to_vec(header).unwrap());
    let p_b64 = base64url_encode_no_pad(&serde_json::to_vec(payload).unwrap());
    let input = format!("{h_b64}.{p_b64}");
    let sig_b64 = base64url_encode_no_pad(&hmac_sha256(b"dev", input.as_bytes()));
    format!("{input}.{sig_b64}")
}

#[test]
fn test_is_item_locked_excluding_trade_and_consumable_branches() {
    let mut snap = WorldStateSnapshot::default();

    // 1. Trade notes (character_id == -73)
    let trade_note_1 = json!({
        "trade_id": 1001, "items_a": [[10, 1], [11, 2]], "items_b": [[20, 1]], "completed": false, "cancelled": false,
    }).to_string();
    snap.owner_notes.insert(1, OwnerNoteRecord {
        note_id: 1, character_id: -73, item_id: 0, label: "trade".into(), note: trade_note_1, timestamp_unix: 1000,
    });

    // item 10 in items_a; item 20 in items_b: locked unless exempt_trade_id == Some(1001)
    assert!(snap.is_item_locked_excluding_trade(10, None));
    assert!(snap.is_item_locked_excluding_trade(10, Some(999)));
    assert!(!snap.is_item_locked_excluding_trade(10, Some(1001)));
    assert!(snap.is_item_locked_excluding_trade(20, None));
    assert!(!snap.is_item_locked_excluding_trade(20, Some(1001)));
    assert!(!snap.is_item_locked_excluding_trade(30, None));

    // Completed or cancelled trades must NOT lock items
    snap.owner_notes.get_mut(&1).unwrap().note = json!({
        "trade_id": 1002, "items_a": [[10, 1]], "items_b": [], "completed": true, "cancelled": false,
    }).to_string();
    assert!(!snap.is_item_locked_excluding_trade(10, None));

    snap.owner_notes.get_mut(&1).unwrap().note = json!({
        "trade_id": 1003, "items_a": [[10, 1]], "items_b": [], "completed": false, "cancelled": true,
    }).to_string();
    assert!(!snap.is_item_locked_excluding_trade(10, None));

    // 2. Raffle notes (character_id == -74)
    snap.owner_notes.insert(2, OwnerNoteRecord {
        note_id: 2, character_id: -74, item_id: 40, label: "raffle".into(),
        note: json!({"item_id": 40, "winner_id": null, "expired": false}).to_string(),
        timestamp_unix: 1000,
    });
    assert!(snap.is_item_locked_excluding_trade(40, None));
    assert!(!snap.is_item_locked_excluding_trade(41, None));

    snap.owner_notes.get_mut(&2).unwrap().note = json!({"item_id": 40, "winner_id": null, "expired": true}).to_string();
    assert!(!snap.is_item_locked_excluding_trade(40, None));

    snap.owner_notes.get_mut(&2).unwrap().note = json!({"item_id": 40, "winner_id": 9999, "expired": false}).to_string();
    assert!(!snap.is_item_locked_excluding_trade(40, None));

    snap.owner_notes.insert(3, OwnerNoteRecord {
        note_id: 3, character_id: 1234, item_id: 40, label: "unrelated".into(), note: "hello".into(), timestamp_unix: 1000,
    });
    assert!(!snap.is_item_locked_excluding_trade(40, None));

    // 3. Consumable by check with active ship in cargo vs contract
    let repo_seed = WorldRepository::with_demo_seed();
    let mut ch = repo_seed
        .read_state()
        .characters
        .values()
        .next()
        .unwrap()
        .clone();
    ch.character_id = 1;
    ch.user_id = 1;
    ch.solar_system_id = 30000142;
    snap.characters.insert(1, ch);

    let mut ship = InventoryItemRecord {
        item_id: 50,
        type_id: 587,
        owner_id: 1,
        location_id: 60000001,
        flag: InventoryFlag::Hangar,
        quantity: 1,
        singleton: true,
        ..Default::default()
    };
    let booster = InventoryItemRecord {
        item_id: 51,
        type_id: 28668,
        owner_id: 1,
        location_id: 50,
        flag: InventoryFlag::Cargo,
        quantity: 1,
        singleton: false,
        ..Default::default()
    };
    snap.items.insert(50, ship.clone());
    snap.items.insert(51, booster.clone());

    // When ship is locked in contract, booster in its cargo is not consumable
    snap.contracts.insert(
        1,
        ContractRecord {
            contract_id: 1,
            issuer_id: 1,
            status: "Outstanding".into(),
            item_ids: vec![50],
            ..Default::default()
        },
    );
    assert!(!snap.is_item_consumable_by(&booster, 1, Some(60000001), Some(50)));

    // Once unlocked, booster is consumable when ship is colocated at dock
    snap.contracts.clear();
    assert!(snap.is_item_consumable_by(&booster, 1, Some(60000001), Some(50)));

    // In space (station_or_struct_id == None), ship location must match character's solar_system_id
    assert!(!snap.is_item_consumable_by(&booster, 1, None, Some(50)));
    ship.location_id = 30000142;
    snap.items.insert(50, ship.clone());
    assert!(snap.is_item_consumable_by(&booster, 1, None, Some(50)));

    // If ship location does not match station_or_struct_id, not colocated
    assert!(!snap.is_item_consumable_by(&booster, 1, Some(60000002), Some(50)));
}

#[test]
fn test_split_stack_locked_items_bpc_mutated_and_ancestors() {
    let repo = WorldRepository::with_demo_seed();
    let cid = 901;
    let uid = 501;

    let mut ch = repo
        .read_state()
        .characters
        .values()
        .next()
        .unwrap()
        .clone();
    ch.character_id = cid;
    ch.user_id = uid;
    ch.name = "SplitTester".into();

    repo.with_mut_snapshot(|s| {
        s.characters.insert(cid, ch);
        // Base container item
        s.items.insert(
            100,
            InventoryItemRecord {
                item_id: 100,
                type_id: 3468, // Secure Container (locked type 3468)
                owner_id: cid,
                location_id: 60000001,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Item in secure container
        s.items.insert(
            101,
            InventoryItemRecord {
                item_id: 101,
                type_id: 34,
                owner_id: cid,
                location_id: 100,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // Splitting item inside container of type 3468 fails with ItemLocked
    assert_eq!(
        repo.split_stack(cid, 101, 10),
        Err(DbError::ItemLocked(101))
    );

    // Change container type to normal (not 3468)
    repo.with_mut_snapshot(|s| {
        s.items.get_mut(&100).unwrap().type_id = 3465;
    });

    // Container with Locked flag fails
    repo.with_mut_snapshot(|s| {
        s.items.get_mut(&100).unwrap().flag = InventoryFlag::Locked;
    });
    assert_eq!(
        repo.split_stack(cid, 101, 10),
        Err(DbError::ItemLocked(101))
    );

    // Container with Impounded flag fails
    repo.with_mut_snapshot(|s| {
        s.items.get_mut(&100).unwrap().flag = InventoryFlag::Impounded;
    });
    assert_eq!(
        repo.split_stack(cid, 101, 10),
        Err(DbError::ItemLocked(101))
    );

    // Container locked in contract fails
    repo.with_mut_snapshot(|s| {
        s.items.get_mut(&100).unwrap().flag = InventoryFlag::Hangar;
        s.contracts.insert(5, ContractRecord {
            contract_id: 5, issuer_id: cid, status: "InProgress".into(), item_ids: vec![100], ..Default::default()
        });
    });
    assert_eq!(repo.split_stack(cid, 101, 10), Err(DbError::ItemLocked(101)));
    repo.with_mut_snapshot(|s| s.contracts.clear());

    // Item itself with type 3468 fails
    repo.with_mut_snapshot(|s| s.items.get_mut(&101).unwrap().type_id = 3468);
    assert_eq!(repo.split_stack(cid, 101, 10), Err(DbError::ItemLocked(101)));
    repo.with_mut_snapshot(|s| s.items.get_mut(&101).unwrap().type_id = 34);

    // Item itself with is_blueprint_copy, dynamic attrs, or mutaplasmid fails with ValidationError
    repo.with_mut_snapshot(|s| s.items.get_mut(&101).unwrap().is_blueprint_copy = true);
    assert!(matches!(repo.split_stack(cid, 101, 10), Err(DbError::ValidationError(_))));
    repo.with_mut_snapshot(|s| {
        let it = s.items.get_mut(&101).unwrap();
        it.is_blueprint_copy = false;
        it.dynamic_attributes.insert(1, 1.0);
    });
    assert!(matches!(repo.split_stack(cid, 101, 10), Err(DbError::ValidationError(_))));
    repo.with_mut_snapshot(|s| {
        let it = s.items.get_mut(&101).unwrap();
        it.dynamic_attributes.clear();
        it.source_mutaplasmid_id = Some(47841);
    });
    assert!(matches!(repo.split_stack(cid, 101, 10), Err(DbError::ValidationError(_))));
    repo.with_mut_snapshot(|s| s.items.get_mut(&101).unwrap().source_mutaplasmid_id = None);

    // Valid split succeeds cleanly
    let res = repo.split_stack(cid, 101, 10);
    assert!(res.is_ok());
    assert_eq!(repo.read_state().items[&101].quantity, 90);
}

#[test]
fn test_verify_quasar_bearer_jwt_and_dev_token_validations() {
    let repo = WorldRepository::with_demo_seed();
    let now = 1_700_000_000_u64;
    let uid = 701_u64;
    let cid = 801_i64;
    let peer_ip: IpAddr = "127.0.0.1".parse().unwrap();
    let session_id = 8888_u64;

    let mut ch = repo
        .read_state()
        .characters
        .values()
        .next()
        .unwrap()
        .clone();
    ch.character_id = cid;
    ch.user_id = uid;
    ch.name = "DevPilot".into();

    repo.with_mut_snapshot(|s| {
        s.accounts.insert(
            "dev_user".into(),
            AccountRecord {
                user_id: uid,
                username: "dev_user".into(),
                password_token: "hash".into(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        s.characters.insert(cid, ch);
    });

    // Set up online session
    let _sess = repo
        .acquire_online_lock_with_computer_hash(uid, session_id, Some(cid), None, false, now)
        .expect("register session");

    // set_online_session_peer_ip verifies os.session_id == session_id
    repo.set_online_session_peer_ip(uid, session_id + 1, "127.0.0.1");
    let rt = repo.auth_runtime.read().unwrap();
    assert_eq!(rt.online_session_ips.get(&uid), None);
    drop(rt);

    repo.set_online_session_peer_ip(uid, session_id, "127.0.0.1");
    let rt = repo.auth_runtime.read().unwrap();
    assert!(rt.online_session_ips.get(&uid).is_some());
    drop(rt);

    // 1. Quasar SSO token with character_id == 0 and user_id > 0 returns user token
    let sso_secret = repo.auth_runtime.read().unwrap().jwt_signing_secret;
    let claims_user = SsoTokenClaims {
        sub: format!("USER:EVE:{uid}"),
        name: "dev_user".into(),
        owner: "owner".into(),
        iss: "https://login.eveonline.com".into(),
        aud: vec!["EVE Online".into()],
        character_id: 0,
        user_id: uid,
        scp: vec!["publicData".into()],
        exp: now + 3600,
        iat: now,
        jti: "jti-1".into(),
    };
    let h = json!({"alg": "HS256", "typ": "JWT"});
    let p_user = serde_json::to_value(&claims_user).unwrap();
    let sso_user_jwt = {
        let h_b64 = base64url_encode_no_pad(&serde_json::to_vec(&h).unwrap());
        let p_b64 = base64url_encode_no_pad(&serde_json::to_vec(&p_user).unwrap());
        let input = format!("{h_b64}.{p_b64}");
        let sig_b64 = base64url_encode_no_pad(&hmac_sha256(&sso_secret, input.as_bytes()));
        format!("{input}.{sig_b64}")
    };

    assert_eq!(
        repo.verify_quasar_bearer_jwt(&sso_user_jwt, now),
        Ok(QuasarVerifiedToken {
            user_id: uid,
            character_id: None,
            is_full_sso: true,
        })
    );

    // 2. Dev client JWT validations
    // Empty segments fail
    assert_eq!(
        repo.verify_local_dev_client_jwt(".payload.sig", peer_ip, now),
        Err(DbError::InvalidCredentials)
    );
    assert_eq!(
        repo.verify_local_dev_client_jwt("header..sig", peer_ip, now),
        Err(DbError::InvalidCredentials)
    );
    assert_eq!(
        repo.verify_local_dev_client_jwt("header.payload.", peer_ip, now),
        Err(DbError::InvalidCredentials)
    );

    // Unsupported alg or crit header
    let bad_alg_h = json!({"alg": "RS256"});
    let bad_alg_jwt = sign_dev_jwt(&bad_alg_h, &json!({"sub": format!("CHARACTER:EVE:{cid}")}));
    assert_eq!(
        repo.verify_local_dev_client_jwt(&bad_alg_jwt, peer_ip, now),
        Err(DbError::InvalidCredentials)
    );

    let crit_h = json!({"alg": "HS256", "crit": ["b64"]});
    let crit_jwt = sign_dev_jwt(&crit_h, &json!({"sub": format!("CHARACTER:EVE:{cid}")}));
    assert_eq!(
        repo.verify_local_dev_client_jwt(&crit_jwt, peer_ip, now),
        Err(DbError::InvalidCredentials)
    );

    // Expired or boundary exp
    let expired_payload = json!({
        "sub": format!("CHARACTER:EVE:{cid}"),
        "exp": now, // now_unix >= exp
        "jti": "jti-exp",
    });
    let exp_jwt = sign_dev_jwt(&h, &expired_payload);
    assert_eq!(
        repo.verify_local_dev_client_jwt(&exp_jwt, peer_ip, now),
        Err(DbError::InvalidCredentials)
    );

    // nbf within 60s clock-skew leeway (including float JSON timestamp) succeeds; beyond 60s fails
    let nbf_leeway_jwt = sign_dev_jwt(
        &h,
        &json!({
            "sub": format!("CHARACTER:EVE:{cid}"),
            "exp": now + 120,
            "nbf": (now as f64) + 59.5,
            "jti": "jti-nbf-leeway",
        }),
    );
    assert!(repo
        .verify_local_dev_client_jwt(&nbf_leeway_jwt, peer_ip, now)
        .is_ok());

    let nbf_payload = json!({
        "sub": format!("CHARACTER:EVE:{cid}"),
        "exp": now + 120,
        "nbf": now + 61,
        "jti": "jti-nbf",
    });
    let nbf_jwt = sign_dev_jwt(&h, &nbf_payload);
    assert_eq!(
        repo.verify_local_dev_client_jwt(&nbf_jwt, peer_ip, now),
        Err(DbError::InvalidCredentials)
    );

    // iat within 60s clock-skew leeway (including float JSON timestamp) succeeds; beyond 60s fails
    let iat_leeway_jwt = sign_dev_jwt(
        &h,
        &json!({
            "sub": format!("CHARACTER:EVE:{cid}"),
            "exp": now + 120,
            "iat": (now as f64) + 59.5,
            "jti": "jti-iat-leeway",
        }),
    );
    assert!(repo
        .verify_local_dev_client_jwt(&iat_leeway_jwt, peer_ip, now)
        .is_ok());

    let iat_payload = json!({
        "sub": format!("CHARACTER:EVE:{cid}"),
        "exp": now + 120,
        "iat": now + 61,
        "jti": "jti-iat",
    });
    let iat_jwt = sign_dev_jwt(&h, &iat_payload);
    assert_eq!(
        repo.verify_local_dev_client_jwt(&iat_jwt, peer_ip, now),
        Err(DbError::InvalidCredentials)
    );

    let negative_iat_jwt = sign_dev_jwt(
        &h,
        &json!({
            "sub": format!("CHARACTER:EVE:{cid}"),
            "exp": now + 120,
            "iat": -1.5,
            "jti": "jti-neg-iat",
        }),
    );
    assert_eq!(
        repo.verify_local_dev_client_jwt(&negative_iat_jwt, peer_ip, now),
        Err(DbError::InvalidCredentials)
    );

    // Empty jti string
    let empty_jti = json!({
        "sub": format!("CHARACTER:EVE:{cid}"),
        "exp": now + 60,
        "jti": "",
    });
    let empty_jti_jwt = sign_dev_jwt(&h, &empty_jti);
    assert_eq!(
        repo.verify_local_dev_client_jwt(&empty_jti_jwt, peer_ip, now),
        Err(DbError::InvalidCredentials)
    );

    // Claimed uid mismatch
    let uid_mismatch = json!({
        "sub": format!("CHARACTER:EVE:{cid}"),
        "exp": now + 60,
        "jti": "jti-valid",
        "uid": uid + 1,
    });
    let uid_mismatch_jwt = sign_dev_jwt(&h, &uid_mismatch);
    assert_eq!(
        repo.verify_local_dev_client_jwt(&uid_mismatch_jwt, peer_ip, now),
        Err(DbError::InvalidCredentials)
    );

    // Valid dev client JWT succeeds
    let valid_payload = json!({
        "sub": format!("CHARACTER:EVE:{cid}"),
        "exp": now + 60,
        "iat": now,
        "jti": "jti-valid",
        "uid": uid,
        "cid": cid,
    });
    let valid_jwt = sign_dev_jwt(&h, &valid_payload);
    assert_eq!(
        repo.verify_local_dev_client_jwt(&valid_jwt, peer_ip, now),
        Ok(QuasarVerifiedToken {
            user_id: uid,
            character_id: Some(cid),
            is_full_sso: false,
        })
    );
}

#[test]
fn test_authorize_pkce_character_bounds() {
    let repo = WorldRepository::with_demo_seed();
    let now = 1_700_000_000_u64;

    repo.with_mut_snapshot(|s| {
        s.accounts.insert(
            "pkce_user".into(),
            AccountRecord {
                user_id: 999,
                username: "pkce_user".into(),
                password_token: "hash".into(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });

    // character_id < 0 fails with ValidationError
    assert_eq!(
        repo.authorize_pkce(
            "pkce_user",
            "hash",
            -1,
            "client_1",
            "https://redirect",
            vec!["publicData".into()],
            "challenge_string_1234567890",
            "S256",
            now,
        ),
        Err(DbError::ValidationError("Invalid character_id".into()))
    );
}

#[test]
fn test_auth_runtime_state_persists_and_prunes_across_sqlite_restart() {
    use crate::accounts::{base64url_encode_no_pad, sha256_bytes};

    let pid = std::process::id();
    let db_path = std::env::temp_dir().join(format!("evomu_540_auth_rt_{pid}.sqlite"));
    let copy_path = std::env::temp_dir().join(format!("evomu_540_auth_rt_copy_{pid}.sqlite"));
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&copy_path);

    let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");
    let now = 1_750_000_000u64;
    let uid_alice = repo.provision_player_account("alice_persist", "pw-alice");
    let uid_bob = repo.provision_player_account("bob_persist", "pw-bob");
    let cid_alice = 91_100_001i64;
    let cid_bob = 91_100_002i64;
    let template_char =
        WorldRepository::with_demo_seed().with_snapshot(|s| s.characters[&90_000_001].clone());
    repo.with_mut_snapshot(|snap| {
        let mut ch1 = template_char.clone();
        ch1.character_id = cid_alice;
        ch1.user_id = uid_alice;
        ch1.name = "Alice Persist".into();
        snap.characters.insert(cid_alice, ch1);

        let mut ch2 = template_char;
        ch2.character_id = cid_bob;
        ch2.user_id = uid_bob;
        ch2.name = "Bob Persist".into();
        snap.characters.insert(cid_bob, ch2);
    });

    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = base64url_encode_no_pad(&sha256_bytes(verifier.as_bytes()));

    // Alice gets two token bundles: bundle_revoked is revoked; bundle_active stays valid for 30 days.
    let code_a1 = repo
        .authorize_pkce(
            "alice_persist",
            "pw-alice",
            cid_alice,
            "evomu-launcher",
            "https://localhost/cb",
            vec!["publicData".into()],
            &challenge,
            "S256",
            now,
        )
        .unwrap();
    let bundle_revoked = repo
        .exchange_pkce_code(&code_a1, "evomu-launcher", verifier, now + 1)
        .unwrap();
    assert!(repo.revoke_sso_refresh_token(&bundle_revoked.access_token));

    let code_a2 = repo
        .authorize_pkce(
            "alice_persist",
            "pw-alice",
            cid_alice,
            "evomu-launcher",
            "https://localhost/cb",
            vec!["publicData".into()],
            &challenge,
            "S256",
            now + 2,
        )
        .unwrap();
    let bundle_active = repo
        .exchange_pkce_code(&code_a2, "evomu-launcher", verifier, now + 3)
        .unwrap();

    // Also mint + revoke an older token whose exp is already expired relative to `now + 2_000`.
    let code_exp = repo
        .authorize_pkce(
            "alice_persist",
            "pw-alice",
            cid_alice,
            "evomu-launcher",
            "https://localhost/cb",
            vec!["publicData".into()],
            &challenge,
            "S256",
            now - 2_000,
        )
        .unwrap();
    let bundle_expired = repo
        .exchange_pkce_code(&code_exp, "evomu-launcher", verifier, now - 1_999)
        .unwrap();
    assert!(repo.revoke_sso_refresh_token(&bundle_expired.access_token));
    let expired_jti = repo
        .auth_runtime
        .read()
        .unwrap()
        .revoked_jti_expiries
        .iter()
        .find_map(|(j, &exp)| (exp == now - 1_999 + 1_200).then(|| j.clone()))
        .expect("expired jti recorded before prune");

    // Bob gets a token, then Bob is kicked.
    let code_b = repo
        .authorize_pkce(
            "bob_persist",
            "pw-bob",
            cid_bob,
            "evomu-launcher",
            "https://localhost/cb",
            vec!["publicData".into()],
            &challenge,
            "S256",
            now + 4,
        )
        .unwrap();
    let bob_bundle = repo
        .exchange_pkce_code(&code_b, "evomu-launcher", verifier, now + 5)
        .unwrap();
    repo.kick_user(uid_bob, "restart-kick", now + 10);

    // Ensure plaintext refresh token is NEVER stored in SQLite (only its SHA-256 digest).
    {
        let conn = repo.sqlite_conn.lock().unwrap();
        let stored_keys: Vec<String> = conn
            .prepare("SELECT token_hash FROM auth_refresh_tokens")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(!stored_keys.is_empty());
        assert!(!stored_keys.contains(&bundle_active.refresh_token));
        assert!(stored_keys.contains(&crate::sqlite_store::refresh_token_digest(
            &bundle_active.refresh_token
        )));
    }

    // Save to another SQLite path and reload from disk.
    repo.save_to_sqlite(&copy_path).expect("save_to_sqlite");
    drop(repo);

    // Verify both direct reopen of `db_path` and `copy_path` restore full AuthRuntimeState.
    for reload_path in [&copy_path, &db_path] {
        let reloaded = WorldRepository::open_sqlite(reload_path).expect("open_sqlite reload");

        // Expired revoked JTI was pruned on mint/kick/reload (`exp <= now + 10`), while active revoked JTI stays!
        assert!(
            !reloaded
                .auth_runtime
                .read()
                .unwrap()
                .revoked_jtis
                .contains(&expired_jti),
            "expired revoked JTI must be pruned"
        );
        assert_eq!(
            reloaded.verify_sso_access_token(&bundle_revoked.access_token, now + 20),
            Err(DbError::InvalidCredentials),
            "revoked JWT must stay rejected across restart"
        );
        assert_eq!(
            reloaded.verify_sso_access_token(&bob_bundle.access_token, now + 20),
            Err(DbError::InvalidCredentials),
            "kicked user JWT must stay rejected across restart"
        );
        assert_eq!(
            reloaded.kicked_reason(uid_bob).as_deref(),
            Some("restart-kick")
        );
        assert_eq!(
            reloaded.refresh_sso_token(&bob_bundle.refresh_token, "evomu-launcher", now + 20),
            Err(DbError::InvalidCredentials),
            "kicked user's refresh token must stay revoked across restart"
        );

        // Active 30-day refresh token survives restart and rotates cleanly.
        let rotated = reloaded
            .refresh_sso_token(&bundle_active.refresh_token, "evomu-launcher", now + 86_400)
            .expect("active refresh token must remain valid across restart");
        assert!(reloaded
            .verify_sso_access_token(&rotated.access_token, now + 86_405)
            .is_ok());
    }
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&copy_path);
}
