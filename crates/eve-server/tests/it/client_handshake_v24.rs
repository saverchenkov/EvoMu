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

#[tokio::test]
async fn issue_25_macho_notification_3_layer_envelope_and_tcp_push() {
    use eve_machonet::{
        placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeAck, CryptoHandshakeResult,
        CryptoRequestPacket, CryptoServerHandshake, MachoAddress, MachoPacket, MachoPacketType,
        MACHO_PROXY_NODE_ID,
    };
    use eve_server::router::NotificationTarget;
    use std::collections::HashMap;

    // 1. Verify ServerContext::build_macho_notification_packet builds a 3-layer V24 GPCS Notification
    let (built_pkt, built_bytes) = ServerContext::build_macho_notification_packet(
        90_000_001,
        Some(1001),
        "OnSkillTrained",
        "charid",
        vec![PyRep::Integer(3300), PyRep::Integer(5)],
    );
    let (bcast_name, bcast_args) = built_pkt
        .extract_broadcast_notification()
        .expect("build_macho_notification_packet must produce 3-layer V24 GPCS notification");
    assert_eq!(bcast_name, "OnSkillTrained");
    assert_eq!(bcast_args, &[PyRep::Integer(3300), PyRep::Integer(5)]);
    let roundtrip_pkt =
        MachoPacket::from_pyrep(&eve_proto::marshal::decode(&built_bytes).unwrap()).unwrap();
    assert_eq!(
        roundtrip_pkt.extract_broadcast_notification(),
        Some(("OnSkillTrained", &[PyRep::Integer(3300), PyRep::Integer(5)][..]))
    );

    // 2. Verify handle_macho_client pushes asynchronous ctx.notify frames over TCP
    let ctx = Arc::new(ServerContext::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind macho");
    let addr = listener.local_addr().unwrap();
    let ctx_srv = Arc::clone(&ctx);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let c = Arc::clone(&ctx_srv);
            tokio::spawn(async move {
                let _ = handle_macho_client(stream, c).await;
            });
        }
    });

    let mut s = TcpStream::connect(addr).await.unwrap();
    let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
        .await
        .unwrap();
    write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
        .await
        .unwrap();
    let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();

    let mut attempt1 = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    attempt1.user_password = None;
    write_framed_pyrep(&mut s, &attempt1.to_pyrep()).await.unwrap();
    let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();

    let attempt2 = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    write_framed_pyrep(&mut s, &attempt2.to_pyrep()).await.unwrap();
    let srv_hs = CryptoServerHandshake::from_pyrep(
        &read_framed_pyrep(&mut s).await.unwrap().unwrap(),
    )
    .unwrap();
    write_framed_pyrep(
        &mut s,
        &(CryptoHandshakeResult {
            challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
            func_output: Vec::new(),
            func_result: PyRep::Integer(0),
        })
        .to_pyrep(),
    )
    .await
    .unwrap();
    let ack = CryptoHandshakeAck::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap())
        .unwrap();
    let init_pkt =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    let MachoAddress::Client { client_id, .. } = init_pkt.destination else {
        panic!("expected Client destination");
    };
    let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();

    let sel_pkt = MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client {
            client_id,
            call_id: 1,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: Some("charUnboundMgr".into()),
            call_id: 0,
        },
        user_id: Some(ack.user_id),
        payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::ascii_bytes("SelectCharacterID"),
                PyRep::Tuple(vec![PyRep::Integer(90_000_001)]),
                PyRep::Dict(vec![]),
            ]))),
        ])]),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &sel_pkt.to_pyrep()).await.unwrap();
    let sel_rsp =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(sel_rsp.packet_type, MachoPacketType::CallRsp);
    let sel_sess =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(sel_sess.packet_type, MachoPacketType::SessionChangeNotification);

    // Emit an async server notification to character 90_000_001 and verify it arrives on the TCP stream
    ctx.notify(
        NotificationTarget::Character(90_000_001),
        "OnSkillsTrained",
        "charid",
        vec![PyRep::List(vec![PyRep::Integer(3300)])],
    );

    let mut received_skills_trained = false;
    for _ in 0..8 {
        let frame = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            read_framed_pyrep(&mut s),
        )
        .await
        .expect("timed out waiting for Notification frame on TCP socket")
        .unwrap()
        .unwrap();
        let pkt = MachoPacket::from_pyrep(&frame).expect("decode MachoPacket");
        if pkt.packet_type == MachoPacketType::Notification {
            let (ev, args) = pkt
                .extract_broadcast_notification()
                .expect("3-layer GPCS notification over TCP");
            if ev == "OnSkillsTrained" {
                assert_eq!(args, &[PyRep::List(vec![PyRep::Integer(3300)])]);
                received_skills_trained = true;
                break;
            }
        }
    }
    assert!(received_skills_trained, "Expected OnSkillsTrained over TCP");
}
