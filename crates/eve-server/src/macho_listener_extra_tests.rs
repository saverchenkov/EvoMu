//! Additional unit and mutation-killing tests for `macho_listener`.

#![forbid(unsafe_code)]

use super::*;
use eve_db::VirtualServerClock;
use eve_machonet::{
    placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeAck, CryptoHandshakeResult,
    CryptoRequestPacket, CryptoServerHandshake,
};
use std::io::Cursor;
use tokio::net::TcpListener;

async fn spawn_test_listener(ctx: Arc<ServerContext>) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test macho listener");
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

async fn login_client(
    addr: std::net::SocketAddr,
    username: &str,
    password: &str,
    lang: &str,
) -> (TcpStream, u64, u64) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
        .await
        .unwrap();
    write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
        .await
        .unwrap();
    let cc = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    assert_eq!(cc.as_str(), Some("OK CC"));

    let mut chal = CryptoChallengePacket::new_password_login(username, password);
    chal.user_language_id = lang.to_string();
    write_framed_pyrep(&mut s, &chal.to_pyrep()).await.unwrap();
    let pw_ver = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    assert_eq!(pw_ver.as_i64(), Some(2));
    let srv_hs =
        CryptoServerHandshake::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap())
            .expect("CryptoServerHandshake");

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
        .expect("CryptoHandshakeAck");
    let init_pkt =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    let MachoAddress::Client { client_id, .. } = init_pkt.destination else {
        panic!("expected Client destination");
    };
    let _sess_chg =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    (s, client_id, ack.user_id)
}

#[tokio::test]
async fn test_read_framed_pyrep_corrupt_marshal_and_single_byte_valid() {
    // 1-byte length with invalid marshal header must fail in marshal::decode
    let mut bad_marshal = Vec::new();
    bad_marshal.extend_from_slice(&1u32.to_le_bytes());
    bad_marshal.push(0x00);
    let mut cur = Cursor::new(bad_marshal);
    assert!(read_framed_pyrep(&mut cur).await.is_err());

    // Exact MAX_MACHO_FRAME_LEN is allowed by length check (fails EOF on empty body, not Invalid frame length)
    let mut max_hdr = Cursor::new((MAX_MACHO_FRAME_LEN as u32).to_le_bytes().to_vec());
    let err_msg = read_framed_pyrep(&mut max_hdr)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        !err_msg.contains("Invalid machoNet frame length"),
        "MAX_MACHO_FRAME_LEN should pass length guard, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_handshake_step_errors_sso_login_and_mid_handshake_ban() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let _uid = ctx.db.provision_player_account("midban_user", "secret_mb");
    let pkce_code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            90_000_001,
            "evomu-launcher",
            "https://localhost/callback",
            vec!["publicData".into()],
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            1_800_000_000,
        )
        .unwrap();
    let jwt = ctx
        .db
        .exchange_pkce_code(
            &pkce_code,
            "evomu-launcher",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            1_800_000_000,
        )
        .unwrap()
        .access_token;
    let addr = spawn_test_listener(Arc::clone(&ctx)).await;

    // 1. Premature invalid packet before VersionExchange triggers handshake.step Err -> GPSTransportClosed
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &PyRep::String("unexpected_handshake_frame".into()))
            .await
            .unwrap();
        let closed_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        assert!(GPSTransportClosed::from_pyrep(&closed_rep).is_some());
    }

    // 2. SSO token login with blank language_id defaults to "EN"
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
            .await
            .unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
            .await
            .unwrap();
        let _cc = read_framed_pyrep(&mut s).await.unwrap().unwrap();

        let mut chal = CryptoChallengePacket::new_sso_login(&jwt);
        chal.user_language_id = "   ".to_string();
        write_framed_pyrep(&mut s, &chal.to_pyrep()).await.unwrap();
        let pw_ver = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        assert_eq!(pw_ver.as_i64(), Some(2));
        let srv_hs =
            CryptoServerHandshake::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap())
                .expect("SSO CryptoServerHandshake");
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
        let ack =
            CryptoHandshakeAck::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap())
                .expect("SSO CryptoHandshakeAck");
        assert_eq!(ack.user_id, 1);
    }

    // 3. Account banned mid-handshake (after CryptoServerHandshake, before CryptoHandshakeResult)
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
            .await
            .unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
            .await
            .unwrap();
        let _cc = read_framed_pyrep(&mut s).await.unwrap().unwrap();

        let chal = CryptoChallengePacket::new_password_login("midban_user", "secret_mb");
        write_framed_pyrep(&mut s, &chal.to_pyrep()).await.unwrap();
        let pw_ver = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        assert_eq!(pw_ver.as_i64(), Some(2));
        let srv_hs =
            CryptoServerHandshake::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap())
                .expect("CryptoServerHandshake before mid-ban");

        // Ban the account before sending CryptoHandshakeResult
        ctx.db
            .ban_account("midban_user", 1_800_090_000, "Mid-handshake ban")
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
        let closed =
            GPSTransportClosed::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap())
                .expect("expected GPSTransportClosed on mid-handshake ban");
        assert_eq!(closed.reason_code.as_deref(), Some("UserBanned"));
    }
}

