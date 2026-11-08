//! Unit tests for the native V24.01 `machoNet` / `GPSTransport` TCP listener,
//! 4-step handshake authentication, `PingReq`, `CallReq` (`OID+` binding & error wrapping),
//! `DoDestinyUpdate` ballpark streaming, and concurrent-login session displacement (`macho_listener`).

#![forbid(unsafe_code)]

use super::*;
use eve_db::VirtualServerClock;
use eve_machonet::{
    placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeAck, CryptoHandshakeResult,
    CryptoRequestPacket, CryptoServerHandshake,
};
use std::io::Cursor;
use tokio::net::TcpListener;

async fn spawn_macho_test_server(ctx: Arc<ServerContext>) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind macho listener");
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

async fn connect_and_authenticate(
    addr: std::net::SocketAddr,
    username: &str,
    password: &str,
) -> (TcpStream, u64, u64) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
        .await
        .unwrap();
    write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
        .await
        .unwrap();
    let cc_reply = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    assert_eq!(cc_reply.as_str(), Some("OK CC"));

    let mut live_update_req = CryptoChallengePacket::new_password_login(username, password);
    live_update_req.user_password = None;
    write_framed_pyrep(&mut s, &live_update_req.to_pyrep())
        .await
        .unwrap();
    let _live_update_rsp = read_framed_pyrep(&mut s).await.unwrap().unwrap();

    let challenge_pkt = CryptoChallengePacket::new_password_login(username, password);
    write_framed_pyrep(&mut s, &challenge_pkt.to_pyrep())
        .await
        .unwrap();
    let srv_hs =
        CryptoServerHandshake::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap())
            .expect("decode CryptoServerHandshake");

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
        .expect("decode CryptoHandshakeAck");
    let init_pkt =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(
        init_pkt.packet_type,
        MachoPacketType::SessionInitialStateNotification
    );
    let MachoAddress::Client { client_id, .. } = init_pkt.destination else {
        panic!("expected Client destination on SessionInitialStateNotification");
    };
    let sess_chg =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(
        sess_chg.packet_type,
        MachoPacketType::SessionChangeNotification
    );
    (s, client_id, ack.user_id)
}

fn make_call_req(
    client_id: u64,
    user_id: u64,
    call_id: u64,
    service: &str,
    bound_id: Option<i64>,
    method: &str,
    args: Vec<PyRep>,
) -> MachoPacket {
    let target = match bound_id {
        Some(bid) => PyRep::Integer(bid),
        None => PyRep::Integer(0),
    };
    MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client {
            client_id,
            call_id,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: Some(service.to_string()),
            call_id: 0,
        },
        user_id: Some(user_id),
        payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
            target,
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::ascii_bytes(method),
                PyRep::Tuple(args),
                PyRep::Dict(Vec::new()),
            ]))),
        ])]),
        named_payload: HashMap::new(),
    }
}

#[tokio::test]
async fn test_macho_frame_codec_bounds_and_invalid_frames() {
    // Empty EOF returns None
    let mut eof_cur = Cursor::new(Vec::<u8>::new());
    assert_eq!(read_framed_pyrep(&mut eof_cur).await.unwrap(), None);

    // Zero length frame rejected
    let mut zero_cur = Cursor::new(0u32.to_le_bytes().to_vec());
    assert!(read_framed_pyrep(&mut zero_cur).await.is_err());

    // Oversized frame rejected
    let mut over_cur = Cursor::new(((MAX_MACHO_FRAME_LEN + 1) as u32).to_le_bytes().to_vec());
    assert!(read_framed_pyrep(&mut over_cur).await.is_err());

    // Roundtrip write + read
    let mut rw_buf = Vec::new();
    let sample = PyRep::Tuple(vec![PyRep::Integer(42), PyRep::ascii_bytes("macho")]);
    write_framed_pyrep(&mut rw_buf, &sample).await.unwrap();
    let mut r_cur = Cursor::new(rw_buf);
    assert_eq!(read_framed_pyrep(&mut r_cur).await.unwrap(), Some(sample));
}

