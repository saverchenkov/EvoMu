//! Unit tests for EVE Online SSO / OAuth2 + PKCE (`S256`) endpoints, Bearer JWT verification,
//! token rotation/revocation, and HTTP/1.1 connection framing (`sso`).

#![forbid(unsafe_code)]

use super::*;
use eve_db::VirtualServerClock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn sha256_digest(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, delta) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(delta);
        }
    }
    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn pkce_challenge_s256(verifier: &str) -> String {
    eve_db::base64url_encode_no_pad(&sha256_digest(verifier.as_bytes()))
}

fn encode_base64_std(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[test]
fn test_sso_discovery_jwks_consent_html_and_helpers() {
    let mut ctx = ServerContext::new();
    ctx.esi_port = 26_002;
    let empty_hdrs = HashMap::new();

    // Discovery metadata with and without Host header
    let disc_no_host = handle_http_request(
        &ctx,
        "GET",
        "/.well-known/oauth-authorization-server",
        &empty_hdrs,
        b"",
    );
    assert_eq!(disc_no_host.status_code, 200);
    let disc_json: serde_json::Value = serde_json::from_slice(&disc_no_host.body).unwrap();
    assert_eq!(
        disc_json["authorization_endpoint"],
        json!("http://127.0.0.1:26002/v2/oauth/authorize")
    );
    assert_eq!(disc_json["code_challenge_methods_supported"], json!(["S256"]));

    let mut host_hdrs = HashMap::new();
    host_hdrs.insert("host".to_string(), "sso.evomu.local:26002".to_string());
    let disc_host = handle_http_request(
        &ctx,
        "GET",
        "/.well-known/openid-configuration",
        &host_hdrs,
        b"",
    );
    let disc_host_json: serde_json::Value = serde_json::from_slice(&disc_host.body).unwrap();
    assert_eq!(
        disc_host_json["token_endpoint"],
        json!("http://sso.evomu.local:26002/v2/oauth/token")
    );

    // JWKS endpoint
    let jwks_resp = handle_http_request(&ctx, "GET", "/oauth/jwks", &empty_hdrs, b"");
    assert_eq!(jwks_resp.status_code, 200);
    let jwks_json: serde_json::Value = serde_json::from_slice(&jwks_resp.body).unwrap();
    assert_eq!(jwks_json["keys"][0]["kid"], json!(ctx.db.jwt_key_id()));
    assert_eq!(jwks_json["keys"][0]["alg"], json!("HS256"));

    // Interactive GET /v2/oauth/authorize HTML consent form escapes special HTML characters
    let authorize_qs = "/v2/oauth/authorize?client_id=eve%3Cclient%3E&redirect_uri=http%3A%2F%2F127.0.0.1%3A8080%2Fcb&code_challenge=abc123&scope=esi-location.read_location.v1&state=s%221%27%262";
    let html_resp = handle_http_request(&ctx, "GET", authorize_qs, &empty_hdrs, b"");
    assert_eq!(html_resp.status_code, 200);
    assert!(html_resp.content_type.starts_with("text/html"));
    let html_str = String::from_utf8(html_resp.body).unwrap();
    assert!(html_str.contains("eve&lt;client&gt;"));
    assert!(html_str.contains("s&quot;1&#x27;&amp;2"));

    // Wire serialization strips CRLF from injected header values
    let mut custom_resp = HttpResponse::json(200, "OK", json!({"ok": true}));
    custom_resp
        .headers
        .push(("X-Test\r\nEvil".to_string(), "val\r\nInjected".to_string()));
    let wire = String::from_utf8_lossy(&custom_resp.to_wire_bytes()).into_owned();
    assert!(wire.contains("X-TestEvil: valInjected\r\n"));

    // Helpers coverage
    assert_eq!(format_unix_iso8601(0), "1970-01-01T00:00:00");
    assert_eq!(format_unix_iso8601(1_700_000_000), "2023-11-14T22:13:20");
    assert!(base64_decode_standard("!!!").is_none());
    assert_eq!(percent_decode("a+b%2Fc%ZZ"), "a b/c%ZZ");
    assert!(is_valid_redirect_uri("eveauth://callback"));
    assert!(is_valid_redirect_uri("https://app.example.com/cb"));
    assert!(!is_valid_redirect_uri("http://untrusted.example.com/cb"));
    assert!(!is_valid_redirect_uri("https://app.example.com/\ncb"));

    // Bugs delegation & 404 fallback
    let bugs_status = handle_http_request(&ctx, "GET", "/bugs/api/status", &empty_hdrs, b"");
    assert_eq!(bugs_status.status_code, 200);
    let not_found = handle_http_request(&ctx, "DELETE", "/v2/unknown", &empty_hdrs, b"");
    assert_eq!(not_found.status_code, 404);
}

#[test]
fn test_sso_pkce_full_lifecycle_validation_and_revocation() {
    let ctx = ServerContext::new();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = pkce_challenge_s256(verifier);

    // 1. Validation failures on /v2/oauth/authorize
    let mut json_hdrs = HashMap::new();
    json_hdrs.insert("content-type".to_string(), "application/json".to_string());

    let bad_rt = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &json_hdrs,
        br#"{"response_type":"token","client_id":"c1","redirect_uri":"http://127.0.0.1/cb","code_challenge":"x"}"#,
    );
    assert_eq!(bad_rt.status_code, 400);

    let bad_redir = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &json_hdrs,
        br#"{"client_id":"c1","redirect_uri":"http://evil.com/cb","code_challenge":"x"}"#,
    );
    assert_eq!(bad_redir.status_code, 400);

    let bad_method = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &json_hdrs,
        br#"{"client_id":"c1","redirect_uri":"http://127.0.0.1/cb","code_challenge":"x","code_challenge_method":"plain"}"#,
    );
    assert_eq!(bad_method.status_code, 400);

    let bad_creds = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &json_hdrs,
        format!(
            r#"{{"client_id":"c1","redirect_uri":"http://127.0.0.1/cb","code_challenge":"{challenge}","username":"demo","password":"wrong"}}"#
        )
        .as_bytes(),
    );
    assert_eq!(bad_creds.status_code, 401);

    // Account with zero characters -> 400
    let _no_char_uid = ctx.db.provision_player_account("empty_pilot", "pass1234");
    let no_char_resp = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &json_hdrs,
        format!(
            r#"{{"client_id":"c1","redirect_uri":"http://127.0.0.1/cb","code_challenge":"{challenge}","username":"empty_pilot","password":"pass1234"}}"#
        )
        .as_bytes(),
    );
    assert_eq!(no_char_resp.status_code, 400);

    // 2. Successful PKCE S256 authorization via JSON body + Basic Auth + array scopes + state
    let mut auth_hdrs = json_hdrs.clone();
    auth_hdrs.insert(
        "authorization".to_string(),
        format!("Basic {}", encode_base64_std(b"demo:demo-secret")),
    );
    let auth_body = serde_json::to_vec(&json!({
        "client_id": "eve-launcher",
        "redirect_uri": "eveauth://callback/?flow=v24",
        "code_challenge": challenge,
        "code_challenge_method": "S256",
        "scope": ["esi-location.read_location.v1", "esi-skills.read_skills.v1"],
        "state": "xyz 123"
    }))
    .unwrap();
    let auth_resp = handle_http_request(&ctx, "POST", "/v2/oauth/authorize", &auth_hdrs, &auth_body);
    assert_eq!(auth_resp.status_code, 302);
    let loc_hdr = auth_resp
        .headers
        .iter()
        .find(|(k, _)| k == "Location")
        .map(|(_, v)| v.as_str())
        .unwrap();
    assert!(loc_hdr.starts_with("eveauth://callback/?flow=v24&code="));
    assert!(loc_hdr.ends_with("&state=xyz%20123"));
    let auth_json: serde_json::Value = serde_json::from_slice(&auth_resp.body).unwrap();
    let code = auth_json["code"].as_str().unwrap().to_string();

    // 3. Token exchange error cases + valid exchange (failed PKCE verifier consumes its one-time code)
    let bad_code_resp = handle_http_request(&ctx, "POST", "/v2/oauth/authorize", &auth_hdrs, &auth_body);
    let bad_code = serde_json::from_slice::<serde_json::Value>(&bad_code_resp.body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();

    let mut form_hdrs = HashMap::new();
    form_hdrs.insert(
        "content-type".to_string(),
        "application/x-www-form-urlencoded".to_string(),
    );
    let wrong_ver = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &form_hdrs,
        format!("grant_type=authorization_code&client_id=eve-launcher&code={bad_code}&code_verifier=wrong_verifier").as_bytes(),
    );
    assert_eq!(wrong_ver.status_code, 400);

    let bad_grant = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &form_hdrs,
        b"grant_type=client_credentials&client_id=eve-launcher",
    );
    assert_eq!(bad_grant.status_code, 400);

    let tok_resp = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &form_hdrs,
        format!("grant_type=authorization_code&client_id=eve-launcher&code={code}&code_verifier={verifier}").as_bytes(),
    );
    assert_eq!(tok_resp.status_code, 200);
    let tok_json: serde_json::Value = serde_json::from_slice(&tok_resp.body).unwrap();
    let access_token = tok_json["access_token"].as_str().unwrap().to_string();
    let refresh_token = tok_json["refresh_token"].as_str().unwrap().to_string();
    assert_eq!(tok_json["token_type"], json!("Bearer"));

    // 4. Verify Bearer token + scope checks
    let mut bearer_hdrs = HashMap::new();
    bearer_hdrs.insert("authorization".to_string(), format!("Bearer {access_token}"));
    let verify_ok = handle_http_request(&ctx, "GET", "/v2/oauth/verify", &bearer_hdrs, b"");
    assert_eq!(verify_ok.status_code, 200);
    let verify_json: serde_json::Value = serde_json::from_slice(&verify_ok.body).unwrap();
    assert_eq!(verify_json["CharacterID"], json!(90_000_001));
    assert_eq!(verify_json["CharacterName"], json!("Aura Capsuleer"));

    let verify_missing_scope = handle_http_request(
        &ctx,
        "GET",
        "/oauth/verify?scope=esi-wallet.read_character_wallet.v1",
        &bearer_hdrs,
        b"",
    );
    assert_eq!(verify_missing_scope.status_code, 403);
    let scope_err: serde_json::Value = serde_json::from_slice(&verify_missing_scope.body).unwrap();
    assert_eq!(scope_err["error"], json!("insufficient_scope"));

    // 5. Refresh token rotation
    let refresh_resp = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &form_hdrs,
        format!("grant_type=refresh_token&client_id=eve-launcher&refresh_token={refresh_token}")
            .as_bytes(),
    );
    assert_eq!(refresh_resp.status_code, 200);
    let refresh_json: serde_json::Value = serde_json::from_slice(&refresh_resp.body).unwrap();
    let new_access = refresh_json["access_token"].as_str().unwrap().to_string();
    let new_refresh = refresh_json["refresh_token"].as_str().unwrap().to_string();
    assert_ne!(new_refresh, refresh_token);

    // 6. Revoke new refresh token and confirm paired access token is rejected
    let revoke_resp = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/revoke",
        &form_hdrs,
        format!("token={new_refresh}").as_bytes(),
    );
    assert_eq!(revoke_resp.status_code, 200);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&revoke_resp.body).unwrap()["revoked"],
        json!(true)
    );

    let mut revoked_bearer = HashMap::new();
    revoked_bearer.insert("authorization".to_string(), format!("Bearer {new_access}"));
    let verify_revoked = handle_http_request(&ctx, "GET", "/v2/oauth/verify", &revoked_bearer, b"");
    assert_eq!(verify_revoked.status_code, 401);

    // 7. Account suspension & ban enforcement on verify / authorize (non-admin account)
    let pilot_uid = ctx
        .db
        .authenticate("empty_pilot", "pass1234")
        .unwrap()
        .user_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut ch) = snap.characters.get(&90_000_001).cloned() {
            ch.character_id = 90_000_991;
            ch.user_id = pilot_uid;
            ch.name = "Pilot Sso".to_string();
            snap.characters.insert(90_000_991, ch);
        }
    });
    let mut pilot_auth_hdrs = json_hdrs.clone();
    pilot_auth_hdrs.insert(
        "authorization".to_string(),
        format!("Basic {}", encode_base64_std(b"empty_pilot:pass1234")),
    );
    let pilot_auth = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &pilot_auth_hdrs,
        &auth_body,
    );
    assert_eq!(pilot_auth.status_code, 302);
    let pilot_auth_json: serde_json::Value = serde_json::from_slice(&pilot_auth.body).unwrap();
    let pilot_code = pilot_auth_json["code"].as_str().unwrap().to_string();
    let pilot_tok = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &json_hdrs,
        serde_json::to_vec(&json!({
            "grant_type": "authorization_code",
            "code": pilot_code,
            "client_id": "eve-launcher",
            "code_verifier": verifier
        }))
        .unwrap()
        .as_slice(),
    );
    let pilot_tok_json: serde_json::Value = serde_json::from_slice(&pilot_tok.body).unwrap();
    let pilot_access = pilot_tok_json["access_token"].as_str().unwrap().to_string();
    let pilot_refresh = pilot_tok_json["refresh_token"].as_str().unwrap().to_string();
    let mut pilot_bearer = HashMap::new();
    pilot_bearer.insert("authorization".to_string(), format!("Bearer {pilot_access}"));

    ctx.db
        .suspend_account("empty_pilot", 1_800_003_600, "Temp suspension test")
        .unwrap();
    let susp_verify = handle_http_request(&ctx, "GET", "/v2/oauth/verify", &pilot_bearer, b"");
    assert_eq!(susp_verify.status_code, 401);
    let susp_auth = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &pilot_auth_hdrs,
        &auth_body,
    );
    assert_eq!(susp_auth.status_code, 403);
    // suspend_account already revokes active refresh tokens via kick_user -> invalid_grant (400)
    let susp_ref = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &json_hdrs,
        serde_json::to_vec(&json!({
            "grant_type": "refresh_token",
            "refresh_token": pilot_refresh,
            "client_id": "eve-launcher"
        }))
        .unwrap()
        .as_slice(),
    );
    assert_eq!(susp_ref.status_code, 400);

    ctx.db
        .ban_account("empty_pilot", 1_900_000_000, "Permanent ban test")
        .unwrap();
    let ban_verify = handle_http_request(&ctx, "GET", "/v2/oauth/verify", &pilot_bearer, b"");
    assert_eq!(ban_verify.status_code, 401);
    let ban_auth = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &pilot_auth_hdrs,
        &auth_body,
    );
    assert_eq!(ban_auth.status_code, 403);
}

