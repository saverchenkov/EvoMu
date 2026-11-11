//! Extended unit tests for `sso` covering remaining error branches, account ban/suspension
//! status codes on active tokens/codes, TCP header limits, and parser edge cases.

#![forbid(unsafe_code)]

use super::*;
use eve_db::VirtualServerClock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn test_pkce_pair() -> (&'static str, String) {
    // RFC 7636 Appendix B test vector:
    // verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
    // S256 challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    (
        "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM".to_string(),
    )
}

#[test]
fn test_sso_authorize_token_revoke_and_verify_missing_params_and_aliases() {
    let ctx = ServerContext::new();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let (verifier, challenge) = test_pkce_pair();
    let empty_hdrs = HashMap::new();

    // 1. /.well-known/jwks.json alias
    let jwks_alias = handle_http_request(&ctx, "GET", "/.well-known/jwks.json", &empty_hdrs, b"");
    assert_eq!(jwks_alias.status_code, 200);

    // 2. /v2/oauth/authorize missing client_id, redirect_uri, code_challenge, username, password
    let no_cid = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        b"redirect_uri=http://127.0.0.1/cb&code_challenge=abc",
    );
    assert_eq!(no_cid.status_code, 400);

    let no_redir = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        b"client_id=c1&code_challenge=abc",
    );
    assert_eq!(no_redir.status_code, 400);

    let no_chal = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        b"client_id=c1&redirect_uri=http://127.0.0.1/cb",
    );
    assert_eq!(no_chal.status_code, 400);

    let no_user = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        format!("client_id=c1&redirect_uri=http://127.0.0.1/cb&code_challenge={challenge}&password=demo-secret").as_bytes(),
    );
    assert_eq!(no_user.status_code, 401);

    let no_pass = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        format!("client_id=c1&redirect_uri=http://127.0.0.1/cb&code_challenge={challenge}&username=demo").as_bytes(),
    );
    assert_eq!(no_pass.status_code, 401);

    // Explicit character_id not owned by user -> 400 Bad Request
    let wrong_char = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        format!("client_id=c1&redirect_uri=http://localhost:8080/cb&code_challenge={challenge}&username=demo&password=demo-secret&character_id=999999999").as_bytes(),
    );
    assert_eq!(wrong_char.status_code, 400);

    // Explicit valid character_id + redirect_uri without '?' and without state -> '?code=...'
    let ok_auth = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        format!("client_id=c1&redirect_uri=http://localhost:8080/cb&code_challenge={challenge}&username=demo&password=demo-secret&character_id=90000001&scope=esi-skills.read_skills.v1").as_bytes(),
    );
    assert_eq!(ok_auth.status_code, 302);
    let loc = ok_auth
        .headers
        .iter()
        .find(|(k, _)| k == "Location")
        .map(|(_, v)| v.as_str())
        .unwrap();
    assert!(loc.starts_with("http://localhost:8080/cb?code="));
    assert!(!loc.contains("state="));
    let code = serde_json::from_slice::<serde_json::Value>(&ok_auth.body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();

    // 3. /oauth/token missing client_id, grant_type, code, code_verifier, refresh_token
    let no_client = handle_http_request(
        &ctx,
        "POST",
        "/oauth/token",
        &empty_hdrs,
        b"grant_type=authorization_code",
    );
    assert_eq!(no_client.status_code, 400);

    let no_grant = handle_http_request(
        &ctx,
        "POST",
        "/oauth/token",
        &empty_hdrs,
        b"client_id=c1",
    );
    assert_eq!(no_grant.status_code, 400);

    let no_code = handle_http_request(
        &ctx,
        "POST",
        "/oauth/token",
        &empty_hdrs,
        b"client_id=c1&grant_type=authorization_code&code_verifier=v",
    );
    assert_eq!(no_code.status_code, 400);

    let no_verifier = handle_http_request(
        &ctx,
        "POST",
        "/oauth/token",
        &empty_hdrs,
        b"client_id=c1&grant_type=authorization_code&code=abc",
    );
    assert_eq!(no_verifier.status_code, 400);

    let no_refresh = handle_http_request(
        &ctx,
        "POST",
        "/oauth/token",
        &empty_hdrs,
        b"client_id=c1&grant_type=refresh_token",
    );
    assert_eq!(no_refresh.status_code, 400);

    // Exchange code using Basic Authorization header for client_id ("c1:secret" -> "YzE6c2VjcmV0")
    let mut basic_hdrs = HashMap::new();
    basic_hdrs.insert("authorization".to_string(), "basic YzE6c2VjcmV0".to_string());
    let tok_resp = handle_http_request(
        &ctx,
        "POST",
        "/oauth/token",
        &basic_hdrs,
        format!(
            "grant_type=authorization_code&code={code}&code_verifier={verifier}&redirect_uri=http%3A%2F%2Flocalhost%3A8080%2Fcb"
        )
        .as_bytes(),
    );
    assert_eq!(tok_resp.status_code, 200);
    let tok_json: serde_json::Value = serde_json::from_slice(&tok_resp.body).unwrap();
    let access_token = tok_json["access_token"].as_str().unwrap();

    // 4. Verify Bearer token edge cases: missing header, non-Bearer scheme, lowercase "bearer ",
    // required_scope query param, and x-required-scope header.
    let v_no_hdr = verify_esi_bearer_token(&ctx, &empty_hdrs, None).unwrap_err();
    assert_eq!(v_no_hdr.status_code, 401);

    let mut bad_scheme_hdrs = HashMap::new();
    bad_scheme_hdrs.insert("authorization".to_string(), "Basic abc".to_string());
    let v_bad_scheme = verify_esi_bearer_token(&ctx, &bad_scheme_hdrs, None).unwrap_err();
    assert_eq!(v_bad_scheme.status_code, 401);

    let mut lower_bearer_hdrs = HashMap::new();
    lower_bearer_hdrs.insert("authorization".to_string(), format!("bearer {access_token}"));
    let v_req_scope_ok = handle_http_request(
        &ctx,
        "GET",
        "/oauth/verify?required_scope=esi-skills.read_skills.v1",
        &lower_bearer_hdrs,
        b"",
    );
    assert_eq!(v_req_scope_ok.status_code, 200);

    let mut x_scope_hdrs = lower_bearer_hdrs.clone();
    x_scope_hdrs.insert(
        "x-required-scope".to_string(),
        "esi-wallet.read_character_wallet.v1".to_string(),
    );
    let v_x_scope_fail = handle_http_request(&ctx, "GET", "/oauth/verify", &x_scope_hdrs, b"");
    assert_eq!(v_x_scope_fail.status_code, 403);

    // 5. /oauth/revoke missing token -> 400
    let rev_missing = handle_http_request(&ctx, "POST", "/oauth/revoke", &empty_hdrs, b"");
    assert_eq!(rev_missing.status_code, 400);
}