#[tokio::test]
async fn test_empty_ping_non_packet_frames_async_notifications_and_revocation() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let addr = spawn_test_listener(Arc::clone(&ctx)).await;

    let (mut s, client_id, user_id) = login_client(addr, "demo", "demo-secret", "DE").await;

    // 1. Non-MachoPacket frame after auth is ignored without dropping connection
    write_framed_pyrep(&mut s, &PyRep::Integer(777))
        .await
        .unwrap();

    // 2. Unhandled MachoPacketType (e.g. Notification sent by client) is ignored without dropping connection
    let ignored_pkt = MachoPacket {
        packet_type: MachoPacketType::Notification,
        source: MachoAddress::Client {
            client_id,
            call_id: 1,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: None,
            call_id: 0,
        },
        user_id: Some(user_id),
        payload: PyRep::None,
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &ignored_pkt.to_pyrep())
        .await
        .unwrap();

    // 3. PingReq with empty times list uses fallback (now_ft, now_ft) and returns 9 entries
    let empty_ping = MachoPacket {
        packet_type: MachoPacketType::PingReq,
        source: MachoAddress::Client {
            client_id,
            call_id: 2,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: None,
            call_id: 0,
        },
        user_id: Some(user_id),
        payload: PyRep::Tuple(vec![PyRep::List(Vec::new())]),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &empty_ping.to_pyrep())
        .await
        .unwrap();
    let ping_rsp =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(ping_rsp.packet_type, MachoPacketType::PingRsp);
    if let PyRep::Tuple(items) = &ping_rsp.payload {
        if let Some(PyRep::List(times)) = items.first() {
            assert_eq!(times.len(), 9);
        } else {
            panic!("expected PingRsp list");
        }
    }

    // 4. SelectCharacterID then push async notification via ctx.enqueue_notification
    let sel_req = MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client {
            client_id,
            call_id: 3,
            service: None,
        },
        destination: MachoAddress::Any {
            service: Some("charUnboundMgr".to_string()),
            call_id: 0,
        },
        user_id: Some(user_id),
        payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::ascii_bytes("SelectCharacterID"),
                PyRep::Tuple(vec![PyRep::Integer(90_000_001)]),
                PyRep::Dict(Vec::new()),
            ]))),
        ])]),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &sel_req.to_pyrep())
        .await
        .unwrap();
    // Wait for both CallRsp and SessionChangeNotification from SelectCharacterID
    let mut got_rsp = false;
    let mut got_sess_chg = false;
    while !got_rsp || !got_sess_chg {
        let pkt = MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap())
            .unwrap();
        if pkt.packet_type == MachoPacketType::CallRsp {
            got_rsp = true;
        } else if pkt.packet_type == MachoPacketType::SessionChangeNotification {
            got_sess_chg = true;
        }
    }

    // Push an asynchronous notification and verify notify_rx branch flushes it to client
    ctx.notify(
        crate::router::NotificationTarget::Character(90_000_001),
        "OnAccountChange",
        "charid",
        vec![
            PyRep::ascii_bytes("cash"),
            PyRep::Integer(1000),
            PyRep::Real(2_500_000.0),
        ],
    );
    loop {
        let pushed = MachoPacket::from_pyrep(
            &tokio::time::timeout(
                std::time::Duration::from_secs(2),
                read_framed_pyrep(&mut s),
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        )
        .unwrap();
        if pushed.packet_type == MachoPacketType::Notification {
            if let MachoAddress::Broadcast { broadcast_id, .. } = &pushed.destination {
                if broadcast_id == "OnAccountChange" {
                    break;
                }
            }
        }
    }

    // 5. Banning account while connected revokes session on next packet
    ctx.db
        .ban_account("demo", 1_800_090_000, "Banned while online")
        .unwrap();
    write_framed_pyrep(&mut s, &empty_ping.to_pyrep())
        .await
        .unwrap();
    loop {
        let rev_frame = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        if let Some(rev_closed) = GPSTransportClosed::from_pyrep(&rev_frame) {
            assert_eq!(rev_closed.reason_code.as_deref(), Some("UserBanned"));
            break;
        }
    }
}
