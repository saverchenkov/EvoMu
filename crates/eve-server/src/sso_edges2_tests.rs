//! Additional edge-case and branch-coverage tests for `sso`.

#![forbid(unsafe_code)]

use super::*;
use eve_db::VirtualServerClock;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn pkce_s256(verifier: &str) -> String {
    let digest = super::sso_tests::sha256_digest(verifier.as_bytes());
    eve_db::base64url_encode_no_pad(&digest)
}

#[test]
fn sso_wire_bytes_discovery_fallback_esi_delegation_and_bugs_routing() {
    // 1. HttpResponse::to_wire_bytes sanitizes CR/LF in header names and values.
    let mut resp = HttpResponse::json(200, "OK", json!({"ok": true}));
    resp.headers
        .push(("X-Test\r\nBad".into(), "val\r\nInjected: yes".into()));
    let wire = String::from_utf8(resp.to_wire_bytes()).unwrap();
    assert!(wire.contains("X-TestBad: valInjected: yes\r\n"));
    assert!(!wire.contains("\r\nInjected: yes"));

    // 2. Discovery endpoint without Host header falls back to `http://127.0.0.1:{esi_port}`.
    let ctx = ServerContext::new();
    ctx.set_esi_port(26444);
    let empty_hdrs = HashMap::new();
    let disc = handle_http_request(
        &ctx,
        "GET",
        "/.well-known/oauth-authorization-server",
        &empty_hdrs,
        b"",
    );
    assert_eq!(disc.status_code, 200);
    let disc_json: serde_json::Value = serde_json::from_slice(&disc.body).unwrap();
    assert_eq!(
        disc_json["authorization_endpoint"],
        "http://127.0.0.1:26444/v2/oauth/authorize"
    );

    // With Host header, uses http://{host}.
    let mut host_hdrs = HashMap::new();
    host_hdrs.insert("host".into(), "sso.local:8443".into());
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
        "http://sso.local:8443/v2/oauth/token"
    );

    // 3. handle_http_request (no Arc<ServerContext>) returns 404 on unknown path.
    let no_arc = handle_http_request(&ctx, "GET", "/v1/status/", &empty_hdrs, b"");
    assert_eq!(no_arc.status_code, 404);

    // 4. handle_http_request_with_ctx delegates to EsiRouter (200 OK on /v1/status/ and 404 on unknown).
    let arc_ctx = Arc::new(ctx);
    let esi_status = handle_http_request_with_ctx(&arc_ctx, "GET", "/v1/status/", &empty_hdrs, b"");
    assert_eq!(esi_status.status_code, 200);
    assert_eq!(esi_status.status_text, "OK");
    let esi_404 =
        handle_http_request_with_ctx(&arc_ctx, "GET", "/v1/nonexistent_route_xyz/", &empty_hdrs, b"");
    assert_eq!(esi_404.status_code, 404);
    assert_eq!(esi_404.status_text, "Not Found");

    // 5. /bugs/api routing is handled without falling through to ESI router.
    let bugs_resp = handle_http_request_with_ctx(&arc_ctx, "GET", "/bugs/api/categories", &empty_hdrs, b"");
    assert_ne!(bugs_resp.status_code, 404);
}

