//! Tests for PKCE authorization, exchange, token refresh, and SSO claims verification.

#![cfg(test)]

use crate::accounts::{base64url_encode_no_pad, sha256_bytes};
use crate::persistence::WorldRepository;
use crate::types::*;

#[test]
fn test_pkce_full_lifecycle_and_edge_branches() {
    let repo = WorldRepository::with_demo_seed();
    let now = 1_700_000_000_u64;
    let uid = 8801_u64;
    let cid = 9901_i64;

    let mut ch = repo
        .read_state()
        .characters
        .values()
        .next()
        .unwrap()
        .clone();
    ch.character_id = cid;
    ch.user_id = uid;
    ch.name = "PkcePilot".into();

    repo.with_mut_snapshot(|s| {
        s.accounts.insert(
            "pkce_test_user".into(),
            AccountRecord {
                user_id: uid,
                username: "pkce_test_user".into(),
                password_token: WorldRepository::hash_password_token("pkce_test_user", "pkce_pass"),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        s.characters.insert(cid, ch);
    });

    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = base64url_encode_no_pad(&sha256_bytes(verifier.as_bytes()));

    // 1. Authorize PKCE with character_id == 0 (account level) succeeds
    let code_user = repo
        .authorize_pkce(
            "pkce_test_user",
            "pkce_pass",
            0,
            "client_app",
            "https://callback",
            vec!["publicData".into()],
            &challenge,
            "S256",
            now,
        )
        .expect("authorize user level");

    // 2. Authorize PKCE with character_id > 0 succeeds
    let code_char = repo
        .authorize_pkce(
            "pkce_test_user",
            "pkce_pass",
            cid,
            "client_app",
            "https://callback",
            vec!["publicData".into()],
            &challenge,
            "S256",
            now,
        )
        .expect("authorize char level");

    // 3. Exchange user-level code: verifies user-level subject and mints tokens
    let bundle_user = repo
        .exchange_pkce_code_with_redirect(
            &code_user,
            "client_app",
            verifier,
            Some("https://callback"),
            now,
        )
        .expect("exchange user code");
    let verified_user = repo
        .verify_sso_access_token(&bundle_user.access_token, now)
        .expect("verify user token");
    assert_eq!(verified_user.character_id, 0);
    assert_eq!(verified_user.sub, format!("USER:EVE:{uid}"));

    // User-level refresh token rotates cleanly
    let refreshed_user = repo
        .refresh_sso_token(&bundle_user.refresh_token, "client_app", now + 10)
        .expect("refresh user token");
    let verified_user_refreshed = repo
        .verify_sso_access_token(&refreshed_user.access_token, now + 10)
        .expect("verify refreshed user token");
    assert_eq!(verified_user_refreshed.character_id, 0);

    // 4. Exchange character-level code
    // Enforce redirect mismatch fails
    let code_char_copy = code_char.clone();
    assert_eq!(
        repo.exchange_pkce_code_with_redirect(
            &code_char_copy,
            "client_app",
            verifier,
            Some("https://wrong_callback"),
            now,
        ),
        Err(DbError::InvalidCredentials)
    );

    // Re-authorize since remove(code) consumes the code
    let code_char2 = repo
        .authorize_pkce(
            "pkce_test_user",
            "pkce_pass",
            cid,
            "client_app",
            "https://callback",
            vec!["publicData".into()],
            &challenge,
            "S256",
            now,
        )
        .expect("reauthorize char");

    // Client mismatch fails
    assert_eq!(
        repo.exchange_pkce_code(
            &code_char2,
            "wrong_client",
            verifier,
            now,
        ),
        Err(DbError::InvalidCredentials)
    );

    // Re-authorize and exchange successfully at exact expiry boundary (now + 300)
    let code_char3 = repo
        .authorize_pkce(
            "pkce_test_user",
            "pkce_pass",
            cid,
            "client_app",
            "https://callback",
            vec!["publicData".into()],
            &challenge,
            "S256",
            now,
        )
        .unwrap();

    let bundle_char = repo
        .exchange_pkce_code_with_redirect(
            &code_char3,
            "client_app",
            verifier,
            Some("https://callback"),
            now + 300, // exact boundary now_unix <= expires_at_unix
        )
        .expect("exchange at boundary");

    let verified_char = repo
        .verify_sso_access_token(&bundle_char.access_token, now + 300)
        .expect("verify char token");
    assert_eq!(verified_char.character_id, cid);
    assert_eq!(verified_char.sub, format!("CHARACTER:EVE:{cid}"));

    // 5. Refresh token rotation: exact expiry boundary (now + 86400 * 30)
    let max_refresh_exp = now + 300 + 86_400 * 30;
    let refreshed_char = repo
        .refresh_sso_token(&bundle_char.refresh_token, "client_app", max_refresh_exp)
        .expect("refresh at boundary");
    let verified_char_refreshed = repo
        .verify_sso_access_token(&refreshed_char.access_token, max_refresh_exp)
        .expect("verify refreshed char token");
    assert_eq!(verified_char_refreshed.character_id, cid);

    // Using old refresh token fails (it was revoked)
    assert_eq!(
        repo.refresh_sso_token(&bundle_char.refresh_token, "client_app", max_refresh_exp),
        Err(DbError::InvalidCredentials)
    );

    // Refresh token after expiry boundary fails
    assert_eq!(
        repo.refresh_sso_token(
            &refreshed_char.refresh_token,
            "client_app",
            max_refresh_exp + 86_400 * 30 + 1,
        ),
        Err(DbError::InvalidCredentials)
    );

    // 6. Verify SSO token when user is kicked:
    // If kicked_at >= parsed.iat, token is rejected.
    // If kicked_at < parsed.iat (kicked in the past before token was issued), token is accepted.
    repo.kick_user(uid, "test_gm_kick", now + 300);
    let rt = repo.auth_runtime.read().unwrap();
    let kicked_at = rt.kicked_users[&uid].0;
    drop(rt);

    // Token issued before kick (iat <= kicked_at) fails
    assert_eq!(
        repo.verify_sso_access_token(&refreshed_char.access_token, max_refresh_exp),
        Err(DbError::InvalidCredentials)
    );

    // Fresh token issued strictly AFTER kick succeeds
    let unkicked_now = kicked_at + 100;
    repo.auth_runtime
        .write()
        .unwrap()
        .kicked_users
        .insert(uid, (kicked_at, "old_kick".into()));

    let code_after_kick = repo
        .authorize_pkce(
            "pkce_test_user",
            "pkce_pass",
            cid,
            "client_app",
            "https://callback",
            vec!["publicData".into()],
            &challenge,
            "S256",
            unkicked_now,
        )
        .unwrap();

    let bundle_after_kick = repo
        .exchange_pkce_code_with_redirect(
            &code_after_kick,
            "client_app",
            verifier,
            Some("https://callback"),
            unkicked_now,
        )
        .unwrap();

    assert!(repo
        .verify_sso_access_token(&bundle_after_kick.access_token, unkicked_now)
        .is_ok());
}
