#![forbid(unsafe_code)]

use super::*;
use eve_db::VirtualServerClock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[test]
fn sso_esi_router_delegation_and_user_token_verify_edges() {
    let ctx1 = Arc::new(ServerContext::new());
    ctx1.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let ctx2 = Arc::new(ServerContext::new());
    ctx2.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let empty_hdrs = HashMap::new();

    // 1. handle_http_request_with_ctx delegates unknown routes to EsiRouter (200 / 401 / 404 / 405)
    let status_resp =
        handle_http_request_with_ctx(&ctx1, "GET", "/latest/status/", &empty_hdrs, b"");
    assert_eq!(status_resp.status_code, 200);

    let unauth_esi = handle_http_request_with_ctx(
        &ctx1,
        "GET",
        "/latest/characters/90000001/wallet/",
        &empty_hdrs,
        b"",
    );
    assert_eq!(unauth_esi.status_code, 401);

    let not_found_esi =
        handle_http_request_with_ctx(&ctx1, "GET", "/latest/nonexistent_route/", &empty_hdrs, b"");
    assert_eq!(not_found_esi.status_code, 404);

    let method_not_allowed =
        handle_http_request_with_ctx(&ctx1, "POST", "/latest/status/", &empty_hdrs, b"");
    assert!(matches!(method_not_allowed.status_code, 404 | 405));

    // Second ctx swaps the cached EsiRouter db_key branch cleanly
    let status_resp2 =
        handle_http_request_with_ctx(&ctx2, "GET", "/latest/status/", &empty_hdrs, b"");
    assert_eq!(status_resp2.status_code, 200);

    // 2. Empty Bearer token -> 401 "Authorization header must use Bearer scheme"
    let mut empty_bearer = HashMap::new();
    empty_bearer.insert("authorization".to_string(), "Bearer   ".to_string());
    let err_empty = verify_esi_bearer_token(&ctx1, &empty_bearer, None).unwrap_err();
    assert_eq!(err_empty.status_code, 401);

    // 3. Account-scoped User token (character_id == 0) verifies with TokenType = "User"
    let _no_char_uid = ctx1.db.provision_player_account("user_only", "secret123");
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = super::sso_tests::pkce_challenge_s256(verifier);

    // GET /v2/oauth/authorize WITH Basic auth directly issues 302 code
    let mut get_auth_hdrs = HashMap::new();
    get_auth_hdrs.insert(
        "authorization".to_string(),
        format!(
            "Basic {}",
            super::sso_tests::encode_base64_std(b"user_only:secret123")
        ),
    );
    let get_auth_qs = format!(
        "/v2/oauth/authorize?client_id=eve-launcher&redirect_uri=https%3A%2F%2Flocalhost%3A8443%2Fcb&code_challenge={challenge}&scope=esi-location.read_location.v1"
    );
    let get_auth = handle_http_request(&ctx1, "GET", &get_auth_qs, &get_auth_hdrs, b"");
    assert_eq!(get_auth.status_code, 302);
    let code = serde_json::from_slice::<serde_json::Value>(&get_auth.body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();

    let tok = handle_http_request(
        &ctx1,
        "POST",
        "/v2/oauth/token",
        &empty_hdrs,
        format!(
            "grant_type=authorization_code&client_id=eve-launcher&code={code}&code_verifier={verifier}&redirect_uri=https%3A%2F%2Flocalhost%3A8443%2Fcb"
        )
        .as_bytes(),
    );
    assert_eq!(tok.status_code, 200);
    let access = serde_json::from_slice::<serde_json::Value>(&tok.body).unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_string();
    let mut bearer = HashMap::new();
    bearer.insert("authorization".to_string(), format!("Bearer {access}"));
    let verify_user = handle_http_request(&ctx1, "GET", "/v2/oauth/verify", &bearer, b"");
    assert_eq!(verify_user.status_code, 200);
    let vjson: serde_json::Value = serde_json::from_slice(&verify_user.body).unwrap();
    assert_eq!(vjson["TokenType"], serde_json::json!("User"));
    assert_eq!(vjson["CharacterID"], serde_json::json!(0));

    // 4. Loopback & percent_decode edge cases
    assert!(is_valid_redirect_uri("http://localhost"));
    assert!(is_valid_redirect_uri("https://127.0.0.1:9000/cb"));
    assert_eq!(percent_decode("abc%2"), "abc%2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sso_tcp_header_and_body_read_timeouts() {
    let ctx = Arc::new(ServerContext::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let srv_ctx = Arc::clone(&ctx);
    let server_task = tokio::spawn(async move {
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let c = Arc::clone(&srv_ctx);
            tokio::spawn(async move {
                let _ = handle_http_connection_with_timeout(
                    stream,
                    c,
                    std::time::Duration::from_millis(25),
                )
                .await;
            });
        }
    });

    // 1. Header read timeout (partial headers without \r\n\r\n, kept open > 25ms)
    let mut s1 = TcpStream::connect(addr).await.unwrap();
    s1.write_all(b"GET /oauth/jwks HTTP/1.1\r\nHost: 127.0.0.1\r\n")
        .await
        .unwrap();
    let mut r1 = Vec::new();
    let _ = s1.read_to_end(&mut r1).await;
    assert!(r1.is_empty());

    // 2. Body read timeout (headers declare Content-Length: 1000, client sends 10 bytes and stalls)
    let mut s2 = TcpStream::connect(addr).await.unwrap();
    s2.write_all(
        b"POST /v2/oauth/revoke HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 1000\r\n\r\ntoken=1234",
    )
    .await
    .unwrap();
    let mut r2 = Vec::new();
    let _ = s2.read_to_end(&mut r2).await;
    assert!(r2.is_empty());

    server_task.await.unwrap();
    assert_eq!(
        super::percent_encode_query_component("azAZ09-_.~ a+b/c=d"),
        "azAZ09-_.~%20a%2Bb%2Fc%3Dd"
    );
}
