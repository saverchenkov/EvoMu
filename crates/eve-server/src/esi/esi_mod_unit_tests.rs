//! Unit and mutation-killing tests for `esi/mod.rs` HTTP/1.1 transport, ETag, date formatting, and server limits.

use super::*;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[test]
fn esi_http_request_builders_and_case_insensitive_headers() {
    let req = EsiHttpRequest::new("GET", "/latest/status/")
        .with_peer_ip("10.20.30.40")
        .with_header("X-Custom-Header", "Alpha")
        .with_bearer_token("tok_123")
        .with_body(b"{\"hello\":1}".to_vec());

    assert_eq!(req.method, "GET");
    assert_eq!(req.raw_target, "/latest/status/");
    assert_eq!(req.peer_ip, "10.20.30.40");
    assert_eq!(req.body, b"{\"hello\":1}");
    assert_eq!(req.header("x-custom-header"), Some("Alpha"));
    assert_eq!(req.header("X-CUSTOM-HEADER"), Some("Alpha"));
    assert_eq!(req.header("authorization"), Some("Bearer tok_123"));
    assert_eq!(req.header("missing"), None);
}

#[test]
fn http_reason_phrase_covers_all_status_branches() {
    let cases = [
        (200, "OK"),
        (201, "Created"),
        (204, "No Content"),
        (304, "Not Modified"),
        (400, "Bad Request"),
        (401, "Unauthorized"),
        (403, "Forbidden"),
        (404, "Not Found"),
        (405, "Method Not Allowed"),
        (413, "Payload Too Large"),
        (420, "Error Limited"),
        (422, "Unprocessable Entity"),
        (500, "Internal Server Error"),
        (501, "Not Implemented"),
        (503, "Service Unavailable"),
        (418, "Response"),
        (599, "Response"),
    ];
    for (code, expected) in cases {
        assert_eq!(http_reason_phrase(code), expected, "code {code}");
    }
}