#[tokio::test]
async fn test_macho_auth_rejections_bans_and_shutdown_gate() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let _banned_uid = ctx.db.provision_player_account("banned_user", "secret1");
    let _susp_uid = ctx.db.provision_player_account("susp_user", "secret2");
    let _gm_uid = ctx
        .db
        .provision_gm_account("gm_admin", "gm_secret", 90_000_099, "GM Tester");

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;

    // 1. Corrupt marshal bytes -> HANDSHAKE_INCOMPATIBLEPROTOCOL
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        s.write_u32_le(4).await.unwrap();
        s.write_all(&[0xFF, 0xFF, 0xFF, 0xFF]).await.unwrap();
        let closed_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let closed = GPSTransportClosed::from_pyrep(&closed_rep).unwrap();
        assert_eq!(
            closed.reason_code.as_deref(),
            Some("HANDSHAKE_INCOMPATIBLEPROTOCOL")
        );
    }

    // 2. Wrong password -> LoginAuthFailed
    for (user, pass, expected_code) in [
        ("demo", "wrong-pass", "LoginAuthFailed"),
        ("banned_user", "secret1", "UserBanned"),
        ("susp_user", "secret2", "AccountSuspended"),
    ] {
        if user == "banned_user" {
            let _ = ctx.db.authenticate("banned_user", "secret1").unwrap();
            ctx.db
                .ban_account("banned_user", 1_800_086_400, "RMT")
                .unwrap();
        } else if user == "susp_user" {
            let _ = ctx.db.authenticate("susp_user", "secret2").unwrap();
            ctx.db
                .suspend_account("susp_user", 1_800_007_200, "Cooldown")
                .unwrap();
        }
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
            .await
            .unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
            .await
            .unwrap();
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let mut live_req = CryptoChallengePacket::new_password_login(user, pass);
        live_req.user_password = None;
        write_framed_pyrep(&mut s, &live_req.to_pyrep())
            .await
            .unwrap();
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();

        let chal = CryptoChallengePacket::new_password_login(user, pass);
        write_framed_pyrep(&mut s, &chal.to_pyrep()).await.unwrap();
        let closed_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let closed = GPSTransportClosed::from_pyrep(&closed_rep).unwrap();
        assert_eq!(closed.reason_code.as_deref(), Some(expected_code));
    }

    // 3. Shutdown rejects non-GM login (`ClusterShuttingDown`) while allowing GM login
    ctx.cluster_state.write().unwrap().shutdown_in_progress = true;
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
            .await
            .unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
            .await
            .unwrap();
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let chal = CryptoChallengePacket::new_password_login("demo", "demo-secret");
        write_framed_pyrep(&mut s, &chal.to_pyrep()).await.unwrap();
        let closed = GPSTransportClosed::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap())
            .unwrap();
        assert_eq!(closed.reason_code.as_deref(), Some("ClusterShuttingDown"));
    }

    let (gm_stream, _gm_cid, _gm_uid) =
        connect_and_authenticate(addr, "gm_admin", "gm_secret").await;
    drop(gm_stream);
}