#[test]
fn test_sso_account_ban_and_suspension_branches_on_verify_and_token_exchange() {
    let ctx = ServerContext::new();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let (verifier, challenge) = test_pkce_pair();
    let empty_hdrs = HashMap::new();

    // Provision a player account with a character.
    let uid = ctx.db.provision_player_account("ban_probe", "pass1234");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut ch) = snap.characters.get(&90_000_001).cloned() {
            ch.character_id = 90_000_992;
            ch.user_id = uid;
            ch.name = "Ban Probe".to_string();
            snap.characters.insert(90_000_992, ch);
        }
    });

    let issue_code = || -> String {
        let resp = handle_http_request(
            &ctx,
            "POST",
            "/v2/oauth/authorize",
            &empty_hdrs,
            format!("client_id=c1&redirect_uri=http://127.0.0.1/cb&code_challenge={challenge}&username=ban_probe&password=pass1234").as_bytes(),
        );
        assert_eq!(resp.status_code, 302);
        serde_json::from_slice::<serde_json::Value>(&resp.body).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // Issue authorization codes and exchange two for independent access + refresh token pairs.
    let code_for_tok1 = issue_code();
    let code_for_tok2 = issue_code();
    let code_while_suspended = issue_code();
    let code_while_banned = issue_code();

    let tok_resp1 = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &empty_hdrs,
        format!("client_id=c1&grant_type=authorization_code&code={code_for_tok1}&code_verifier={verifier}&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb").as_bytes(),
    );
    assert_eq!(tok_resp1.status_code, 200);
    let tok_json1: serde_json::Value = serde_json::from_slice(&tok_resp1.body).unwrap();
    let access_tok = tok_json1["access_token"].as_str().unwrap().to_string();
    let refresh_for_susp = tok_json1["refresh_token"].as_str().unwrap().to_string();

    let tok_resp2 = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &empty_hdrs,
        format!("client_id=c1&grant_type=authorization_code&code={code_for_tok2}&code_verifier={verifier}&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb").as_bytes(),
    );
    assert_eq!(tok_resp2.status_code, 200);
    let tok_json2: serde_json::Value = serde_json::from_slice(&tok_resp2.body).unwrap();
    let refresh_for_ban = tok_json2["refresh_token"].as_str().unwrap().to_string();

    let mut bearer_hdrs = HashMap::new();
    bearer_hdrs.insert("authorization".to_string(), format!("Bearer {access_tok}"));

    // 1. Mark account suspended in snapshot (without revoking token JTI) -> 403 on verify, code exchange, and refresh.
    ctx.db.with_mut_snapshot(|snap| {
        let acc = snap.accounts.get_mut("ban_probe").unwrap();
        acc.banned_until_unix = Some(1_800_010_000);
        acc.ban_reason = Some("SUSPENDED:Unit test suspension".to_string());
    });

    let verify_susp = handle_http_request(&ctx, "GET", "/v2/oauth/verify", &bearer_hdrs, b"");
    assert_eq!(verify_susp.status_code, 403);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&verify_susp.body).unwrap()["error"],
        json!("account_suspended")
    );

    let exch_susp = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &empty_hdrs,
        format!("client_id=c1&grant_type=authorization_code&code={code_while_suspended}&code_verifier={verifier}&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb").as_bytes(),
    );
    assert_eq!(exch_susp.status_code, 403);

    let ref_susp = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &empty_hdrs,
        format!("client_id=c1&grant_type=refresh_token&refresh_token={refresh_for_susp}").as_bytes(),
    );
    assert_eq!(ref_susp.status_code, 403);

    // 2. Mark account banned in snapshot -> 403 on verify, code exchange, and refresh.
    ctx.db.with_mut_snapshot(|snap| {
        let acc = snap.accounts.get_mut("ban_probe").unwrap();
        acc.banned_until_unix = Some(1_900_000_000);
        acc.ban_reason = Some("Unit test ban".to_string());
    });

    let verify_ban = handle_http_request(&ctx, "GET", "/v2/oauth/verify", &bearer_hdrs, b"");
    assert_eq!(verify_ban.status_code, 403);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&verify_ban.body).unwrap()["error"],
        json!("account_banned")
    );

    let exch_ban = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &empty_hdrs,
        format!("client_id=c1&grant_type=authorization_code&code={code_while_banned}&code_verifier={verifier}&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb").as_bytes(),
    );
    assert_eq!(exch_ban.status_code, 403);

    let ref_ban = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &empty_hdrs,
        format!("client_id=c1&grant_type=refresh_token&refresh_token={refresh_for_ban}").as_bytes(),
    );
    assert_eq!(ref_ban.status_code, 403);
}

