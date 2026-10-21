//! Regression tests for handshake mismatches found by running the real V24.01 client (T-6).
//!
//! 1. The client's `GPS.Authenticate` reads `response[6]` (`update_info`) from the server's first
//!    frame, so the server hello must be a 7-tuple; a 6-tuple raised `IndexError` in
//!    `machoNet.GetServerStatus`.
//! 2. The client compares `codename@region`, `boot.version` and `macho.version` for equality and
//!    refuses a server whose build is newer than its own (`start.ini`: `V24.01@ccp`, build 3624425,
//!    `machoVersion = 496`).
//! 3. After `GetServerStatus` sends `(None, 'QC')`, the client keeps the half-open transport and
//!    `ConnectToServer` begins by reading a fresh version tuple, so the server has to restart the
//!    handshake after answering the queue check.
//! 4. A CryptoAPI crypto context is refused with a reason instead of "OK CC".

use eve_machonet::{GPSTransportClosed, HandshakeCommand, VersionExchange};
use eve_proto::PyRep;
use eve_server::macho_listener::{handle_macho_client, read_framed_pyrep, write_framed_pyrep};
use eve_server::router::ServerContext;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

async fn spawn_listener() -> std::net::SocketAddr {
    let ctx = Arc::new(ServerContext::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind macho");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let c = Arc::clone(&ctx);
            tokio::spawn(async move {
                let _ = handle_macho_client(stream, c).await;
            });
        }
    });
    addr
}

/// Client reply exactly as `GPS.Authenticate` builds it from the installed client's boot values.
fn real_client_request(build: i64, codename_at_region: &str) -> PyRep {
    PyRep::Tuple(vec![
        PyRep::Integer(170_472),
        PyRep::Integer(496),
        PyRep::Integer(0),
        PyRep::Real(24.01),
        PyRep::Integer(build),
        PyRep::ascii_bytes(codename_at_region),
    ])
}

#[tokio::test]
async fn server_hello_is_v24_seven_tuple_and_qc_restarts_handshake() {
    let addr = spawn_listener().await;
    let mut s = TcpStream::connect(addr).await.unwrap();
    let hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    let PyRep::Tuple(items) = &hello else {
        panic!("server hello must be a tuple, got {hello:?}");
    };
    assert_eq!(items.len(), 7, "GPS.Authenticate reads response[6]");
    assert_eq!(items[0], PyRep::Integer(170_472));
    assert_eq!(items[1], PyRep::Integer(496));
    assert_eq!(items[3], PyRep::Real(24.01));
    assert_eq!(items[4], PyRep::Integer(3_624_425));
    assert_eq!(items[5].as_str(), Some("V24.01@ccp"));
    assert_eq!(items[6], PyRep::None);

    // GetServerStatus: version reply, then queue check.
    write_framed_pyrep(&mut s, &real_client_request(3_624_425, "V24.01@ccp"))
        .await
        .unwrap();
    write_framed_pyrep(&mut s, &HandshakeCommand::QueueCheck.to_pyrep())
        .await
        .unwrap();
    assert_eq!(
        read_framed_pyrep(&mut s).await.unwrap().unwrap(),
        PyRep::Integer(1)
    );
    // ConnectToServer on the same transport reads a new version tuple first.
    let again = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    assert_eq!(again, hello);
    write_framed_pyrep(&mut s, &real_client_request(3_624_425, "V24.01@ccp"))
        .await
        .unwrap();
    write_framed_pyrep(&mut s, &HandshakeCommand::QueueCheck.to_pyrep())
        .await
        .unwrap();
    assert_eq!(
        read_framed_pyrep(&mut s).await.unwrap().unwrap(),
        PyRep::Integer(1)
    );
}