#[tokio::test]
async fn test_macho_ping_rpc_errors_bound_oid_and_displacement() {
    let ctx = Arc::new(ServerContext::new());
    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;

    let (mut s1, client_id, user_id) = connect_and_authenticate(addr, "demo", "demo-secret").await;

    // 1. PingReq appends 9 turnaround timing tuples in PingRsp
    let ping_pkt = MachoPacket {
        packet_type: MachoPacketType::PingReq,
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
        payload: PyRep::Tuple(vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(100),
            PyRep::Integer(200),
            PyRep::ascii_bytes("client::send"),
        ])])]),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s1, &ping_pkt.to_pyrep())
        .await
        .unwrap();
    let ping_rsp =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s1).await.unwrap().unwrap()).unwrap();
    assert_eq!(ping_rsp.packet_type, MachoPacketType::PingRsp);
    if let PyRep::Tuple(items) = &ping_rsp.payload {
        if let Some(PyRep::List(entries)) = items.first() {
            assert_eq!(entries.len(), 10);
        } else {
            panic!("expected PingRsp list");
        }
    } else {
        panic!("expected PingRsp tuple");
    }

    // 2. Duplicate AuthenticationReq post-handshake returns ErrorResponse (AlreadyAuthenticated)
    let dup_auth = MachoPacket {
        packet_type: MachoPacketType::AuthenticationReq,
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
        payload: PyRep::None,
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s1, &dup_auth.to_pyrep())
        .await
        .unwrap();
    let dup_rsp =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s1).await.unwrap().unwrap()).unwrap();
    assert_eq!(dup_rsp.packet_type, MachoPacketType::ErrorResponse);

    // 3. UnimplementedRPC -> wrapped RuntimeError ErrorResponse
    let unimpl_call = make_call_req(
        client_id,
        user_id,
        3,
        "machoNet",
        None,
        "",
        Vec::new(),
    );
    write_framed_pyrep(&mut s1, &unimpl_call.to_pyrep())
        .await
        .unwrap();
    let err_rsp =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s1).await.unwrap().unwrap()).unwrap();
    assert_eq!(err_rsp.packet_type, MachoPacketType::ErrorResponse);

    // 4. SelectCharacterID -> MachoBindObject (OID+ header) -> Undock -> DoDestinyUpdate tick
    async fn read_next_rpc_rsp(stream: &mut TcpStream) -> MachoPacket {
        loop {
            let pkt = MachoPacket::from_pyrep(
                &tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    read_framed_pyrep(stream),
                )
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            )
            .unwrap();
            if matches!(
                pkt.packet_type,
                MachoPacketType::CallRsp | MachoPacketType::ErrorResponse
            ) {
                return pkt;
            }
        }
    }

    let sel_call = make_call_req(
        client_id,
        user_id,
        4,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        vec![PyRep::Integer(90_000_001)],
    );
    write_framed_pyrep(&mut s1, &sel_call.to_pyrep())
        .await
        .unwrap();
    let sel_rsp = read_next_rpc_rsp(&mut s1).await;
    assert_eq!(sel_rsp.packet_type, MachoPacketType::CallRsp);

    // Undock from station via stationSvc.Undock, then bind beyonce in space
    let undock_call = make_call_req(
        client_id,
        user_id,
        5,
        "stationSvc",
        None,
        "Undock",
        Vec::new(),
    );
    write_framed_pyrep(&mut s1, &undock_call.to_pyrep())
        .await
        .unwrap();
    let undock_rsp = read_next_rpc_rsp(&mut s1).await;
    assert_eq!(undock_rsp.packet_type, MachoPacketType::CallRsp);

    let bind_call = make_call_req(
        client_id,
        user_id,
        6,
        "beyonce",
        None,
        "MachoBindObject",
        vec![PyRep::Integer(30_000_142), PyRep::None],
    );
    write_framed_pyrep(&mut s1, &bind_call.to_pyrep())
        .await
        .unwrap();
    let bind_rsp = read_next_rpc_rsp(&mut s1).await;
    assert_eq!(bind_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(bind_rsp.named_payload.contains_key("OID+"));

    // Step Jita system actor to emit DoDestinyUpdate over TCP
    let jita = ctx.systems.get_or_spawn(30_000_142);
    let _ = jita.step_manual(1.0).await.unwrap();
    let destiny_pkt = loop {
        let pkt = MachoPacket::from_pyrep(
            &tokio::time::timeout(
                std::time::Duration::from_secs(2),
                read_framed_pyrep(&mut s1),
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        )
        .unwrap();
        if pkt.packet_type == MachoPacketType::Notification {
            break pkt;
        }
    };
    assert_eq!(destiny_pkt.packet_type, MachoPacketType::Notification);

    // 5. Concurrent login on same account displaces s1 with ACL_DISPLACED
    let (s2, _cid2, _uid2) = connect_and_authenticate(addr, "demo", "demo-secret").await;
    let disp_frame = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        read_framed_pyrep(&mut s1),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let disp_closed = GPSTransportClosed::from_pyrep(&disp_frame).unwrap();
    assert_eq!(disp_closed.reason_code.as_deref(), Some("ACL_DISPLACED"));

    // Drop s2 and confirm disconnect cleanup releases online lock
    drop(s2);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(ctx.db.get_online_session(user_id).is_none());
}

#[tokio::test]
async fn select_character_restores_ceo_corprole_and_scoped_roles_over_machonet() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let corp_id = 98_100_001;
    let ceo_id = 90_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.corporation_id = corp_id;
        }
        let mut corp = eve_db::CorporationRecord {
            corporation_id: corp_id,
            name: "Playtest Corp".into(),
            ticker: "PLAY".into(),
            ceo_id,
            alliance_id: None,
            tax_rate: 0.05,
            master_wallet: eve_db::IskCents(0),
            members: std::collections::HashSet::from([ceo_id]),
            member_roles: HashMap::from([(ceo_id, u64::MAX)]),
            applications: HashMap::new(),
            wallet_divisions: vec![],
            description: String::new(),
            url: String::new(),
            hq_station_id: Some(60_003_760),
        };
        corp.members.insert(ceo_id);
        snap.corporations.insert(corp_id, corp);
    });

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) = connect_and_authenticate(addr, "demo", "demo-secret").await;
    let sel_req = make_call_req(
        client_id,
        user_id,
        1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        vec![PyRep::Integer(ceo_id)],
    );
    write_framed_pyrep(&mut s, &sel_req.to_pyrep()).await.unwrap();

    let mut sess_changes: Option<PyRep> = None;
    let mut got_rsp = false;
    for _ in 0..8 {
        let pkt = MachoPacket::from_pyrep(
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
        if pkt.packet_type == MachoPacketType::CallRsp {
            got_rsp = true;
        } else if pkt.packet_type == MachoPacketType::SessionChangeNotification {
            sess_changes = Some(pkt.payload);
        }
        if got_rsp && sess_changes.is_some() {
            break;
        }
    }
    assert!(got_rsp, "SelectCharacterID CallRsp missing");
    let payload = sess_changes.expect("SessionChangeNotification missing");
    let PyRep::Tuple(outer) = payload else { panic!("expected Tuple payload") };
    let PyRep::Tuple(inner) = &outer[1] else { panic!("expected inner tuple") };
    let PyRep::Dict(entries) = &inner[1] else { panic!("expected Dict changes") };
    let lookup = |k: &str| {
        entries
            .iter()
            .find(|(ek, _)| ek.as_str() == Some(k))
            .map(|(_, v)| v.clone())
    };
    let expected = PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(-1)]);
    assert_eq!(lookup("corprole"), Some(expected.clone()));
    assert_eq!(lookup("rolesAtAll"), Some(expected.clone()));
    assert_eq!(lookup("rolesAtHQ"), Some(expected.clone()));
    assert_eq!(lookup("rolesAtBase"), Some(expected.clone()));
    assert_eq!(lookup("rolesAtOther"), Some(expected));
    drop(s);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}