#[test]
fn test_sso_parsers_decoders_and_date_formatting_edge_cases() {
    // JSON body parser with String, Number, Bool, Array, Null, and Object values
    let mut hdrs = HashMap::new();
    let json_body = br#"{"str":"val","num":42,"flag":true,"arr":["a","b",123],"ignored":null,"obj":{}}"#;
    let parsed = parse_request_body(&hdrs, json_body);
    assert_eq!(parsed.get("str").map(String::as_str), Some("val"));
    assert_eq!(parsed.get("num").map(String::as_str), Some("42"));
    assert_eq!(parsed.get("flag").map(String::as_str), Some("true"));
    assert_eq!(parsed.get("arr").map(String::as_str), Some("a b"));
    assert!(!parsed.contains_key("ignored"));
    assert!(!parsed.contains_key("obj"));

    // Non-JSON content falls back to form-urlencoded including key-only pair
    hdrs.insert("content-type".to_string(), "application/json".to_string());
    let fallback_parsed = parse_request_body(&hdrs, b"not_json_flag&k=v%201");
    assert_eq!(
        fallback_parsed.get("not_json_flag").map(String::as_str),
        Some("")
    );
    assert_eq!(fallback_parsed.get("k").map(String::as_str), Some("v 1"));

    // Base64 URL-safe alphabet ('-', '_') & whitespace & Basic auth edge cases
    let decoded = base64_decode_standard(" \t -_8= \r\n ").unwrap();
    assert_eq!(decoded, vec![0xFB, 0xFF]);
    assert!(parse_basic_auth("Bearer xyz").is_none());
    assert!(parse_basic_auth("Basicbm9fY29sb24=").is_none()); // missing space
    assert!(parse_basic_auth("Basic bm9fY29sb24=").is_none()); // "no_colon"

    // Date formatting across Jan/Feb (m <= 2) and Mar-Dec (m > 2)
    assert_eq!(format_unix_iso8601(1_709_164_800), "2024-02-29T00:00:00"); // Leap day Feb
    assert_eq!(format_unix_iso8601(1_719_792_000), "2024-07-01T00:00:00"); // July

    // HTML escaping strips CR/LF and escapes quotes/brackets
    assert_eq!(escape_html_attr("a\r\nb<>\"'&"), "ab&lt;&gt;&quot;&#x27;&amp;");

    // Redirect URI validation edge cases
    assert!(!is_valid_redirect_uri(""));
    assert!(!is_valid_redirect_uri("https://example.com/\x07bell"));
    assert!(is_valid_redirect_uri("http://localhost:26002/callback"));
}

