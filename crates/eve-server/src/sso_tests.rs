//! Unit tests for EVE Online SSO / OAuth2 + PKCE (`S256`) endpoints, Bearer JWT verification,
//! token rotation/revocation, and HTTP/1.1 connection framing (`sso`).

#![forbid(unsafe_code)]

use super::*;

#[cfg(test)]
#[path = "sso_http_framing_and_security_tests.rs"]
mod sso_http_framing_and_security_tests;

pub(super) fn sha256_digest(data: &[u8]) -> [u8; 32] {
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

pub(super) fn pkce_challenge_s256(verifier: &str) -> String {
    eve_db::base64url_encode_no_pad(&sha256_digest(verifier.as_bytes()))
}

pub(super) fn encode_base64_std(input: &[u8]) -> String {
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
    assert!(is_valid_redirect_uri("eveauth-app://callback"));
    assert!(is_valid_redirect_uri("http://localhost:8080/cb"));
    assert!(is_valid_redirect_uri("http://127.0.0.1:8080/cb"));
    assert!(!is_valid_redirect_uri("https://app.example.com/cb"));
    assert!(!is_valid_redirect_uri("http://localhost.attacker.com/cb"));
    assert!(!is_valid_redirect_uri("http://untrusted.example.com/cb"));
    assert!(!is_valid_redirect_uri("https://app.example.com/\ncb"));

    // Bugs delegation & 404 fallback
    let bugs_status = handle_http_request(&ctx, "GET", "/bugs/api/status", &empty_hdrs, b"");
    assert_eq!(bugs_status.status_code, 200);
    let not_found = handle_http_request(&ctx, "DELETE", "/v2/unknown", &empty_hdrs, b"");
    assert_eq!(not_found.status_code, 404);
}