#[tokio::test]
async fn test_sso_tcp_http_connection_framing_and_limits() {
    let ctx = Arc::new(ServerContext::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // 1. Standard GET + Expect: 100-continue POST + oversized Content-Length over real TCP
    let srv_ctx = Arc::clone(&ctx);
    let server_task = tokio::spawn(async move {
        for _ in 0..3 {
            let (stream, _) = listener.accept().await.unwrap();
            let c = Arc::clone(&srv_ctx);
            tokio::spawn(async move {
                let _ = handle_http_connection(stream, c).await;
            });
        }
    });

    // Request 1: GET /oauth/jwks
    let mut s1 = TcpStream::connect(addr).await.unwrap();
    s1.write_all(b"GET /oauth/jwks HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let mut r1 = Vec::new();
    s1.read_to_end(&mut r1).await.unwrap();
    let r1_str = String::from_utf8_lossy(&r1);
    assert!(r1_str.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(r1_str.contains("\"alg\":\"HS256\""));

    // Request 2: POST /v2/oauth/revoke with Expect: 100-continue
    let mut s2 = TcpStream::connect(addr).await.unwrap();
    let body2 = b"token=nonexistent-token";
    let hdr2 = format!(
        "POST /v2/oauth/revoke HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/x-www-form-urlencoded\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n",
        body2.len()
    );
    s2.write_all(hdr2.as_bytes()).await.unwrap();
    let mut cont_buf = [0u8; 64];
    let n = s2.read(&mut cont_buf).await.unwrap();
    assert!(String::from_utf8_lossy(&cont_buf[..n]).starts_with("HTTP/1.1 100 Continue"));
    s2.write_all(body2).await.unwrap();
    let mut r2 = Vec::new();
    s2.read_to_end(&mut r2).await.unwrap();
    let r2_str = String::from_utf8_lossy(&r2);
    assert!(r2_str.contains("HTTP/1.1 200 OK"));
    assert!(r2_str.contains("\"revoked\":false"));

    // Request 3: Oversized Content-Length -> 413 Payload Too Large
    let mut s3 = TcpStream::connect(addr).await.unwrap();
    s3.write_all(b"POST /v2/oauth/token HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 99999999\r\n\r\n")
        .await
        .unwrap();
    let mut r3 = Vec::new();
    s3.read_to_end(&mut r3).await.unwrap();
    assert!(String::from_utf8_lossy(&r3).starts_with("HTTP/1.1 413 Payload Too Large"));

    server_task.await.unwrap();
}