#[test]
fn sso_authorize_token_revoke_and_verify_edge_paths() {
    let ctx = ServerContext::new();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_850_000_000)));
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = pkce_s256(verifier);
    let empty_hdrs = HashMap::new();

    // 1. Unsupported response_type -> 400 unsupported_response_type
    let bad_rt = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        b"response_type=token&client_id=c1&redirect_uri=http://localhost/cb&code_challenge=abc",
    );
    assert_eq!(bad_rt.status_code, 400);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bad_rt.body).unwrap()["error"],
        "unsupported_response_type"
    );

    // 2. Unsupported code_challenge_method -> 400 invalid_request
    let bad_ccm = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        b"client_id=c1&redirect_uri=http://localhost/cb&code_challenge=abc&code_challenge_method=plain",
    );
    assert_eq!(bad_ccm.status_code, 400);

    // 3. Invalid redirect_uri variants vs valid loopback / eveauth schemes
    assert!(!is_valid_redirect_uri("http://localhost/cb\n"));
    for bad_uri in [
        "http://localhost.evil.com/cb",
        "http://127.0.0.1.evil.com/cb",
        "https://evil.example/cb",
        "http://localhost/cb\nfoo",
    ] {
        assert!(!is_valid_redirect_uri(bad_uri), "must reject {bad_uri}");
        let r = handle_http_request(
            &ctx,
            "POST",
            "/v2/oauth/authorize",
            &empty_hdrs,
            format!("client_id=c1&redirect_uri={bad_uri}&code_challenge={challenge}").as_bytes(),
        );
        assert_eq!(r.status_code, 400);
    }
    for good_uri in [
        "http://localhost",
        "https://localhost/cb",
        "https://127.0.0.1:9000/cb",
        "eveauth-app://callback",
        "eveauth://callback",
    ] {
        assert!(is_valid_redirect_uri(good_uri), "must accept {good_uri}");
    }

    // 4. GET /v2/oauth/authorize without credentials escapes HTML attributes cleanly.
    let html_resp = handle_http_request(
        &ctx,
        "GET",
        "/v2/oauth/authorize?client_id=c%221&redirect_uri=http%3A%2F%2Flocalhost%2Fcb&code_challenge=abc&state=a%3Cb%26c%27d",
        &empty_hdrs,
        b"",
    );
    assert_eq!(html_resp.status_code, 200);
    let html_str = String::from_utf8(html_resp.body).unwrap();
    assert!(html_str.contains("value=\"c&quot;1\""));
    assert!(html_str.contains("value=\"a&lt;b&amp;c&#x27;d\""));

    // 5. GET /v2/oauth/authorize WITH Basic Auth credentials authorizes directly (302)
    // and redirect_uri already containing '?' uses '&' before code= and percent-encodes state.
    let mut basic_user_hdrs = HashMap::new();
    let creds_b64 = super::sso_tests::encode_base64_std(b"demo:demo-secret");
    basic_user_hdrs.insert("authorization".into(), format!("Basic {creds_b64}"));
    let auth_get = handle_http_request(
        &ctx,
        "GET",
        &format!(
            "/v2/oauth/authorize?client_id=c1&redirect_uri=http%3A%2F%2Flocalhost%2Fcb%3Fexisting%3D1&code_challenge={challenge}&state=st%201%26x&scope=s1,s2"
        ),
        &basic_user_hdrs,
        b"",
    );
    assert_eq!(auth_get.status_code, 302);
    let loc = auth_get
        .headers
        .iter()
        .find(|(k, _)| k == "Location")
        .map(|(_, v)| v.as_str())
        .unwrap();
    assert!(loc.starts_with("http://localhost/cb?existing=1&code="));
    assert!(loc.ends_with("&state=st%201%26x"));

    // 6. Wrong password -> 401 access_denied; banned & suspended accounts on authorize -> 403.
    let bad_pw = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        format!("client_id=c1&redirect_uri=http://localhost/cb&code_challenge={challenge}&username=demo&password=wrong").as_bytes(),
    );
    assert_eq!(bad_pw.status_code, 401);

    let uid_charless = ctx.db.provision_player_account("nochar_user", "pw123");
    assert!(ctx.db.get_characters_for_user(uid_charless).is_empty());
    // Characterless user gets TokenType == "User" and character_id == 0 on /v2/oauth/verify.
    let charless_auth = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        format!("client_id=c1&redirect_uri=http://localhost/cb&code_challenge={challenge}&username=nochar_user&password=pw123&scope=esi-skills.read_skills.v1").as_bytes(),
    );
    assert_eq!(charless_auth.status_code, 302);
    let charless_code = serde_json::from_slice::<serde_json::Value>(&charless_auth.body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let charless_tok = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &empty_hdrs,
        format!("client_id=c1&grant_type=authorization_code&code={charless_code}&code_verifier={verifier}&redirect_uri=http%3A%2F%2Flocalhost%2Fcb").as_bytes(),
    );
    assert_eq!(charless_tok.status_code, 200);
    let charless_json: serde_json::Value = serde_json::from_slice(&charless_tok.body).unwrap();
    let access_tok = charless_json["access_token"].as_str().unwrap();
    let refresh_tok = charless_json["refresh_token"].as_str().unwrap();

    let mut bearer_hdrs = HashMap::new();
    bearer_hdrs.insert("authorization".into(), format!("Bearer {access_tok}"));
    // Query `scope=` parameter + empty `required_scope = Some("")`
    let ver_ok = handle_http_request(
        &ctx,
        "GET",
        "/v2/oauth/verify?scope=esi-skills.read_skills.v1",
        &bearer_hdrs,
        b"",
    );
    assert_eq!(ver_ok.status_code, 200);
    let ver_json: serde_json::Value = serde_json::from_slice(&ver_ok.body).unwrap();
    assert_eq!(ver_json["TokenType"], "User");
    assert_eq!(ver_json["CharacterID"], 0);
    assert!(verify_esi_bearer_token(&ctx, &bearer_hdrs, Some("")).is_ok());

    // Empty Bearer token -> 401
    let mut empty_bearer = HashMap::new();
    empty_bearer.insert("authorization".into(), "Bearer   ".into());
    assert_eq!(
        verify_esi_bearer_token(&ctx, &empty_bearer, None)
            .unwrap_err()
            .status_code,
        401
    );

    // Suspended and banned accounts rejected at /v2/oauth/authorize with 403
    ctx.db.with_mut_snapshot(|snap| {
        let acc = snap.accounts.get_mut("nochar_user").unwrap();
        acc.banned_until_unix = Some(1_900_000_000);
        acc.ban_reason = Some("SUSPENDED:temp".into());
    });
    let auth_susp = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        format!("client_id=c1&redirect_uri=http://localhost/cb&code_challenge={challenge}&username=nochar_user&password=pw123").as_bytes(),
    );
    assert_eq!(auth_susp.status_code, 403);
    assert!(serde_json::from_slice::<serde_json::Value>(&auth_susp.body).unwrap()["suspended_until_unix"].is_u64());

    ctx.db.with_mut_snapshot(|snap| {
        let acc = snap.accounts.get_mut("nochar_user").unwrap();
        acc.ban_reason = Some("perm ban".into());
    });
    let auth_ban = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &empty_hdrs,
        format!("client_id=c1&redirect_uri=http://localhost/cb&code_challenge={challenge}&username=nochar_user&password=pw123").as_bytes(),
    );
    assert_eq!(auth_ban.status_code, 403);
    assert!(serde_json::from_slice::<serde_json::Value>(&auth_ban.body).unwrap()["banned_until_unix"].is_u64());

    // 7. Unsupported grant_type, invalid authorization_code, invalid refresh_token, revoke idempotency
    let bad_gt = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &empty_hdrs,
        b"client_id=c1&grant_type=client_credentials",
    );
    assert_eq!(bad_gt.status_code, 400);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bad_gt.body).unwrap()["error"],
        "unsupported_grant_type"
    );

    let bad_code = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &empty_hdrs,
        b"client_id=c1&grant_type=authorization_code&code=nope&code_verifier=v",
    );
    assert_eq!(bad_code.status_code, 400);

    let bad_ref = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &empty_hdrs,
        b"client_id=c1&grant_type=refresh_token&refresh_token=nope",
    );
    assert_eq!(bad_ref.status_code, 400);

    let rev1 = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/revoke",
        &empty_hdrs,
        format!("token={refresh_tok}").as_bytes(),
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&rev1.body).unwrap()["revoked"],
        true
    );
    let rev2 = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/revoke",
        &empty_hdrs,
        b"token=nonexistent_refresh_token",
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&rev2.body).unwrap()["revoked"],
        false
    );
    // Access token associated with revoked refresh token now fails verification with 401.
    let ver_revoked = handle_http_request(&ctx, "GET", "/v2/oauth/verify", &bearer_hdrs, b"");
    assert_eq!(ver_revoked.status_code, 401);
}