#[tokio::test]
async fn test_sso_tcp_empty_eof_oversized_headers_and_bug_attachment_limit() {
    let ctx = Arc::new(ServerContext::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let srv_ctx = Arc::clone(&ctx);
    let server_task = tokio::spawn(async move {
        for _ in 0..4 {
            let (stream, _) = listener.accept().await.unwrap();
            let c = Arc::clone(&srv_ctx);
            tokio::spawn(async move {
                let _ = handle_http_connection(stream, c).await;
            });
        }
    });

    // 1. Immediate EOF (client disconnects without sending bytes)
    let s1 = TcpStream::connect(addr).await.unwrap();
    drop(s1);

    // 2. Oversized headers (> MAX_HTTP_REQUEST_BYTES without \r\n\r\n) -> 413 Payload Too Large
    let mut s2 = TcpStream::connect(addr).await.unwrap();
    let mut huge_hdr = Vec::with_capacity(MAX_HTTP_REQUEST_BYTES + 1024);
    huge_hdr.extend_from_slice(b"GET /oauth/jwks HTTP/1.1\r\nX-Pad: ");
    huge_hdr.resize(MAX_HTTP_REQUEST_BYTES + 512, b'A');
    let _ = s2.write_all(&huge_hdr).await;
    let mut r2 = Vec::new();
    s2.read_to_end(&mut r2).await.unwrap();
    let r2_str = String::from_utf8_lossy(&r2);
    assert!(r2_str.starts_with("HTTP/1.1 413 Payload Too Large"));
    assert!(r2_str.contains("Request headers too large"));

    // 3. Bug attachment endpoint exceeding MAX_BUG_ATTACHMENT_BYTES -> 413
    let mut s3 = TcpStream::connect(addr).await.unwrap();
    let over_bug = MAX_BUG_ATTACHMENT_BYTES + 1024;
    let req3 = format!(
        "POST /bugs/api/attachment/1 HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {over_bug}\r\n\r\n"
    );
    s3.write_all(req3.as_bytes()).await.unwrap();
    let mut r3 = Vec::new();
    s3.read_to_end(&mut r3).await.unwrap();
    assert!(String::from_utf8_lossy(&r3).starts_with("HTTP/1.1 413 Payload Too Large"));

    // 4. Truncated body EOF (Content-Length: 100, sends 5 bytes and shuts down write half)
    let mut s4 = TcpStream::connect(addr).await.unwrap();
    s4.write_all(
        b"POST /v2/oauth/revoke HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 100\r\n\r\ntoken=abc",
    )
    .await
    .unwrap();
    s4.shutdown().await.unwrap();
    let mut r4 = Vec::new();
    s4.read_to_end(&mut r4).await.unwrap();
    assert!(String::from_utf8_lossy(&r4).starts_with("HTTP/1.1 200 OK"));

    server_task.await.unwrap();
}

#[tokio::test]
async fn test_sso_esi_delegation_authorize_edge_branches_and_tcp_100_continue_body_timeout() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let (verifier, challenge) = test_pkce_pair();
    let empty_hdrs = HashMap::new();

    // 1. ESI delegation via handle_http_request_with_ctx across status codes and context switch
    let r_esi_200 = handle_http_request_with_ctx(&ctx, "GET", "/latest/status", &empty_hdrs, b"");
    assert_eq!(r_esi_200.status_code, 200);
    assert_eq!(r_esi_200.status_text, "OK");
    assert!(!r_esi_200
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type") || k.eq_ignore_ascii_case("content-length")));

    let r_esi_405 = handle_http_request_with_ctx(&ctx, "DELETE", "/latest/status", &empty_hdrs, b"");
    assert_eq!(r_esi_405.status_code, 405);
    assert_eq!(r_esi_405.status_text, "Method Not Allowed");

    let r_esi_400 = handle_http_request_with_ctx(&ctx, "GET", "/v1/status/?page=0", &empty_hdrs, b"");
    assert_eq!(r_esi_400.status_code, 400);

    let r_esi_401 = handle_http_request_with_ctx(
        &ctx,
        "GET",
        "/latest/characters/90000001/wallet",
        &empty_hdrs,
        b"",
    );
    assert_eq!(r_esi_401.status_code, 401);
    assert_eq!(r_esi_401.status_text, "Unauthorized");

    let r_esi_404 =
        handle_http_request_with_ctx(&ctx, "GET", "/v1/does_not_exist_endpoint", &empty_hdrs, b"");
    assert_eq!(r_esi_404.status_code, 404);

    // Switching ServerContext swaps the cached EsiRouter cleanly
    let ctx2 = Arc::new(ServerContext::new());
    let r_ctx2 = handle_http_request_with_ctx(&ctx2, "GET", "/latest/status", &empty_hdrs, b"");
    assert_eq!(r_ctx2.status_code, 200);

    // 2. OAuth authorize edge branches: unsupported_response_type, bad redirect_uri, plain PKCE, redirect with '?' + encoded state, Basic auth
    let bad_rt = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        b"response_type=token&client_id=c1&redirect_uri=http://127.0.0.1/cb&code_challenge=abc",
    );
    assert_eq!(bad_rt.status_code, 400);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bad_rt.body).unwrap()["error"],
        json!("unsupported_response_type")
    );

    let bad_redir = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        b"client_id=c1&redirect_uri=https://evil.example/cb&code_challenge=abc",
    );
    assert_eq!(bad_redir.status_code, 400);

    let bad_pkce_method = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        b"client_id=c1&redirect_uri=http://127.0.0.1/cb&code_challenge=abc&code_challenge_method=plain",
    );
    assert_eq!(bad_pkce_method.status_code, 400);

    // Basic auth credentials + redirect URI already containing '?' + percent-encoded state
    let mut basic_hdrs = HashMap::new();
    // Base64("demo:demo-secret") == "ZGVtbzpkZW1vLXNlY3JldA=="
    basic_hdrs.insert(
        "authorization".to_string(),
        "Basic ZGVtbzpkZW1vLXNlY3JldA==".to_string(),
    );
    let q_auth = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &basic_hdrs,
        format!(
            "client_id=eve-cli&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb%3Ffoo%3Dbar&code_challenge={challenge}&code_challenge_method=S256&state=a%2Bb%20c"
        )
        .as_bytes(),
    );
    assert_eq!(q_auth.status_code, 302);
    let q_json: serde_json::Value = serde_json::from_slice(&q_auth.body).unwrap();
    let redir_loc = q_json["redirect_uri"].as_str().unwrap();
    assert!(redir_loc.starts_with("http://127.0.0.1/cb?foo=bar&code="));
    assert!(redir_loc.contains("&state=a%2Bb%20c"));

    // Invalid JSON / unsupported_grant_type / missing client_id on /v2/oauth/token
    let tok_no_cid = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &empty_hdrs,
        b"grant_type=authorization_code",
    );
    assert_eq!(tok_no_cid.status_code, 400);

    let tok_bad_grant = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &empty_hdrs,
        b"client_id=eve-cli&grant_type=client_credentials",
    );
    assert_eq!(tok_bad_grant.status_code, 400);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&tok_bad_grant.body).unwrap()["error"],
        json!("unsupported_grant_type")
    );

    // Banned and suspended account checks directly on /v2/oauth/authorize
    let _u_ban = ctx.db.provision_player_account("sso_auth_ban", "pw");
    ctx.db
        .ban_account("sso_auth_ban", 1_800_005_000, "ban-auth")
        .unwrap();
    let ban_auth = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        format!(
            "client_id=eve-cli&redirect_uri=http://127.0.0.1/cb&code_challenge={challenge}&username=sso_auth_ban&password=pw"
        )
        .as_bytes(),
    );
    assert_eq!(ban_auth.status_code, 403);
    assert!(String::from_utf8_lossy(&ban_auth.body).contains("banned_until_unix"));

    let _u_susp = ctx.db.provision_player_account("sso_auth_susp", "pw");
    ctx.db
        .suspend_account("sso_auth_susp", 1_800_005_000, "susp-auth")
        .unwrap();
    let susp_auth = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        format!(
            "client_id=eve-cli&redirect_uri=http://127.0.0.1/cb&code_challenge={challenge}&username=sso_auth_susp&password=pw"
        )
        .as_bytes(),
    );
    assert_eq!(susp_auth.status_code, 403);
    assert!(String::from_utf8_lossy(&susp_auth.body).contains("suspended_until_unix"));

    // Exchange code obtained via Basic auth + query-bearing redirect_uri
    let code = q_json["code"].as_str().unwrap();
    let ok_tok = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &empty_hdrs,
        format!("client_id=eve-cli&grant_type=authorization_code&code={code}&code_verifier={verifier}&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb%3Ffoo%3Dbar")
            .as_bytes(),
    );
    assert_eq!(ok_tok.status_code, 200);

    // 3. TCP Expect: 100-continue followed by complete body, plus body-read timeout
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let (srv_acc1, cli_res1) = tokio::join!(listener.accept(), TcpStream::connect(addr));
    let (srv1, _) = srv_acc1.unwrap();
    let mut cli1 = cli_res1.unwrap();
    let ctx_tcp = Arc::clone(&ctx);
    let srv_task1 = tokio::spawn(async move {
        let _ = handle_http_connection_with_timeout(
            srv1,
            ctx_tcp,
            std::time::Duration::from_millis(500),
        )
        .await;
    });
    let body_payload = b"grant_type=unknown";
    cli1.write_all(
        format!(
            "POST /v2/oauth/token HTTP/1.1\r\nHost: 127.0.0.1\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n",
            body_payload.len()
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut cont_buf = [0u8; 64];
    let n_cont = cli1.read(&mut cont_buf).await.unwrap();
    assert!(String::from_utf8_lossy(&cont_buf[..n_cont]).starts_with("HTTP/1.1 100 Continue"));
    cli1.write_all(body_payload).await.unwrap();
    let mut final_resp = Vec::new();
    cli1.read_to_end(&mut final_resp).await.unwrap();
    assert!(String::from_utf8_lossy(&final_resp).contains("400 Bad Request"));
    srv_task1.await.unwrap();

    // 4. Stalled body after headers triggers body-read timeout and exits cleanly
    let (srv_acc2, cli_res2) = tokio::join!(listener.accept(), TcpStream::connect(addr));
    let (srv2, _) = srv_acc2.unwrap();
    let mut cli2 = cli_res2.unwrap();
    cli2.write_all(b"POST /v2/oauth/token HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 200\r\n\r\npartial")
        .await
        .unwrap();
    let timed = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        handle_http_connection_with_timeout(
            srv2,
            Arc::clone(&ctx),
            std::time::Duration::from_millis(35),
        ),
    )
    .await;
    assert!(timed.is_ok());
    assert!(timed.unwrap().is_ok());
}