#[test]
fn esi_http_response_json_and_wire_serialization() {
    let mut headers = HashMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers.insert("ETag".to_string(), "\"abc\"".to_string());
    headers.insert("Content-Length".to_string(), "9999".to_string());
    headers.insert("Connection".to_string(), "keep-alive".to_string());

    let body = br#"{"players":42}"#.to_vec();
    let resp = EsiHttpResponse {
        status_code: 200,
        headers: headers.clone(),
        body: body.clone(),
    };

    assert_eq!(resp.header("content-type"), Some("application/json"));
    assert_eq!(resp.header("ETAG"), Some("\"abc\""));
    assert_eq!(resp.header("unknown"), None);
    assert_eq!(resp.reason_phrase(), "OK");
    let parsed = resp.json().expect("valid json");
    assert_eq!(parsed["players"], 42);

    let wire = String::from_utf8(resp.to_http11_bytes()).expect("utf8 wire");
    assert!(wire.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(wire.contains("Content-Type: application/json\r\n"));
    assert!(wire.contains("ETag: \"abc\"\r\n"));
    assert!(wire.contains(&format!("Content-Length: {}\r\n", body.len())));
    assert!(!wire.contains("Content-Length: 9999"));
    assert!(!wire.contains("Connection: keep-alive"));
    assert!(wire.contains("Connection: close\r\n\r\n{\"players\":42}"));

    // 204 and 304 must suppress body and emit Content-Length: 0
    for code in [204u16, 304u16] {
        let empty_resp = EsiHttpResponse {
            status_code: code,
            headers: HashMap::new(),
            body: b"should_not_be_emitted".to_vec(),
        };
        let wire_empty = String::from_utf8(empty_resp.to_http11_bytes()).expect("utf8");
        assert!(wire_empty.contains("Content-Length: 0\r\n"));
        assert!(!wire_empty.contains("should_not_be_emitted"));
    }

    let bad_resp = EsiHttpResponse {
        status_code: 500,
        headers: HashMap::new(),
        body: b"not-json".to_vec(),
    };
    assert!(bad_resp.json().is_err());
}

#[test]
fn compute_etag_and_if_none_match_rfc7232_evaluation() {
    let empty_etag = compute_etag(b"");
    assert_eq!(empty_etag, "\"cbf29ce484222325\"");
    let hello_etag = compute_etag(b"hello");
    assert_ne!(empty_etag, hello_etag);
    assert!(hello_etag.starts_with('"') && hello_etag.ends_with('"'));
    assert_eq!(hello_etag.len(), 18);

    assert!(if_none_match_matches("*", &hello_etag));
    assert!(if_none_match_matches(" * ", &hello_etag));
    assert!(if_none_match_matches(&hello_etag, &hello_etag));
    assert!(if_none_match_matches(&format!("W/{hello_etag}"), &hello_etag));
    assert!(if_none_match_matches(&hello_etag, &format!("W/{hello_etag}")));
    assert!(if_none_match_matches(
        &format!("\"other\", W/{hello_etag}, \"third\""),
        &hello_etag
    ));
    assert!(!if_none_match_matches("\"mismatch\"", &hello_etag));
    assert!(!if_none_match_matches("", &hello_etag));
}

#[test]
fn format_http_date_formats_epoch_and_leap_dates() {
    assert_eq!(format_http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
    assert_eq!(format_http_date(86_400), "Fri, 02 Jan 1970 00:00:00 GMT");
    assert_eq!(format_http_date(1_700_000_000), "Tue, 14 Nov 2023 22:13:20 GMT");
}

#[tokio::test]
async fn esi_server_tcp_roundtrip_chunked_and_error_guards() {
    let ctx = Arc::new(ServerContext::with_repository(Arc::new(
        eve_db::WorldRepository::with_demo_seed(),
    )));
    let (addr, _router, handle) = EsiServer::spawn(ctx, "127.0.0.1:0")
        .await
        .expect("spawn ESI server");

    // 1. Standard request via send_raw_esi_request
    let req = EsiHttpRequest::new("GET", "/latest/status/")
        .with_header("Accept", "application/json");
    let resp = send_raw_esi_request(addr, &req)
        .await
        .expect("send_raw_esi_request");
    assert_eq!(resp.status_code, 200);
    let json = resp.json().expect("status json");
    assert!(json.get("players").is_some());

    // 2. Empty EOF connection returns cleanly
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.shutdown().await.unwrap();
    }

    // 3. Chunked Transfer-Encoding request body decoding
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let raw_req = concat!(
            "POST /latest/universe/ids/ HTTP/1.1\r\n",
            "Host: 127.0.0.1\r\n",
            "Transfer-Encoding: chunked\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            "8;ext=1\r\n",
            "[\"Jita\"]\r\n",
            "0\r\n",
            "\r\n"
        );
        s.write_all(raw_req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "got: {text}");
    }

    // 4. Oversized Content-Length (> 2 MiB) returns 413 Payload Too Large
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let raw_req = concat!(
            "POST /latest/universe/ids/ HTTP/1.1\r\n",
            "Host: 127.0.0.1\r\n",
            "Content-Length: 9999999\r\n",
            "\r\n"
        );
        s.write_all(raw_req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.starts_with("HTTP/1.1 413 Payload Too Large"), "got: {text}");
    }

    // 5. Invalid Content-Length header returns 400 Bad Request
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let raw_req = concat!(
            "POST /latest/universe/ids/ HTTP/1.1\r\n",
            "Host: 127.0.0.1\r\n",
            "Content-Length: not_a_number\r\n",
            "\r\n"
        );
        s.write_all(raw_req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.starts_with("HTTP/1.1 400 Bad Request"), "got: {text}");
    }

    // 6. Invalid chunk size returns 400 Bad Request
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let raw_req = concat!(
            "POST /latest/universe/ids/ HTTP/1.1\r\n",
            "Host: 127.0.0.1\r\n",
            "Transfer-Encoding: chunked\r\n",
            "\r\n",
            "ZZZ\r\n"
        );
        s.write_all(raw_req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.starts_with("HTTP/1.1 400 Bad Request"), "got: {text}");
    }

    // 7. Excessive header count (> 64) returns 400 Bad Request
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let mut raw_req = String::from("GET /latest/status/ HTTP/1.1\r\n");
        for i in 0..70 {
            raw_req.push_str(&format!("X-Hdr-{i}: val\r\n"));
        }
        raw_req.push_str("\r\n");
        s.write_all(raw_req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.starts_with("HTTP/1.1 400 Bad Request"), "got: {text}");
    }

    handle.abort();
}