#[tokio::test]
async fn version_checks_follow_client_gps_semantics() {
    let addr = spawn_listener().await;
    let cases: [(i64, &str, Option<&str>); 4] = [
        // Newer client build is accepted (server refuses only if its own build is newer).
        (3_700_000, "V24.01@ccp", None),
        (3_619_879, "V24.01@ccp", Some("HANDSHAKE_INCOMPATIBLEBUILD")),
        (3_624_425, "EVE-EVE-TRANQUILITY@ccp", Some("HANDSHAKE_INCOMPATIBLERELEASE")),
        (3_624_425, "V24.01@optic", Some("HANDSHAKE_INCOMPATIBLEREGION")),
    ];
    for (build, cr, expected) in cases {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &real_client_request(build, cr))
            .await
            .unwrap();
        match expected {
            None => {
                // A queue check only gets an answer if the version reply was accepted.
                write_framed_pyrep(&mut s, &HandshakeCommand::QueueCheck.to_pyrep())
                    .await
                    .unwrap();
                let rsp = read_framed_pyrep(&mut s).await.unwrap().unwrap();
                assert_eq!(rsp, PyRep::Integer(1), "build={build} {cr}");
            }
            Some(code) => {
                // The server answers a rejected version reply with GPSTransportClosed and closes.
                let rsp = read_framed_pyrep(&mut s).await.unwrap().unwrap();
                let closed = GPSTransportClosed::from_pyrep(&rsp)
                    .unwrap_or_else(|| panic!("expected GPSTransportClosed for {build} {cr}"));
                assert_eq!(closed.reason_code.as_deref(), Some(code), "build={build} {cr}");
            }
        }
    }
    let d = VersionExchange::default();
    assert_eq!((d.codename(), d.region()), ("V24.01", "ccp"));
}

#[tokio::test]
async fn cryptoapi_session_key_request_is_refused_with_reason() {
    // The real client (start.ini `cryptoPack = CryptoAPI`) sends keyVersion "placebo" plus an
    // RSA-encrypted `crypting_sessionkey`/`crypting_sessioniv`; only Placebo is usable here.
    let addr = spawn_listener().await;
    let mut s = TcpStream::connect(addr).await.unwrap();
    let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    write_framed_pyrep(&mut s, &real_client_request(3_624_425, "V24.01@ccp"))
        .await
        .unwrap();
    let mut req = eve_machonet::CryptoRequestPacket::placebo();
    req.key_params
        .insert("crypting_sessionkey".into(), PyRep::Bytes(vec![0xAB; 512]));
    req.key_params
        .insert("crypting_sessioniv".into(), PyRep::Bytes(vec![0xCD; 512]));
    write_framed_pyrep(&mut s, &req.to_pyrep()).await.unwrap();
    let rsp = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    let closed = GPSTransportClosed::from_pyrep(&rsp).expect("GPSTransportClosed, not OK CC");
    assert_eq!(
        closed.reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEPUBLICKEY")
    );
    assert!(closed.reason.contains("cryptoPack = Placebo"), "{}", closed.reason);

    // A Placebo context (empty params) is still accepted.
    let mut s2 = TcpStream::connect(addr).await.unwrap();
    let _ = read_framed_pyrep(&mut s2).await.unwrap().unwrap();
    write_framed_pyrep(&mut s2, &real_client_request(3_624_425, "V24.01@ccp"))
        .await
        .unwrap();
    write_framed_pyrep(&mut s2, &eve_machonet::CryptoRequestPacket::placebo().to_pyrep())
        .await
        .unwrap();
    let ok = read_framed_pyrep(&mut s2).await.unwrap().unwrap();
    assert_eq!(ok.as_str(), Some("OK CC"));
}

#[test]
fn placebo_crypto_hash_matches_blue_marshal_bytes() {
    // The real client rejected our CryptoServerHandshake ("Server response signature incorrect")
    // while the hash was computed over a 0x10 short-string encoding. blue.marshal.Save writes
    // `7E 00000000 25 13 40 <64 bytes>` for `(challenge,)`; Python's binascii.crc_hqx of those
    // bytes for a 64-byte zero challenge is 55087.
    assert_eq!(eve_machonet::placebo_crypto_hash(&[0u8; 64]), "55087");
    let mut blue = vec![0x7E, 0, 0, 0, 0, 0x25, 0x13, 0x40];
    blue.extend_from_slice(&[0u8; 64]);
    assert_eq!(
        eve_machonet::placebo_crypto_hash(&[0u8; 64]),
        eve_machonet::crc_hqx(&blue, 0).to_string()
    );
}