#[test]
fn sso_helper_parsers_and_decoders_edge_cases() {
    assert_eq!(find_subsequence(b"hello", b"hello world"), None);
    assert_eq!(find_subsequence(b"abc\r\n\r\ndef", b"\r\n\r\n"), Some(3));
    assert_eq!(find_subsequence(b"abcdef", b"xyz"), None);

    // percent_decode: '+', valid hex, invalid hex '%ZZ', truncated '%A'
    assert_eq!(percent_decode("a+b%20%2f%2F%ZZ%A"), "a b //%ZZ%A");

    // base64_decode_standard invalid character returns None
    assert!(base64_decode_standard("ab!d").is_none());

    // parse_request_body empty slice
    assert!(parse_request_body(&HashMap::new(), b"").is_empty());

    // format_unix_iso8601 epoch 0 and Dec 31
    assert_eq!(format_unix_iso8601(0), "1970-01-01T00:00:00");
    assert_eq!(format_unix_iso8601(1_735_689_599), "2024-12-31T23:59:59");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sso_tcp_expect_100_continue_non_bug_body_limit_and_mid_body_timeout() {
    let ctx = Arc::new(ServerContext::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let srv_ctx = Arc::clone(&ctx);
    let srv = tokio::spawn(async move {
        // Connection 1: Expect: 100-continue
        let (s1, _) = listener.accept().await.unwrap();
        handle_http_connection(s1, Arc::clone(&srv_ctx))
            .await
            .unwrap();
        // Connection 2: non-bug Content-Length > MAX_HTTP_REQUEST_BYTES -> 413
        let (s2, _) = listener.accept().await.unwrap();
        handle_http_connection(s2, Arc::clone(&srv_ctx))
            .await
            .unwrap();
        // Connection 3: mid-body read timeout
        let (s3, _) = listener.accept().await.unwrap();
        handle_http_connection_with_timeout(
            s3,
            Arc::clone(&srv_ctx),
            std::time::Duration::from_millis(50),
        )
        .await
        .unwrap();
    });

    // 1. Expect: 100-continue receives 100 Continue before sending body.
    let mut c1 = TcpStream::connect(addr).await.unwrap();
    c1.write_all(
        b"POST /v2/oauth/revoke HTTP/1.1\r\nHost: 127.0.0.1\r\nExpect: 100-continue\r\nContent-Length: 10\r\n\r\n",
    )
    .await
    .unwrap();
    let mut prelim = [0u8; 64];
    let n = c1.read(&mut prelim).await.unwrap();
    assert!(String::from_utf8_lossy(&prelim[..n]).starts_with("HTTP/1.1 100 Continue"));
    c1.write_all(b"token=none").await.unwrap();
    let mut rest = Vec::new();
    c1.read_to_end(&mut rest).await.unwrap();
    assert!(String::from_utf8_lossy(&rest).starts_with("HTTP/1.1 200 OK"));

    // 2. Non-bug endpoint Content-Length > MAX_HTTP_REQUEST_BYTES -> 413 Request body too large.
    let mut c2 = TcpStream::connect(addr).await.unwrap();
    let over_req = MAX_HTTP_REQUEST_BYTES + 1;
    let hdr2 = format!(
        "POST /v2/oauth/token HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {over_req}\r\n\r\n"
    );
    c2.write_all(hdr2.as_bytes()).await.unwrap();
    let mut r2 = Vec::new();
    c2.read_to_end(&mut r2).await.unwrap();
    let r2_str = String::from_utf8_lossy(&r2);
    assert!(r2_str.starts_with("HTTP/1.1 413 Payload Too Large"));
    assert!(r2_str.contains("Request body too large"));

    // 3. Mid-body timeout: send headers claiming Content-Length: 100, then stall without EOF.
    let mut c3 = TcpStream::connect(addr).await.unwrap();
    c3.write_all(
        b"POST /v2/oauth/revoke HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 100\r\n\r\ntoken=",
    )
    .await
    .unwrap();
    let mut r3 = Vec::new();
    c3.read_to_end(&mut r3).await.unwrap();
    assert!(r3.is_empty());

    srv.await.unwrap();
}

#[tokio::test]
async fn sso_bug_attachment_total_deadline_concurrency_cap_and_preflight_validation() {
    struct TempDirCleanup(std::path::PathBuf);
    impl Drop for TempDirCleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    assert_eq!(
        MAX_BUG_ATTACHMENT_BYTES,
        crate::bug_reporting::MAX_BUG_ATTACHMENT_BYTES
    );

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let qdir = std::env::temp_dir().join(format!(
        "evomu_sso_bug_attach_{}_{}",
        std::process::id(),
        nanos
    ));
    std::fs::create_dir_all(&qdir).unwrap();
    let _cleanup = TempDirCleanup(qdir.clone());

    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx.bug_reporting.set_queue_dir_override(Some(qdir));

    let empty_hdrs = HashMap::new();
    let issue_payload = serde_json::to_vec(&json!({
        "categoryID": "Combat",
        "summary": "Attachment security check",
        "description": "Verifies HTTP upload caps and deadlines",
        "reproductionSteps": "1. Upload",
        "computerInfo": "macOS",
        "originalReporterId": 1,
        "Server": "EvoMu-Local",
        "Build": eve_machonet::V24_BOOT_BUILD,
        "labels": ["bug"]
    }))
    .unwrap();
    let issue_resp = handle_http_request(&ctx, "POST", "/bugs/api/issue", &empty_hdrs, &issue_payload);
    assert_eq!(issue_resp.status_code, 201);
    let issue_json: serde_json::Value = serde_json::from_slice(&issue_resp.body).unwrap();
    let report_key = issue_json["key"].as_str().unwrap().to_string();

    // 1. Short per-read (80ms) + short total deadline (180ms) listener for trickle & preflight tests.
    let l1 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr1 = l1.local_addr().unwrap();
    let c1 = Arc::clone(&ctx);
    let srv1 = tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = l1.accept().await else {
                break;
            };
            let cc = Arc::clone(&c1);
            tokio::spawn(async move {
                let _ = handle_http_connection_with_deadlines(
                    sock,
                    cc,
                    std::time::Duration::from_millis(80),
                    std::time::Duration::from_millis(180),
                )
                .await;
            });
        }
    });

    // 1a. Oversized Content-Length > 8 MiB -> 413 without Expect: 100-continue.
    let mut s_big = TcpStream::connect(addr1).await.unwrap();
    let over = MAX_BUG_ATTACHMENT_BYTES + 1;
    s_big
        .write_all(
            format!(
                "POST /bugs/api/attachment/{report_key} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: multipart/form-data; boundary=B\r\nContent-Length: {over}\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut r_big = Vec::new();
    s_big.read_to_end(&mut r_big).await.unwrap();
    assert!(String::from_utf8_lossy(&r_big).starts_with("HTTP/1.1 413 Payload Too Large"));

    // 1b. Nonexistent issue ID -> 404 before 100 Continue.
    let mut s_missing = TcpStream::connect(addr1).await.unwrap();
    s_missing
        .write_all(
            b"POST /bugs/api/attachment/EBR-9999-MISSING HTTP/1.1\r\nHost: 127.0.0.1\r\nExpect: 100-continue\r\nContent-Type: multipart/form-data; boundary=B\r\nContent-Length: 32\r\n\r\n",
        )
        .await
        .unwrap();
    let mut r_missing = Vec::new();
    s_missing.read_to_end(&mut r_missing).await.unwrap();
    let r_missing_str = String::from_utf8_lossy(&r_missing);
    assert!(r_missing_str.starts_with("HTTP/1.1 404 Not Found"));
    assert!(!r_missing_str.contains("100 Continue"));

    // 1c. Non-multipart Content-Type -> 400 before 100 Continue.
    let mut s_bad_ct = TcpStream::connect(addr1).await.unwrap();
    s_bad_ct
        .write_all(
            format!(
                "POST /bugs/api/attachment/{report_key} HTTP/1.1\r\nHost: 127.0.0.1\r\nExpect: 100-continue\r\nContent-Type: application/octet-stream\r\nContent-Length: 32\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut r_bad_ct = Vec::new();
    s_bad_ct.read_to_end(&mut r_bad_ct).await.unwrap();
    assert!(String::from_utf8_lossy(&r_bad_ct).starts_with("HTTP/1.1 400 Bad Request"));

    // 1d. Invalid optional Authorization Bearer header -> 401 before 100 Continue.
    let mut s_bad_auth = TcpStream::connect(addr1).await.unwrap();
    s_bad_auth
        .write_all(
            format!(
                "POST /bugs/api/attachment/{report_key} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer bad.jwt.token\r\nContent-Type: multipart/form-data; boundary=B\r\nContent-Length: 32\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut r_bad_auth = Vec::new();
    s_bad_auth.read_to_end(&mut r_bad_auth).await.unwrap();
    assert!(String::from_utf8_lossy(&r_bad_auth).starts_with("HTTP/1.1 401 Unauthorized"));

    // 1e. Slowloris trickle body (every 25ms < 80ms read_timeout) is still cut off by 180ms total deadline.
    let mut s_trickle = TcpStream::connect(addr1).await.unwrap();
    s_trickle
        .write_all(
            format!(
                "POST /bugs/api/attachment/{report_key} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: multipart/form-data; boundary=B\r\nContent-Length: 4096\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let start = tokio::time::Instant::now();
    let mut closed_by_deadline = false;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        if s_trickle.write_all(b"aaaaaaaaaa").await.is_err() {
            closed_by_deadline = true;
            break;
        }
        let mut probe = [0u8; 32];
        if let Ok(Ok(0)) = tokio::time::timeout(
            std::time::Duration::from_millis(2),
            s_trickle.read(&mut probe),
        )
        .await
        {
            closed_by_deadline = true;
            break;
        }
    }
    assert!(
        closed_by_deadline || start.elapsed() < std::time::Duration::from_millis(450),
        "trickle attachment upload must hit total request deadline cleanly"
    );
    if !closed_by_deadline {
        let mut leftover = Vec::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            s_trickle.read_to_end(&mut leftover),
        )
        .await;
        assert!(leftover.is_empty());
    }
    srv1.abort();

    // 2. Bounded concurrency semaphore (MAX_CONCURRENT_BUG_ATTACHMENTS = 4) -> 5th returns 503,
    //    then releasing permits allows legitimate unauthenticated multipart uploads -> 201 Created.
    let l2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr2 = l2.local_addr().unwrap();
    let c2 = Arc::clone(&ctx);
    let srv2 = tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = l2.accept().await else {
                break;
            };
            let cc = Arc::clone(&c2);
            tokio::spawn(async move {
                let _ = handle_http_connection_with_deadlines(
                    sock,
                    cc,
                    std::time::Duration::from_secs(2),
                    std::time::Duration::from_secs(2),
                )
                .await;
            });
        }
    });

    let mut holders = Vec::with_capacity(MAX_CONCURRENT_BUG_ATTACHMENTS);
    for _ in 0..MAX_CONCURRENT_BUG_ATTACHMENTS {
        let mut h = TcpStream::connect(addr2).await.unwrap();
        h.write_all(
            format!(
                "POST /bugs/api/attachment/{report_key} HTTP/1.1\r\nHost: 127.0.0.1\r\nExpect: 100-continue\r\nContent-Type: multipart/form-data; boundary=B\r\nContent-Length: 512\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let mut ack = [0u8; 64];
        let n = h.read(&mut ack).await.unwrap();
        assert!(
            String::from_utf8_lossy(&ack[..n]).starts_with("HTTP/1.1 100 Continue"),
            "holder must acquire attachment permit and receive 100 Continue"
        );
        holders.push(h);
    }

    // 5th concurrent attachment must fail fast with 503 Service Unavailable.
    let mut s_overflow = TcpStream::connect(addr2).await.unwrap();
    s_overflow
        .write_all(
            format!(
                "POST /bugs/api/attachment/{report_key} HTTP/1.1\r\nHost: 127.0.0.1\r\nExpect: 100-continue\r\nContent-Type: multipart/form-data; boundary=B\r\nContent-Length: 128\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut r_overflow = Vec::new();
    s_overflow.read_to_end(&mut r_overflow).await.unwrap();
    assert!(
        String::from_utf8_lossy(&r_overflow).starts_with("HTTP/1.1 503 Service Unavailable"),
        "exceeding concurrent upload cap must return 503, got {}",
        String::from_utf8_lossy(&r_overflow)
    );

    // Drop held connections so permits are released cleanly.
    drop(holders);
    let mp_body = b"--B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"shot.png\"\r\nContent-Type: image/png\r\n\r\nPNGDATA\r\n--B--\r\n";
    let mut upload_ok = false;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let mut s_ok = TcpStream::connect(addr2).await.unwrap();
        s_ok.write_all(
            format!(
                "POST /bugs/api/attachment/{report_key} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: multipart/form-data; boundary=B\r\nContent-Length: {}\r\n\r\n",
                mp_body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        s_ok.write_all(mp_body).await.unwrap();
        let mut r_ok = Vec::new();
        s_ok.read_to_end(&mut r_ok).await.unwrap();
        let r_str = String::from_utf8_lossy(&r_ok);
        if r_str.starts_with("HTTP/1.1 201 Created") {
            upload_ok = true;
            break;
        }
        assert!(
            r_str.starts_with("HTTP/1.1 503 Service Unavailable"),
            "unexpected status while waiting for permits to drain: {r_str}"
        );
    }
    assert!(
        upload_ok,
        "valid unauthenticated multipart bug attachment upload must succeed once permits release"
    );
    srv2.abort();
}
