//! Semantic unit tests split from `sso_tests`.

#![forbid(unsafe_code)]

use super::*;

use eve_db::VirtualServerClock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

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

    // Account with zero characters -> 302 account-scoped USER:EVE:<uid> authorization (#472)
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
    assert_eq!(no_char_resp.status_code, 302);

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
        format!("grant_type=authorization_code&client_id=eve-launcher&code={code}&code_verifier={verifier}&redirect_uri=eveauth%3A%2F%2Fcallback%2F%3Fflow%3Dv24").as_bytes(),
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
            "code_verifier": verifier,
            "redirect_uri": "eveauth://callback/?flow=v24"
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

#[tokio::test]
async fn test_issue_427_slowloris_timeout_and_oauth_redirect_uri_binding() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::{TcpListener, TcpStream};

    let ctx = Arc::new(ServerContext::new());

    // 1. Stalled Slowloris TCP header read terminates promptly at read_timeout instead of hanging
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (accept_res, cli_res) = tokio::join!(listener.accept(), TcpStream::connect(addr));
    let (srv_stream, _) = accept_res.unwrap();
    let mut cli_stream = cli_res.unwrap();
    cli_stream
        .write_all(b"GET /v2/oauth/authorize HTTP/1.1\r\nHost: 127.0.0.1\r\n")
        .await
        .unwrap();
    let timed = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        handle_http_connection_with_timeout(
            srv_stream,
            ctx.clone(),
            std::time::Duration::from_millis(40),
        ),
    )
    .await;
    assert!(timed.is_ok(), "Slowloris connection must terminate within timeout");

    // 2. OAuth2 token exchange rejects mismatched redirect_uri and accepts exact match
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = pkce_challenge_s256(verifier);
    let bound_redir = "http://127.0.0.1:8080/callback";
    let hdrs = HashMap::from([(
        "content-type".to_string(),
        "application/x-www-form-urlencoded".to_string(),
    )]);
    let code1 = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            90_000_001,
            "eve-launcher",
            bound_redir,
            vec![],
            &challenge,
            "S256",
            ctx.now_unix(),
        )
        .unwrap();
    let bad_tok = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &hdrs,
        format!("grant_type=authorization_code&client_id=eve-launcher&code={code1}&code_verifier={verifier}&redirect_uri=http%3A%2F%2F127.0.0.1%3A9999%2Fsteal").as_bytes(),
    );
    assert_eq!(bad_tok.status_code, 400);
    let code2 = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            90_000_001,
            "eve-launcher",
            bound_redir,
            vec![],
            &challenge,
            "S256",
            ctx.now_unix(),
        )
        .unwrap();
    let good_tok = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &hdrs,
        format!("grant_type=authorization_code&client_id=eve-launcher&code={code2}&code_verifier={verifier}&redirect_uri=http%3A%2F%2F127.0.0.1%3A8080%2Fcallback").as_bytes(),
    );
    assert_eq!(good_tok.status_code, 200);
}
