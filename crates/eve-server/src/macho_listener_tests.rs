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
    async fn read_next_rpc_rsp_with_notifs(
        stream: &mut TcpStream,
    ) -> (MachoPacket, Vec<MachoPacket>) {
        let mut notifs = Vec::new();
        loop {
            let pkt = MachoPacket::from_pyrep(
                &tokio::time::timeout(
                    std::time::Duration::from_secs(60),
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
                return (pkt, notifs);
            }
            notifs.push(pkt);
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
    let (sel_rsp, _) = read_next_rpc_rsp_with_notifs(&mut s1).await;
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
    let (undock_rsp, _) = read_next_rpc_rsp_with_notifs(&mut s1).await;
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
    let (bind_rsp, bind_notifs) = read_next_rpc_rsp_with_notifs(&mut s1).await;
    assert_eq!(bind_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(bind_rsp.named_payload.contains_key("OID+"));
    assert!(
        bind_notifs
            .iter()
            .any(|p| p.packet_type == MachoPacketType::Notification),
        "MachoBindObject must emit DoDestinyUpdate(SetState) before CallRsp (#1083)"
    );

    // Spawn a nearby dynamic ball and step Jita system actor to emit a subsequent DoDestinyUpdate over TCP
    let jita = ctx.systems.get_or_spawn(30_000_142);
    let active_ship_id = ctx.db.get_character(90_000_001).unwrap().active_ship_id;
    let ship_pos = jita
        .get_ball_snapshot(active_ship_id)
        .await
        .unwrap()
        .unwrap()
        .1
        .position;
    jita.spawn_ball(eve_destiny::Ball {
        id: 900_000_020_106,
        type_id: 1230,
        position: ship_pos + eve_destiny::DVec3::new(2_500.0, 0.0, 0.0),
        velocity: eve_destiny::DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1.0e6,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    })
    .await
    .unwrap();
    let _ = jita.step_manual(1.0).await.unwrap();
    let destiny_pkt = loop {
        let pkt = MachoPacket::from_pyrep(
            &tokio::time::timeout(
                std::time::Duration::from_secs(60),
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
        std::time::Duration::from_secs(60),
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
    for _ in 0..500 {
        if ctx.db.get_online_session(user_id).is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
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
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Playtest Corp".into();
        corp.ticker = "PLAY".into();
        corp.ceo_id = ceo_id;
        corp.tax_rate = 0.05;
        corp.members = std::collections::HashSet::from([ceo_id]);
        corp.member_roles = HashMap::from([(ceo_id, u64::MAX)]);
        corp.hq_station_id = Some(60_003_760);
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
                std::time::Duration::from_secs(60),
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
    assert_eq!(
        lookup("raceID"),
        Some(PyRep::Tuple(vec![PyRep::None, PyRep::Integer(2)]))
    );
    assert_eq!(
        lookup("bloodlineID"),
        Some(PyRep::Tuple(vec![PyRep::None, PyRep::Integer(3)]))
    );
    assert_eq!(
        lookup("genderID"),
        Some(PyRep::Tuple(vec![PyRep::None, PyRep::Integer(0)]))
    );
    drop(s);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

#[tokio::test]
async fn select_character_isd_polaris_emits_bloodline_and_gender_session_change() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx.db
        .provision_gm_account("admin", "admin-secret", 90_000_003, "ISD Polaris");
    // Also simulate a legacy DB record missing bloodline_id/race_id to verify SelectCharacterID backfill.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_003) {
            ch.bloodline_id = 0;
            ch.race_id = 0;
        }
    });

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) = connect_and_authenticate(addr, "admin", "admin-secret").await;
    let sel_req = make_call_req(
        client_id,
        user_id,
        1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        vec![PyRep::Integer(90_000_003)],
    );
    write_framed_pyrep(&mut s, &sel_req.to_pyrep()).await.unwrap();

    let mut sess_changes: Option<PyRep> = None;
    let mut got_rsp = false;
    for _ in 0..8 {
        let pkt = MachoPacket::from_pyrep(
            &tokio::time::timeout(
                std::time::Duration::from_secs(60),
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
    assert_eq!(
        lookup("charid"),
        Some(PyRep::Tuple(vec![PyRep::None, PyRep::Integer(90_000_003)]))
    );
    assert_eq!(
        lookup("raceID"),
        Some(PyRep::Tuple(vec![PyRep::None, PyRep::Integer(1)]))
    );
    assert_eq!(
        lookup("bloodlineID"),
        Some(PyRep::Tuple(vec![PyRep::None, PyRep::Integer(1)]))
    );
    assert_eq!(
        lookup("genderID"),
        Some(PyRep::Tuple(vec![PyRep::None, PyRep::Integer(0)]))
    );
    let ch = ctx.db.get_character(90_000_003).unwrap();
    assert_eq!(ch.race_id, 1);
    assert_eq!(ch.bloodline_id, 1);
    drop(s);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

#[tokio::test]
async fn re_undock_after_advance_clock_and_dock_delivers_set_state_and_subsequent_add_balls2() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("demo") {
            acc.role |= eve_machonet::service_roles::ROLE_GMS;
        }
    });
    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) = connect_and_authenticate(addr, "demo", "demo-secret").await;

    fn packet_contains_ascii(rep: &PyRep, needle: &str) -> bool {
        match rep {
            PyRep::String(st) => st.contains(needle),
            PyRep::Bytes(b) => {
                std::str::from_utf8(b).is_ok_and(|st| st.contains(needle))
                    || b.windows(needle.len()).any(|w| w == needle.as_bytes())
            }
            PyRep::Tuple(items) | PyRep::List(items) => {
                items.iter().any(|i| packet_contains_ascii(i, needle))
            }
            PyRep::Dict(entries) => entries
                .iter()
                .any(|(k, v)| packet_contains_ascii(k, needle) || packet_contains_ascii(v, needle)),
            PyRep::SubStream(inner) | PyRep::SubStruct(inner) => packet_contains_ascii(inner, needle),
            PyRep::Object { type_name, args } => {
                type_name.contains(needle) || packet_contains_ascii(args, needle)
            }
            PyRep::ObjectEx { header, list, dict, .. } => {
                packet_contains_ascii(header, needle)
                    || list.iter().any(|i| packet_contains_ascii(i, needle))
                    || dict
                        .iter()
                        .any(|(k, v)| packet_contains_ascii(k, needle) || packet_contains_ascii(v, needle))
            }
            _ => false,
        }
    }

    async fn send_rpc_and_collect_until_rsp(
        stream: &mut TcpStream,
        req: &MachoPacket,
    ) -> (MachoPacket, Vec<MachoPacket>) {
        write_framed_pyrep(stream, &req.to_pyrep()).await.unwrap();
        let mut notifs = Vec::new();
        let rsp = loop {
            let pkt = MachoPacket::from_pyrep(
                &tokio::time::timeout(
                    std::time::Duration::from_secs(60),
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
                break pkt;
            }
            notifs.push(pkt);
        };
        // Drain any post-CallRsp notifications flushed on the same RPC turn (e.g. SessionChangeNotification + DoDestinyUpdate)
        while let Ok(Ok(Some(rep))) = tokio::time::timeout(
            std::time::Duration::from_millis(60),
            read_framed_pyrep(stream),
        )
        .await
        {
            if let Some(pkt) = MachoPacket::from_pyrep(&rep) {
                notifs.push(pkt);
            }
        }
        (rsp, notifs)
    }

    // 1. SelectCharacterID(90_000_001) (starts docked in Jita IV-4 60_003_760)
    let (sel_rsp, _) = send_rpc_and_collect_until_rsp(
        &mut s,
        &make_call_req(
            client_id,
            user_id,
            1,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            vec![PyRep::Integer(90_000_001)],
        ),
    )
    .await;
    assert_eq!(sel_rsp.packet_type, MachoPacketType::CallRsp);

    // 2. First Undock: ship.Undock -> beyonce.GetFormations -> beyonce.MachoBindObject
    let (undock1_rsp, _) = send_rpc_and_collect_until_rsp(
        &mut s,
        &make_call_req(client_id, user_id, 2, "ship", None, "Undock", Vec::new()),
    )
    .await;
    assert_eq!(undock1_rsp.packet_type, MachoPacketType::CallRsp);

    let (form1_rsp, form1_notifs) = send_rpc_and_collect_until_rsp(
        &mut s,
        &make_call_req(
            client_id,
            user_id,
            3,
            "beyonce",
            None,
            "GetFormations",
            Vec::new(),
        ),
    )
    .await;
    assert_eq!(form1_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        form1_notifs
            .iter()
            .any(|p| packet_contains_ascii(&p.payload, "SetState")),
        "1st undock GetFormations must deliver DoDestinyUpdate(SetState) before CallRsp (#1083)"
    );

    let (bind1_rsp, bind1_notifs) = send_rpc_and_collect_until_rsp(
        &mut s,
        &make_call_req(
            client_id,
            user_id,
            4,
            "beyonce",
            None,
            "MachoBindObject",
            vec![PyRep::Integer(30_000_142), PyRep::None],
        ),
    )
    .await;
    assert_eq!(bind1_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !bind1_notifs
            .iter()
            .any(|p| packet_contains_ascii(&p.payload, "SetState")
                || packet_contains_ascii(&p.payload, "DoDestinyUpdate")),
        "1st undock MachoBindObject after GetFormations must NOT replay DoDestinyUpdate (#1083)"
    );

    // 3. Stop ship within docking perimeter, AdvanceClock(11) past 10s session-change timer, + stationSvc.Dock(60_003_760)
    let active_ship_id = ctx
        .db
        .get_character(90_000_001)
        .unwrap()
        .active_ship_id;
    let jita = ctx.systems.get_or_spawn(30_000_142);
    let _ = jita
        .set_movement_mode(active_ship_id, eve_destiny::MovementMode::Stop)
        .await;
    let _ = jita.set_speed_fraction(active_ship_id, 0.0).await;

    let (adv_rsp, _) = send_rpc_and_collect_until_rsp(
        &mut s,
        &make_call_req(
            client_id,
            user_id,
            5,
            "timeControl",
            None,
            "AdvanceClock",
            vec![PyRep::Integer(11)],
        ),
    )
    .await;
    assert_eq!(adv_rsp.packet_type, MachoPacketType::CallRsp);

    let (dock_rsp, _) = send_rpc_and_collect_until_rsp(
        &mut s,
        &make_call_req(
            client_id,
            user_id,
            6,
            "stationSvc",
            None,
            "Dock",
            vec![PyRep::Integer(60_003_760)],
        ),
    )
    .await;
    assert_eq!(dock_rsp.packet_type, MachoPacketType::CallRsp);

    // AdvanceClock(11) past dock session-change timer before 2nd undock
    let (adv2_rsp, _) = send_rpc_and_collect_until_rsp(
        &mut s,
        &make_call_req(
            client_id,
            user_id,
            60,
            "timeControl",
            None,
            "AdvanceClock",
            vec![PyRep::Integer(11)],
        ),
    )
    .await;
    assert_eq!(adv2_rsp.packet_type, MachoPacketType::CallRsp);

    // 4. Second Undock (#802): ship.Undock -> 1 Hz StepManual race -> beyonce.GetFormations -> beyonce.MachoBindObject (no MachoResolveObject)
    let (undock2_rsp, _) = send_rpc_and_collect_until_rsp(
        &mut s,
        &make_call_req(client_id, user_id, 7, "ship", None, "Undock", Vec::new()),
    )
    .await;
    assert_eq!(undock2_rsp.packet_type, MachoPacketType::CallRsp);

    let jita = ctx.systems.get_or_spawn(30_000_142);
    // Simulate 1 Hz background physics tick firing while client transitions hangar -> inflight
    let _ = jita.step_manual(1.0).await.unwrap();

    let active_ship_id = ctx
        .db
        .get_character(90_000_001)
        .unwrap()
        .active_ship_id;
    let _ = jita.drain_observer_updates(active_ship_id).await;

    let (form2_rsp, form2_notifs) = send_rpc_and_collect_until_rsp(
        &mut s,
        &make_call_req(
            client_id,
            user_id,
            8,
            "beyonce",
            None,
            "GetFormations",
            Vec::new(),
        ),
    )
    .await;
    assert_eq!(form2_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        form2_notifs
            .iter()
            .any(|p| packet_contains_ascii(&p.payload, "SetState")),
        "2nd undock GetFormations must deliver DoDestinyUpdate(SetState) even after 1Hz ticks (#802, #1083)"
    );

    let (bind2_rsp, bind2_notifs) = send_rpc_and_collect_until_rsp(
        &mut s,
        &make_call_req(
            client_id,
            user_id,
            9,
            "beyonce",
            None,
            "MachoBindObject",
            vec![
                PyRep::Integer(30_000_142),
                PyRep::Tuple(vec![
                    PyRep::ascii_bytes("GetFormations"),
                    PyRep::Tuple(Vec::new()),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
        ),
    )
    .await;
    assert_eq!(bind2_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !bind2_notifs
            .iter()
            .any(|p| packet_contains_ascii(&p.payload, "SetState")
                || packet_contains_ascii(&p.payload, "DoDestinyUpdate")),
        "2nd undock MachoBindObject after GetFormations must NOT replay DoDestinyUpdate (#1083)"
    );

    // 5. Subsequent `/spawn 1230` (dynamic ball spawn on grid + 1 Hz tick) must stream AddBalls2 over TCP
    let ship_pos = jita
        .get_ball_snapshot(active_ship_id)
        .await
        .unwrap()
        .unwrap()
        .1
        .position;
    jita.spawn_ball(eve_destiny::Ball {
        id: 900_000_020_105,
        type_id: 1230,
        position: ship_pos + eve_destiny::DVec3::new(2_500.0, 0.0, 0.0),
        velocity: eve_destiny::DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1.0e6,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    })
    .await
    .unwrap();
    let _ = jita.step_manual(1.0).await.unwrap();

    let spawn_pkt = MachoPacket::from_pyrep(
        &tokio::time::timeout(
            std::time::Duration::from_secs(60),
            read_framed_pyrep(&mut s),
        )
        .await
        .expect("timed out waiting for post-reundock AddBalls2 DoDestinyUpdate")
        .unwrap()
        .unwrap(),
    )
    .unwrap();
    assert_eq!(spawn_pkt.packet_type, MachoPacketType::Notification);
    assert!(
        packet_contains_ascii(&spawn_pkt.payload, "AddBalls2"),
        "post-reundock spawn must stream AddBalls2 to client ballpark, got {:?}",
        spawn_pkt.payload
    );

    drop(s);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

#[tokio::test]
async fn drain_pending_tick_frames_emits_service_call_for_target_fail_lock_target() {
    use crate::outbound_sequencer::{OutFrame, SessionWireState};
    use crate::router::NotificationTarget;

    let ctx = Arc::new(ServerContext::new());
    let char_id = 90_000_001_i64;
    let target_id = 881_002_i64;
    let client_id = 1_000_000_000_099_u64;

    let mut session = SessionState::new(99);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.live_notify_listener = true;
    ctx.db
        .acquire_online_lock(1, session.session_id, Some(char_id), true, ctx.now_unix_secs())
        .expect("acquire_online_lock");
    ctx.sync_session(&mut session);
    let _ = ctx.drain_notifications_for_char(char_id);

    let mut sys_rx = ctx.systems.get_or_spawn(30_000_142).subscribe();
    let mut subscribed_sys_id = Some(30_000_142);
    let mut wire_state = SessionWireState::new(session.session_id, client_id);
    let mut deferred_ship_id = None;
    let mut wire_refs = CallReqWireRefs {
        client_id,
        sys_rx: &mut sys_rx,
        subscribed_sys_id: &mut subscribed_sys_id,
        wire_state: &mut wire_state,
        deferred_ship_id: &mut deferred_ship_id,
    };

    ctx.notify(
        NotificationTarget::Character(char_id),
        "FailLockTarget",
        "service:target",
        vec![PyRep::Integer(target_id)],
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnTarget",
        "charid",
        vec![PyRep::Integer(target_id), PyRep::String("lost".into())],
    );

    let (out_frames, wire_packets) =
        drain_pending_tick_frames_for_wire_session(&ctx, &mut session, &mut wire_refs);

    assert!(
        out_frames.iter().any(|frame| matches!(
            frame,
            OutFrame::ServiceCall { service, method, args }
                if service == "target"
                    && method == "FailLockTarget"
                    && args == &vec![PyRep::Integer(target_id)]
        )),
        "expected OutFrame::ServiceCall for service:target FailLockTarget, got {out_frames:?}"
    );
    assert!(
        out_frames.iter().any(|frame| matches!(
            frame,
            OutFrame::Notification { event, .. } if event == "OnTarget"
        )),
        "expected OutFrame::Notification for charid OnTarget, got {out_frames:?}"
    );

    let decoded: Vec<MachoPacket> = wire_packets
        .iter()
        .map(|p| MachoPacket::from_pyrep(p).expect("decode wire packet"))
        .collect();
    let expected_payload = PyRep::Tuple(vec![PyRep::Tuple(vec![
        PyRep::Integer(0),
        PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::String("FailLockTarget".into()),
            PyRep::Tuple(vec![PyRep::Integer(target_id)]),
        ]))),
    ])]);
    assert!(
        decoded.iter().any(|pkt| {
            pkt.packet_type == MachoPacketType::Notification
                && matches!(
                    &pkt.destination,
                    MachoAddress::Client {
                        client_id: cid,
                        service: Some(svc),
                        ..
                    } if *cid == client_id && svc == "target"
                )
                && pkt.payload == expected_payload
        }),
        "expected wire Notification targeting client service 'target' with FailLockTarget, got {decoded:?}"
    );
}

#[tokio::test]
async fn test_1140_structure_dock_preserves_ship_id_and_station_machonet_hangar_flush() {
    let ctx = Arc::new(ServerContext::new());
    let client_id = 77_u64;
    let char_id = 90_000_001_i64;
    let ship_id = 200_114_001_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let struct_id = 1_001_896_944_936_i64;

    let mut space_session = SessionState::new(1140);
    space_session.user_id = Some(1);
    space_session.character_id = Some(char_id);
    space_session.ship_id = Some(ship_id);
    space_session.solar_system_id = Some(sys_id);

    // 1. Upwell structure dock (`structure_id` set, `station_id` None) must preserve `shipid`
    //    immediately so `godma.Prime` -> `clientDogmaLocation._MakeShipActive` never sees None.
    let mut struct_docked = space_session.clone();
    struct_docked.structure_id = Some(struct_id);

    let mut deferred = None;
    let (sess_frame, flush_frame) = resolve_deferred_session_change_frame(
        &ctx,
        client_id,
        &space_session,
        &struct_docked,
        &mut deferred,
        false,
    );
    assert!(flush_frame.is_none());
    assert!(
        deferred.is_none(),
        "structure dock must not arm deferred_ship_id timer"
    );
    let wire_rep = sess_frame.expect("structureid session change frame");
    let pkt = MachoPacket::from_pyrep(&wire_rep).expect("decode SessionChange MachoPacket");
    let sc_map: std::collections::HashMap<String, (PyRep, PyRep)> =
        eve_machonet::SessionChange::from_pyrep(&pkt.payload)
            .expect("decode SessionChange payload")
            .changes
            .into_iter()
            .collect();
    assert_eq!(
        sc_map.get("structureid"),
        Some(&(PyRep::None, PyRep::Integer(struct_id)))
    );
    assert!(
        !sc_map.contains_key("shipid"),
        "structure dock SessionChange must preserve shipid without [ship_id, None]: {sc_map:?}"
    );

    // 2. NPC station dock (`station_id` set, `structure_id` None) retains `#1085` deferral, and
    //    live-wire bound `machoNet` calls backed by `invbroker` trigger early hangar flush.
    let mut station_docked = space_session.clone();
    station_docked.station_id = Some(station_id);
    let (st_frame, st_flush) = resolve_deferred_session_change_frame(
        &ctx,
        client_id,
        &space_session,
        &station_docked,
        &mut deferred,
        false,
    );
    assert!(st_flush.is_none());
    assert!(deferred.is_some());
    let st_pkt = MachoPacket::from_pyrep(&st_frame.expect("station session change"))
        .expect("decode station SessionChange MachoPacket");
    let st_map: std::collections::HashMap<String, (PyRep, PyRep)> =
        eve_machonet::SessionChange::from_pyrep(&st_pkt.payload)
            .expect("decode station SessionChange payload")
            .changes
            .into_iter()
            .collect();
    assert_eq!(
        st_map.get("shipid"),
        Some(&(PyRep::Integer(ship_id), PyRep::None))
    );

    let hangar_bid = 55_001_u64;
    let ship_bid = 55_002_u64;
    let dogma_bid = 55_003_u64;
    {
        let mut bounds = ctx.bound_objects.write().unwrap();
        for (bid, svc, obj_id) in [
            (hangar_bid, "invbroker", station_id),
            (ship_bid, "invbroker", ship_id),
            (dogma_bid, "dogmaIM", station_id),
        ] {
            bounds.insert(
                bid,
                crate::router::context::BoundObjectEntry {
                    bound_id: bid,
                    session_id: space_session.session_id,
                    user_id: 1,
                    character_id: char_id,
                    service: svc.into(),
                    object_id: obj_id,
                    oid_str: bid.to_string(),
                    ref_id: 0,
                },
            );
        }
    }

    assert!(is_dock_hangar_flush_rpc(
        &ctx,
        "machoNet",
        Some(hangar_bid),
        "List",
        &[],
        deferred
    ));
    assert!(is_dock_hangar_flush_rpc(
        &ctx,
        "machoNet",
        Some(hangar_bid),
        "GetSelfInvItem",
        &[],
        deferred
    ));
    assert!(is_dock_hangar_flush_rpc(
        &ctx,
        "invbroker",
        Some(hangar_bid),
        "ListByFlags",
        &[],
        deferred
    ));
    assert!(!is_dock_hangar_flush_rpc(
        &ctx,
        "machoNet",
        Some(ship_bid),
        "List",
        &[],
        deferred
    ));
    assert!(!is_dock_hangar_flush_rpc(
        &ctx,
        "machoNet",
        Some(dogma_bid),
        "List",
        &[],
        deferred
    ));
    assert!(!is_dock_hangar_flush_rpc(
        &ctx,
        "machoNet",
        Some(hangar_bid),
        "GetInventoryFromId",
        &[],
        deferred
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1360_safe_logoff_returns_to_charsel_without_transport_closed_and_allows_reentry() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) =
        connect_and_authenticate(addr, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;

    async fn read_rpc_rsp_with_notifs(
        stream: &mut TcpStream,
    ) -> (MachoPacket, Vec<MachoPacket>) {
        let mut notifs = Vec::new();
        loop {
            let rep = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                read_framed_pyrep(stream),
            )
            .await
            .expect("RPC frame timeout")
            .expect("RPC read error")
            .expect("unexpected EOF waiting for CallRsp");
            assert!(
                GPSTransportClosed::from_pyrep(&rep).is_none(),
                "unexpected GPSTransportClosed: {rep:?}"
            );
            let pkt = MachoPacket::from_pyrep(&rep).expect("valid MachoPacket");
            if matches!(
                pkt.packet_type,
                MachoPacketType::CallRsp | MachoPacketType::ErrorResponse
            ) {
                return (pkt, notifs);
            }
            notifs.push(pkt);
        }
    }

    async fn wait_for_safe_logoff_activated(stream: &mut TcpStream) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let rep = tokio::time::timeout(remaining, read_framed_pyrep(stream))
                .await
                .expect("timed out waiting for OnSafeLogoffActivated")
                .expect("read error waiting for OnSafeLogoffActivated")
                .expect("TCP closed instead of clean SafeLogoff");
            assert!(
                GPSTransportClosed::from_pyrep(&rep).is_none(),
                "SafeLogoff must not send GPSTransportClosed (#1360): {rep:?}"
            );
            let pkt = MachoPacket::from_pyrep(&rep).expect("decode post-SafeLogoff packet");
            assert_ne!(
                pkt.packet_type,
                MachoPacketType::SessionChangeNotification,
                "SafeLogoff must not clear session.charid remotely before OnSafeLogoffActivated (#1360)"
            );
            if pkt.packet_type == MachoPacketType::Notification {
                if let MachoAddress::Broadcast { broadcast_id, .. } = &pkt.destination {
                    if broadcast_id == "OnSafeLogoffActivated" {
                        return;
                    }
                }
            }
        }
    }

    fn make_ping(client_id: u64, user_id: u64, call_id: u64) -> MachoPacket {
        MachoPacket {
            packet_type: MachoPacketType::PingReq,
            source: MachoAddress::Client {
                client_id,
                call_id,
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
        }
    }

    // 1. Enter space on char 90_000_001 and bind beyonce.
    write_framed_pyrep(
        &mut s,
        &make_call_req(
            client_id,
            user_id,
            1,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            vec![PyRep::Integer(char_id)],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let (sel_rsp, _) = read_rpc_rsp_with_notifs(&mut s).await;
    assert_eq!(sel_rsp.packet_type, MachoPacketType::CallRsp);

    write_framed_pyrep(
        &mut s,
        &make_call_req(client_id, user_id, 2, "stationSvc", None, "Undock", Vec::new())
            .to_pyrep(),
    )
    .await
    .unwrap();
    let (undock_rsp, _) = read_rpc_rsp_with_notifs(&mut s).await;
    assert_eq!(undock_rsp.packet_type, MachoPacketType::CallRsp);

    write_framed_pyrep(
        &mut s,
        &make_call_req(
            client_id,
            user_id,
            3,
            "beyonce",
            None,
            "MachoBindObject",
            vec![PyRep::Integer(sys_id as i64), PyRep::None],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let (bind_rsp, bind_notifs) = read_rpc_rsp_with_notifs(&mut s).await;
    assert_eq!(bind_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(bind_rsp.named_payload.contains_key("OID+"));
    assert!(bind_notifs
        .iter()
        .any(|p| p.packet_type == MachoPacketType::Notification));

    let online_before = ctx.db.get_online_session(user_id).unwrap();
    let mut gw_sess = SessionState::new(online_before.session_id);
    gw_sess.user_id = Some(user_id);
    gw_sess.character_id = Some(char_id);
    let _gw_rx = ctx
        .quasar_gateway
        .subscribe_session(&gw_sess, Vec::new(), 1_800_000_000);

    // 2. Start SafeLogoff and advance 30s to complete the countdown.
    write_framed_pyrep(
        &mut s,
        &make_call_req(client_id, user_id, 4, "ship", None, "SafeLogoff", Vec::new())
            .to_pyrep(),
    )
    .await
    .unwrap();
    let (logoff_rsp, _) = read_rpc_rsp_with_notifs(&mut s).await;
    assert_eq!(logoff_rsp.packet_type, MachoPacketType::CallRsp);

    ctx.advance_secs(30);
    wait_for_safe_logoff_activated(&mut s).await;

    // 3. Step Jita system tick after ship removal: no post-SafeLogoff DoDestinyUpdate/RemoveBall
    //    and no GPSTransportClosed may be emitted; PingReq must return PingRsp immediately.
    let jita = ctx.systems.get_or_spawn(sys_id);
    let _ = jita.step_manual(1.0).await.unwrap();
    tokio::task::yield_now().await;

    write_framed_pyrep(&mut s, &make_ping(client_id, user_id, 5).to_pyrep())
        .await
        .unwrap();
    let ping_rep = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        read_framed_pyrep(&mut s),
    )
    .await
    .expect("PingRsp timeout at charsel")
    .expect("PingRsp read error")
    .expect("unexpected EOF after SafeLogoff");
    assert!(GPSTransportClosed::from_pyrep(&ping_rep).is_none());
    let ping_pkt = MachoPacket::from_pyrep(&ping_rep).unwrap();
    assert_eq!(ping_pkt.packet_type, MachoPacketType::PingRsp);

    let online_charsel = ctx
        .db
        .get_online_session(user_id)
        .expect("online lock must remain active at character selection");
    assert!(online_charsel.character_id.is_none());
    assert!(ctx.bound_objects.read().unwrap().is_empty());
    {
        let subs = ctx.quasar_gateway.subscribers.read().unwrap();
        let sub = subs
            .get(&online_charsel.session_id)
            .expect("Quasar subscription preserved across SafeLogoff");
        assert_eq!(sub.user_id, Some(user_id as i32));
        assert!(sub.character_id.is_none());
    }

    // 4. Re-enter the world via SelectCharacterID -> beyonce.MachoBindObject on the SAME socket.
    write_framed_pyrep(
        &mut s,
        &make_call_req(
            client_id,
            user_id,
            6,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            vec![PyRep::Integer(char_id)],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let (reselect_rsp, reselect_notifs) = read_rpc_rsp_with_notifs(&mut s).await;
    assert_eq!(reselect_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        reselect_notifs
            .iter()
            .any(|p| p.packet_type == MachoPacketType::SessionChangeNotification),
        "re-entry SelectCharacterID must emit SessionChangeNotification"
    );
    assert_eq!(
        ctx.db.get_online_session(user_id).unwrap().character_id,
        Some(char_id)
    );

    write_framed_pyrep(
        &mut s,
        &make_call_req(
            client_id,
            user_id,
            7,
            "beyonce",
            None,
            "MachoBindObject",
            vec![PyRep::Integer(sys_id as i64), PyRep::None],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let (rebind_rsp, rebind_notifs) = read_rpc_rsp_with_notifs(&mut s).await;
    assert_eq!(rebind_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(rebind_rsp.named_payload.contains_key("OID+"));
    assert!(rebind_notifs
        .iter()
        .any(|p| p.packet_type == MachoPacketType::Notification));

    // 5. Trigger a second SafeLogoff on the same socket to confirm closer registration stayed live.
    write_framed_pyrep(
        &mut s,
        &make_call_req(client_id, user_id, 8, "ship", None, "SafeLogoff", Vec::new())
            .to_pyrep(),
    )
    .await
    .unwrap();
    let (logoff2_rsp, _) = read_rpc_rsp_with_notifs(&mut s).await;
    assert_eq!(logoff2_rsp.packet_type, MachoPacketType::CallRsp);

    ctx.advance_secs(30);
    wait_for_safe_logoff_activated(&mut s).await;

    write_framed_pyrep(&mut s, &make_ping(client_id, user_id, 9).to_pyrep())
        .await
        .unwrap();
    let ping2_rep = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        read_framed_pyrep(&mut s),
    )
    .await
    .expect("second PingRsp timeout")
    .expect("second PingRsp read error")
    .expect("unexpected EOF after second SafeLogoff");
    assert_eq!(
        MachoPacket::from_pyrep(&ping2_rep).unwrap().packet_type,
        MachoPacketType::PingRsp
    );
}

#[tokio::test]
async fn test_1360_safe_logoff_stale_closer_does_not_clobber_active_character() {
    let ctx = ServerContext::new();
    let now_unix = 1_800_000_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    let sid = 77_001_u64;
    let active_char_id = 90_000_001_i64;
    let stale_char_id = 90_000_002_i64;
    ctx.db
        .acquire_online_lock(1, sid, Some(active_char_id), true, now_unix)
        .unwrap();

    let mut session = SessionState::new(sid);
    session.user_id = Some(1);
    session.character_id = Some(active_char_id);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(100_000_001);

    let mut sys_rx = ctx.systems.get_or_spawn(30_000_142).subscribe();
    let mut notify_rx = ctx.notify_tx.subscribe();
    let mut subscribed_sys_id = Some(30_000_142);
    let mut wire_state = crate::outbound_sequencer::SessionWireState::new(sid, 1_000_000_000_001);
    let mut deferred_ship_id = None;
    let mut pending_dock = None;
    let mut safe_logoff_teardown = None;
    let (_client_rd, mut server_wr) = tokio::io::duplex(4096);

    // 1. Mismatched character id reason is ignored.
    {
        let mut wire = SafeLogoffWireContext {
            client_id: 1_000_000_000_001,
            sys_rx: &mut sys_rx,
            notify_rx: &mut notify_rx,
            subscribed_sys_id: &mut subscribed_sys_id,
            wire_state: &mut wire_state,
            deferred_ship_id: &mut deferred_ship_id,
            pending_dock: &mut pending_dock,
            safe_logoff_teardown: &mut safe_logoff_teardown,
        };
        handle_safe_logoff_to_charsel(
            &ctx,
            &mut session,
            &mut wire,
            &mut server_wr,
            &stale_char_id.to_string(),
        )
        .await
        .unwrap();
    }
    assert_eq!(session.character_id, Some(active_char_id));
    assert_eq!(subscribed_sys_id, Some(30_000_142));
    assert!(safe_logoff_teardown.is_none());

    // 2. Late SafeLogoff signal after online lock was already re-acquired for active_char_id is ignored.
    {
        let mut wire = SafeLogoffWireContext {
            client_id: 1_000_000_000_001,
            sys_rx: &mut sys_rx,
            notify_rx: &mut notify_rx,
            subscribed_sys_id: &mut subscribed_sys_id,
            wire_state: &mut wire_state,
            deferred_ship_id: &mut deferred_ship_id,
            pending_dock: &mut pending_dock,
            safe_logoff_teardown: &mut safe_logoff_teardown,
        };
        handle_safe_logoff_to_charsel(
            &ctx,
            &mut session,
            &mut wire,
            &mut server_wr,
            &active_char_id.to_string(),
        )
        .await
        .unwrap();
    }
    assert_eq!(session.character_id, Some(active_char_id));
    assert_eq!(subscribed_sys_id, Some(30_000_142));
}

fn test_1386_packet_contains_ascii(rep: &PyRep, needle: &str) -> bool {
    match rep {
        PyRep::String(st) => st.contains(needle),
        PyRep::Bytes(b) => {
            std::str::from_utf8(b).is_ok_and(|st| st.contains(needle))
                || b.windows(needle.len()).any(|w| w == needle.as_bytes())
        }
        PyRep::Tuple(items) | PyRep::List(items) => items
            .iter()
            .any(|i| test_1386_packet_contains_ascii(i, needle)),
        PyRep::Dict(entries) => entries.iter().any(|(k, v)| {
            test_1386_packet_contains_ascii(k, needle) || test_1386_packet_contains_ascii(v, needle)
        }),
        PyRep::SubStream(inner) | PyRep::SubStruct(inner) => {
            test_1386_packet_contains_ascii(inner, needle)
        }
        PyRep::Object { type_name, args } => {
            type_name.contains(needle) || test_1386_packet_contains_ascii(args, needle)
        }
        PyRep::ObjectEx {
            header, list, dict, ..
        } => {
            test_1386_packet_contains_ascii(header, needle)
                || list.iter().any(|i| test_1386_packet_contains_ascii(i, needle))
                || dict.iter().any(|(k, v)| {
                    test_1386_packet_contains_ascii(k, needle)
                        || test_1386_packet_contains_ascii(v, needle)
                })
        }
        _ => false,
    }
}

fn test_1386_make_ping(client_id: u64, user_id: u64, call_id: u64) -> MachoPacket {
    MachoPacket {
        packet_type: MachoPacketType::PingReq,
        source: MachoAddress::Client {
            client_id,
            call_id,
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
    }
}

async fn test_1386_call_rpc(
    stream: &mut TcpStream,
    client_id: u64,
    user_id: u64,
    call_id: u64,
    service: &str,
    method: &str,
    args: Vec<PyRep>,
) -> (MachoPacket, Vec<MachoPacket>) {
    let req = make_call_req(client_id, user_id, call_id, service, None, method, args);
    write_framed_pyrep(stream, &req.to_pyrep()).await.unwrap();
    let mut notifs = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let rep = tokio::time::timeout(remaining, read_framed_pyrep(stream))
            .await
            .expect("timed out waiting for CallRsp")
            .expect("read error")
            .expect("unexpected EOF waiting for CallRsp");
        assert!(
            GPSTransportClosed::from_pyrep(&rep).is_none(),
            "unexpected GPSTransportClosed during {service}.{method}: {rep:?}"
        );
        let pkt = MachoPacket::from_pyrep(&rep).expect("decode MachoPacket");
        if matches!(
            pkt.packet_type,
            MachoPacketType::CallRsp | MachoPacketType::ErrorResponse
        ) {
            assert_eq!(
                pkt.packet_type,
                MachoPacketType::CallRsp,
                "unexpected ErrorResponse for {service}.{method}: {pkt:?}"
            );
            return (pkt, notifs);
        }
        notifs.push(pkt);
    }
}

#[tokio::test]
async fn test_1386_stale_and_fallback_safe_logoff_emits_transport_closed_without_stealing_notifications(
) {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let char_id = 90_000_001_i64;

    // 1. Fallback safe-logoff disconnect (`"SafeLogoff"`) emits GPSTransportClosed and closes socket.
    let (mut s_fb, fb_client_id, user_id) =
        connect_and_authenticate(addr, "demo", "demo-secret").await;
    let _ = test_1386_call_rpc(
        &mut s_fb,
        fb_client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    let sid_fb = ctx
        .db
        .get_online_session(user_id)
        .expect("fallback online lock present")
        .session_id;
    // Flush any post-SelectCharacterID async notifications before disconnecting the fallback socket.
    write_framed_pyrep(
        &mut s_fb,
        &test_1386_make_ping(fb_client_id, user_id, 2).to_pyrep(),
    )
    .await
    .unwrap();
    loop {
        let rep = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read_framed_pyrep(&mut s_fb),
        )
        .await
        .expect("timed out waiting for fallback pre-disconnect PingRsp")
        .expect("read error waiting for fallback pre-disconnect PingRsp")
        .expect("fallback socket closed prematurely");
        if MachoPacket::from_pyrep(&rep)
            .is_some_and(|p| p.packet_type == MachoPacketType::PingRsp)
        {
            break;
        }
    }
    ctx.db.release_online_lock(user_id, sid_fb);
    ctx.disconnect_session(sid_fb, "SafeLogoff", "Safe logoff completed");

    let closed = loop {
        let frame = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read_framed_pyrep(&mut s_fb),
        )
        .await
        .expect("timed out waiting for fallback GPSTransportClosed")
        .expect("read error waiting for fallback GPSTransportClosed")
        .expect("fallback socket closed without GPSTransportClosed");
        if let Some(c) = GPSTransportClosed::from_pyrep(&frame) {
            break c;
        }
        let pkt = MachoPacket::from_pyrep(&frame).expect("decode pre-close packet");
        assert_eq!(
            pkt.packet_type,
            MachoPacketType::Notification,
            "unexpected non-notification frame before GPSTransportClosed: {pkt:?}"
        );
    };
    assert_eq!(closed.reason_code.as_deref(), Some("SafeLogoff"));
    assert_eq!(closed.reason, "Safe logoff completed");
    let eof = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        read_framed_pyrep(&mut s_fb),
    )
    .await
    .expect("timed out waiting for fallback EOF")
    .expect("read error waiting for fallback EOF");
    assert!(eof.is_none(), "fallback socket must terminate after GPSTransportClosed");
    for _ in 0..200 {
        if !ctx.session_closers.read().unwrap().contains_key(&sid_fb) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(!ctx.session_closers.read().unwrap().contains_key(&sid_fb));

    // 2. Stale bound-only socket closed via `"SafeLogoff"` terminates with GPSTransportClosed
    //    and neither steals nor wipes a live owning session's queued character notifications.
    let (mut s_stale, stale_client_id, _) =
        connect_and_authenticate(addr, "demo", "demo-secret").await;
    let _ = test_1386_call_rpc(
        &mut s_stale,
        stale_client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    let _ = test_1386_call_rpc(
        &mut s_stale,
        stale_client_id,
        user_id,
        2,
        "stationSvc",
        "Undock",
        Vec::new(),
    )
    .await;
    let _ = test_1386_call_rpc(
        &mut s_stale,
        stale_client_id,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(30_000_142), PyRep::None],
    )
    .await;
    let sid_stale = ctx
        .db
        .get_online_session(user_id)
        .expect("stale online lock present")
        .session_id;
    ctx.db.release_online_lock(user_id, sid_stale);

    let (mut s_live, live_client_id, _) =
        connect_and_authenticate(addr, "demo", "demo-secret").await;
    let _ = test_1386_call_rpc(
        &mut s_live,
        live_client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    let sid_live = ctx
        .db
        .get_online_session(user_id)
        .expect("live online lock present")
        .session_id;
    assert_ne!(sid_live, sid_stale);

    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnAccountChange",
        "charid",
        vec![
            PyRep::ascii_bytes("cash"),
            PyRep::Integer(1000),
            PyRep::Integer(4242),
        ],
    );
    ctx.disconnect_session(sid_stale, "SafeLogoff", "Safe logoff completed");

    let stale_closed = loop {
        let frame = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read_framed_pyrep(&mut s_stale),
        )
        .await
        .expect("timed out waiting for stale GPSTransportClosed")
        .expect("read error waiting for stale GPSTransportClosed")
        .expect("stale socket closed without GPSTransportClosed");
        if let Some(c) = GPSTransportClosed::from_pyrep(&frame) {
            break c;
        }
        let pkt = MachoPacket::from_pyrep(&frame).expect("decode pre-close packet on stale socket");
        assert_eq!(
            pkt.packet_type,
            MachoPacketType::Notification,
            "unexpected non-notification frame before stale GPSTransportClosed: {pkt:?}"
        );
    };
    assert_eq!(stale_closed.reason_code.as_deref(), Some("SafeLogoff"));
    assert_eq!(stale_closed.reason, "Safe logoff completed");
    let stale_eof = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        read_framed_pyrep(&mut s_stale),
    )
    .await
    .expect("timed out waiting for stale EOF")
    .expect("read error waiting for stale EOF");
    assert!(stale_eof.is_none(), "stale socket must close after GPSTransportClosed");
    for _ in 0..200 {
        if !ctx.session_closers.read().unwrap().contains_key(&sid_stale) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(!ctx.session_closers.read().unwrap().contains_key(&sid_stale));

    // Confirm the active session receives OnAccountChange and stays healthy on PingRsp.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let rep = tokio::time::timeout(remaining, read_framed_pyrep(&mut s_live))
            .await
            .expect("timed out waiting for OnAccountChange on live session")
            .expect("read error on live session")
            .expect("live session unexpectedly closed");
        assert!(
            GPSTransportClosed::from_pyrep(&rep).is_none(),
            "live session must not receive GPSTransportClosed: {rep:?}"
        );
        let pkt = MachoPacket::from_pyrep(&rep).unwrap();
        if pkt.packet_type == MachoPacketType::Notification {
            if let MachoAddress::Broadcast { broadcast_id, .. } = &pkt.destination {
                if broadcast_id == "OnAccountChange" {
                    break;
                }
            }
        }
    }

    write_framed_pyrep(
        &mut s_live,
        &test_1386_make_ping(live_client_id, user_id, 2).to_pyrep(),
    )
    .await
    .unwrap();
    let ping_rsp = loop {
        let rep = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read_framed_pyrep(&mut s_live),
        )
        .await
        .expect("timed out waiting for live PingRsp")
        .expect("read error waiting for live PingRsp")
        .expect("live session closed before PingRsp");
        let pkt = MachoPacket::from_pyrep(&rep).unwrap();
        if pkt.packet_type == MachoPacketType::PingRsp {
            break pkt;
        }
    };
    assert_eq!(ping_rsp.packet_type, MachoPacketType::PingRsp);
}

#[tokio::test]
async fn test_1386_displaced_safe_logoff_charsel_preserves_live_session_notifications() {
    let ctx = ServerContext::new();
    let now_unix = 1_800_000_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    let char_id = 90_000_001_i64;
    let user_id = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).unwrap().user_id);
    let stale_sid = 77_201_u64;
    let live_sid = 77_202_u64;

    ctx.db
        .acquire_online_lock(user_id, stale_sid, Some(char_id), true, now_unix)
        .unwrap();
    let displaced = ctx
        .db
        .acquire_online_lock(user_id, live_sid, Some(char_id), true, now_unix + 1)
        .unwrap()
        .expect("live_sid must displace stale_sid");
    ctx.disconnect_displaced_session(live_sid, &displaced);

    ctx.emit_typed(
        crate::router::NotificationTarget::Character(char_id),
        crate::router::notify_registry::OnSafeLogoffActivated,
    );
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnAccountChange",
        "charid",
        vec![
            PyRep::ascii_bytes("cash"),
            PyRep::Integer(1000),
            PyRep::Integer(555),
        ],
    );

    let mut stale_session = SessionState::new(stale_sid);
    stale_session.user_id = Some(user_id);
    stale_session.character_id = Some(char_id);
    stale_session.solar_system_id = Some(30_000_142);

    let (_sys_tx, mut sys_rx) = tokio::sync::broadcast::channel(4);
    let mut notify_rx = ctx.notify_tx.subscribe();
    let mut subscribed_sys_id = Some(30_000_142);
    let mut wire_state = crate::outbound_sequencer::SessionWireState::new(stale_sid, 1_000_000_000_001);
    let mut deferred_ship_id = None;
    let mut pending_dock = None;
    let mut safe_logoff_teardown = None;
    let (mut client_rd, mut server_wr) = tokio::io::duplex(4096);

    #[allow(clippy::too_many_arguments)]
    async fn call_charsel(
        ctx: &ServerContext,
        char_id: i64,
        session: &mut SessionState,
        sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
        notify_rx: &mut tokio::sync::broadcast::Receiver<(
            i64,
            crate::router::PendingMachoNotification,
        )>,
        subscribed_sys_id: &mut Option<u32>,
        wire_state: &mut crate::outbound_sequencer::SessionWireState,
        deferred_ship_id: &mut Option<PendingDeferredShipId>,
        pending_dock: &mut Option<PendingDockCall>,
        safe_logoff_teardown: &mut Option<SafeLogoffTeardownGrace>,
        server_wr: &mut tokio::io::DuplexStream,
    ) -> bool {
        let mut wire = SafeLogoffWireContext {
            client_id: 1_000_000_000_001,
            sys_rx,
            notify_rx,
            subscribed_sys_id,
            wire_state,
            deferred_ship_id,
            pending_dock,
            safe_logoff_teardown,
        };
        handle_session_closer_event(
            ctx,
            session,
            &mut wire,
            server_wr,
            "SafeLogoffCharsel".to_string(),
            char_id.to_string(),
        )
        .await
        .unwrap()
    }

    // 1. Live session owns online lock with character_id = Some(char_id).
    assert!(
        call_charsel(
            &ctx,
            char_id,
            &mut stale_session,
            &mut sys_rx,
            &mut notify_rx,
            &mut subscribed_sys_id,
            &mut wire_state,
            &mut deferred_ship_id,
            &mut pending_dock,
            &mut safe_logoff_teardown,
            &mut server_wr,
        )
        .await
    );
    // 2. Live session owns online lock at charsel (character_id = None).
    ctx.db
        .acquire_online_lock(user_id, live_sid, None, false, now_unix + 2)
        .unwrap();
    {
        let mut wire = SafeLogoffWireContext {
            client_id: 1_000_000_000_001,
            sys_rx: &mut sys_rx,
            notify_rx: &mut notify_rx,
            subscribed_sys_id: &mut subscribed_sys_id,
            wire_state: &mut wire_state,
            deferred_ship_id: &mut deferred_ship_id,
            pending_dock: &mut pending_dock,
            safe_logoff_teardown: &mut safe_logoff_teardown,
        };
        handle_safe_logoff_to_charsel(
            &ctx,
            &mut stale_session,
            &mut wire,
            &mut server_wr,
            &char_id.to_string(),
        )
        .await
        .unwrap();
    }
    // 3. Even if live_sid releases its lock, revoked stale_sid must still not steal/clear notifs.
    ctx.db.release_online_lock(user_id, live_sid);
    assert!(
        call_charsel(
            &ctx,
            char_id,
            &mut stale_session,
            &mut sys_rx,
            &mut notify_rx,
            &mut subscribed_sys_id,
            &mut wire_state,
            &mut deferred_ship_id,
            &mut pending_dock,
            &mut safe_logoff_teardown,
            &mut server_wr,
        )
        .await
    );

    assert_eq!(stale_session.character_id, Some(char_id));
    assert_eq!(subscribed_sys_id, Some(30_000_142));
    drop(server_wr);
    assert!(
        read_framed_pyrep(&mut client_rd).await.unwrap().is_none(),
        "displaced/stale SafeLogoffCharsel must write zero frames"
    );

    let surviving = ctx.drain_notifications_for_char(char_id);
    assert!(
        surviving.iter().any(|n| n.event == "OnSafeLogoffActivated"),
        "OnSafeLogoffActivated must remain queued for live session: {surviving:?}"
    );
    assert!(
        surviving.iter().any(|n| n.event == "OnAccountChange"),
        "OnAccountChange must remain queued for live session: {surviving:?}"
    );
}

#[tokio::test]
async fn test_1386_safe_logoff_pre_notification_online_lock_prevents_remove_ball() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) =
        connect_and_authenticate(addr, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;

    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    let _ = test_1386_call_rpc(&mut s, client_id, user_id, 2, "stationSvc", "Undock", Vec::new()).await;

    let (_bind_rsp, bind_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(sys_id as i64), PyRep::None],
    )
    .await;
    assert!(
        bind_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "DoDestinyUpdate")),
        "MachoBindObject must initialize live DoDestinyUpdate stream"
    );

    let jita = ctx.systems.get_or_spawn(sys_id);
    let active_ship_id = ctx.db.get_character(char_id).unwrap().active_ship_id;
    assert!(jita.get_ball_snapshot(active_ship_id).await.unwrap().is_some());

    let _ = test_1386_call_rpc(&mut s, client_id, user_id, 4, "ship", "SafeLogoff", Vec::new()).await;

    // Flush SafeLogoff's pre-timer Stop / SetSpeedFraction(0.0) tick before timer completion.
    let _ = jita.step_manual(1.0).await.unwrap();
    write_framed_pyrep(
        &mut s,
        &test_1386_make_ping(client_id, user_id, 5).to_pyrep(),
    )
    .await
    .unwrap();
    loop {
        let rep = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read_framed_pyrep(&mut s),
        )
        .await
        .expect("timed out waiting for pre-completion PingRsp")
        .expect("read error waiting for pre-completion PingRsp")
        .expect("socket closed before pre-completion PingRsp");
        if MachoPacket::from_pyrep(&rep)
            .is_some_and(|p| p.packet_type == MachoPacketType::PingRsp)
        {
            break;
        }
    }

    // Complete SafeLogoff timer and immediately step the system BEFORE reading frames so
    // the RemoveBall TickBroadcast competes with OnSafeLogoffActivated / SafeLogoffCharsel.
    ctx.advance_secs(30);
    let online = ctx
        .db
        .get_online_session(user_id)
        .expect("online lock must remain reserved at character selection");
    assert!(
        online.character_id.is_none(),
        "online_lock.character_id must already be None before RemoveBall / OnSafeLogoffActivated"
    );
    assert!(
        jita.get_ball_snapshot(active_ship_id)
            .await
            .unwrap()
            .is_none(),
        "ship ball must be removed on safe logoff"
    );
    let _ = jita.step_manual(1.0).await.unwrap();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let rep = tokio::time::timeout(remaining, read_framed_pyrep(&mut s))
            .await
            .expect("timed out waiting for OnSafeLogoffActivated")
            .expect("read error waiting for OnSafeLogoffActivated")
            .expect("TCP closed instead of clean SafeLogoff");
        assert!(
            GPSTransportClosed::from_pyrep(&rep).is_none(),
            "SafeLogoff must not send GPSTransportClosed: {rep:?}"
        );
        assert!(
            !test_1386_packet_contains_ascii(&rep, "DoDestinyUpdate")
                && !test_1386_packet_contains_ascii(&rep, "RemoveBall"),
            "RemoveBall / DoDestinyUpdate must NOT be emitted after safe logoff: {rep:?}"
        );
        let pkt = MachoPacket::from_pyrep(&rep).unwrap();
        assert_ne!(
            pkt.packet_type,
            MachoPacketType::SessionChangeNotification,
            "SafeLogoff must not emit SessionChangeNotification"
        );
        if pkt.packet_type == MachoPacketType::Notification {
            if let MachoAddress::Broadcast { broadcast_id, .. } = &pkt.destination {
                if broadcast_id == "OnSafeLogoffActivated" {
                    break;
                }
            }
        }
    }

    // Subsequent system step + PingReq confirms sys_rx stays silenced and socket stays open at charsel.
    let _ = jita.step_manual(1.0).await.unwrap();
    write_framed_pyrep(
        &mut s,
        &test_1386_make_ping(client_id, user_id, 6).to_pyrep(),
    )
    .await
    .unwrap();
    loop {
        let rep = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read_framed_pyrep(&mut s),
        )
        .await
        .expect("timed out waiting for PingRsp")
        .expect("read error waiting for PingRsp")
        .expect("socket closed before PingRsp");
        assert!(GPSTransportClosed::from_pyrep(&rep).is_none());
        assert!(
            !test_1386_packet_contains_ascii(&rep, "DoDestinyUpdate")
                && !test_1386_packet_contains_ascii(&rep, "RemoveBall"),
            "RemoveBall / DoDestinyUpdate must NOT leak after safe logoff: {rep:?}"
        );
        let pkt = MachoPacket::from_pyrep(&rep).unwrap();
        assert_ne!(pkt.packet_type, MachoPacketType::SessionChangeNotification);
        if pkt.packet_type == MachoPacketType::PingRsp {
            break;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bf1344_03_macho_handshake_telemetry_upgrade_and_disconnect_cleanup() {
    use crate::log_ring_buffer::{LogRingBuffer, LogRingLayer};
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::layer::SubscriberExt;

    // 1. Direct build extraction across VersionExchange, CryptoChallengePacket, and MachoPacket wrappers.
    let stale_vx = VersionExchange {
        build_version: 3_600_000,
        ..VersionExchange::default()
    };
    assert_eq!(
        extract_handshake_client_build(&stale_vx.to_pyrep()),
        Some(3_600_000)
    );
    let ch = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    assert_eq!(extract_handshake_client_build(&ch.to_pyrep()), Some(ch.boot_build));

    let ring = Arc::new(LogRingBuffer::new(128));
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::registry().with(LogRingLayer::new(Arc::clone(&ring))),
    );

    let ctx = Arc::new(ServerContext::with_repository(Arc::new(
        eve_db::WorldRepository::with_demo_seed(),
    )));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().unwrap();
    let accept_ctx = Arc::clone(&ctx);
    let accept_dispatch = dispatch.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let c = Arc::clone(&accept_ctx);
            let sub = accept_dispatch.clone();
            tokio::spawn(async move {
                let _ = handle_macho_client(stream, c).with_subscriber(sub).await;
            });
        }
    });

    // 2. Stale client handshake is rejected with Boot Build Mismatch and emits WARN telemetry.
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &stale_vx.to_pyrep())
            .await
            .unwrap();
        let reply = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let closed = GPSTransportClosed::from_pyrep(&reply).expect("expected GPSTransportClosed");
        assert!(
            closed.reason.contains("Boot Build Mismatch")
                || closed.reason_code.as_deref() == Some("HANDSHAKE_INCOMPATIBLEBUILD"),
            "unexpected closure: {closed:?}"
        );
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let warn_lines = ring.snapshot_recent(50);
    assert!(
        warn_lines.iter().any(|l| {
            l.contains("WARN")
                && l.contains("Rejected MachoNet client handshake: Boot Build Mismatch")
                && l.contains("client_build=3600000")
        }),
        "expected Boot Build Mismatch WARN in {warn_lines:?}"
    );

    // 3. Seed prior mismatch & gateway count on user_id=1, plus pre-auth churn >512 sessions;
    //    then authenticate cleanly and verify reset + non-eviction + disconnect cleanup.
    ctx.flight_recorder.record_handshake_builds(
        80_001,
        Some(1),
        Some(3_600_000),
        eve_machonet::V24_BOOT_BUILD,
        true,
    );
    ctx.flight_recorder.record_gateway_request(Some(80_001), Some(1));

    let (sock, _client_id, uid) = connect_and_authenticate(addr, "demo", "demo-secret").await;
    assert_eq!(uid, 1);
    let active_sid = ctx
        .db
        .get_online_session(uid)
        .expect("online session active")
        .session_id;

    // Simulate >512 pre-auth rejections AFTER active_sid authenticated; must NOT evict active_sid.
    for junk_sid in 900_000..900_600 {
        ctx.flight_recorder.record_handshake_builds(
            junk_sid,
            None,
            Some(3_500_000),
            eve_machonet::V24_BOOT_BUILD,
            true,
        );
    }

    let live_tele = ctx.flight_recorder.get_session_telemetry(active_sid, Some(uid));
    assert_eq!(live_tele.gateway_request_count, 0);
    let live_hs = live_tele.handshake.expect("active session handshake preserved");
    assert_eq!(live_hs.client_boot_build, Some(eve_machonet::V24_BOOT_BUILD));
    assert_eq!(live_hs.build_mismatch, Some(false));

    // User-keyed lookup (session_id=0) also reflects clean reset after upgrade.
    let user_tele = ctx.flight_recorder.get_session_telemetry(0, Some(uid));
    assert_eq!(user_tele.gateway_request_count, 0);
    assert_eq!(
        user_tele.handshake.as_ref().and_then(|h| h.build_mismatch),
        Some(false)
    );

    // Drop client socket -> disconnect cleanup clears per-session telemetry maps.
    drop(sock);
    let mut cleaned = false;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let post = ctx.flight_recorder.get_session_telemetry(active_sid, None);
        if post.handshake.is_none()
            && post.server_request_count == 0
            && post.gateway_request_count == 0
        {
            cleaned = true;
            break;
        }
    }
    assert!(cleaned, "disconnect did not clear session telemetry for {active_sid}");
}

#[allow(clippy::too_many_arguments)]
async fn test_1386_call_rpc_raw(
    stream: &mut TcpStream,
    client_id: u64,
    user_id: u64,
    call_id: u64,
    service: &str,
    bound_id: Option<i64>,
    method: &str,
    args: Vec<PyRep>,
) -> (MachoPacket, Vec<MachoPacket>) {
    let req = make_call_req(client_id, user_id, call_id, service, bound_id, method, args);
    write_framed_pyrep(stream, &req.to_pyrep()).await.unwrap();
    let mut notifs = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let rep = tokio::time::timeout(remaining, read_framed_pyrep(stream))
            .await
            .expect("timed out waiting for CallRsp/ErrorResponse")
            .expect("read error")
            .expect("unexpected EOF waiting for CallRsp/ErrorResponse");
        assert!(
            GPSTransportClosed::from_pyrep(&rep).is_none(),
            "unexpected GPSTransportClosed during {service}.{method}: {rep:?}"
        );
        let pkt = MachoPacket::from_pyrep(&rep).expect("decode MachoPacket");
        if matches!(
            pkt.packet_type,
            MachoPacketType::CallRsp | MachoPacketType::ErrorResponse
        ) {
            return (pkt, notifs);
        }
        notifs.push(pkt);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1386_advance_clock_rpc_delivers_safe_logoff_activated_and_unskewed_timer() {
    let base_unix = 1_800_000_000_u64;
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) =
        connect_and_authenticate(addr, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;

    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        2,
        "stationSvc",
        "Undock",
        Vec::new(),
    )
    .await;
    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(sys_id as i64), PyRep::None],
    )
    .await;

    // 1. Advance server virtual clock by +447s via `timeControl.AdvanceClock` (without sim rebase)
    //    from a separate GM session (matching playtester harness).
    let mut gm_sess = SessionState::new(99_138_601);
    gm_sess.user_id = Some(2);
    gm_sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    let empty = HashMap::new();
    ctx.dispatch_rpc(
        &mut gm_sess,
        "timeControl",
        None,
        "AdvanceClock",
        &[PyRep::Integer(447)],
        &empty,
        None,
    )
    .await
    .expect("GM AdvanceClock +447s");
    assert_eq!(ctx.unrebased_clock_offset_secs(), 447);

    // 2. Start ship.SafeLogoff: server expiry tracks `now_unix + 30`, while `OnSafeLogoffTimerStarted`
    //    subtracts `unrebased_clock_offset_secs` so V24 `SafeLogoffTimer2` shows 30.0s instead of 477s.
    let (_safe_rsp, safe_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        4,
        "ship",
        "SafeLogoff",
        Vec::new(),
    )
    .await;
    let extract_timer_ft = |pkt: &MachoPacket| -> Option<i64> {
        if pkt.packet_type != MachoPacketType::Notification {
            return None;
        }
        let MachoAddress::Broadcast { broadcast_id, .. } = &pkt.destination else {
            return None;
        };
        if broadcast_id != "OnSafeLogoffTimerStarted" {
            return None;
        }
        let PyRep::Tuple(outer) = &pkt.payload else {
            return None;
        };
        let PyRep::Tuple(inner) = outer.first()? else {
            return None;
        };
        let PyRep::SubStream(sub) = inner.get(1)? else {
            return None;
        };
        let PyRep::Tuple(sub_t) = sub.as_ref() else {
            return None;
        };
        let PyRep::Tuple(body_t) = sub_t.get(1)? else {
            return None;
        };
        let PyRep::Tuple(args) = body_t.get(1)? else {
            return None;
        };
        args.first()?.as_i64()
    };
    let mut timer_started_ft = safe_notifs.iter().find_map(extract_timer_ft);
    if timer_started_ft.is_none() {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while timer_started_ft.is_none() {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let rep = tokio::time::timeout(remaining, read_framed_pyrep(&mut s))
                .await
                .expect("timed out waiting for OnSafeLogoffTimerStarted")
                .expect("read error waiting for OnSafeLogoffTimerStarted")
                .expect("EOF waiting for OnSafeLogoffTimerStarted");
            let pkt = MachoPacket::from_pyrep(&rep).expect("decode OnSafeLogoffTimerStarted");
            timer_started_ft = extract_timer_ft(&pkt);
        }
    }
    let expected_client_ft = crate::sp6_lifecycle::unix_to_filetime(base_unix + 30);
    assert_eq!(
        timer_started_ft,
        Some(expected_client_ft),
        "OnSafeLogoffTimerStarted must anchor to client sim clock (base+30s), not skewed base+477s"
    );
    let stored_complete_at = crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        st.safe_logoffs.get(&char_id).map(|e| e.complete_at_unix)
    });
    assert_eq!(stored_complete_at, Some(base_unix + 447 + 30));

    // 3. Complete SafeLogoff via `timeControl.AdvanceClock(30)` inside `dispatch_rpc`
    //    (`with_staged_dispatch_notifications`): player's live TCP socket MUST still receive
    //    `OnSafeLogoffActivated` cleanly before transitioning to charsel (Root Cause 1).
    ctx.dispatch_rpc(
        &mut gm_sess,
        "timeControl",
        None,
        "AdvanceClock",
        &[PyRep::Integer(30)],
        &empty,
        None,
    )
    .await
    .expect("GM AdvanceClock +30s");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let rep = tokio::time::timeout(remaining, read_framed_pyrep(&mut s))
            .await
            .expect("timed out waiting for OnSafeLogoffActivated after AdvanceClock RPC")
            .expect("read error waiting for OnSafeLogoffActivated")
            .expect("TCP closed instead of clean SafeLogoff");
        assert!(
            GPSTransportClosed::from_pyrep(&rep).is_none(),
            "SafeLogoff must not send GPSTransportClosed: {rep:?}"
        );
        let pkt = MachoPacket::from_pyrep(&rep).expect("decode post-AdvanceClock frame");
        assert_ne!(
            pkt.packet_type,
            MachoPacketType::SessionChangeNotification,
            "SafeLogoff must not emit SessionChangeNotification before OnSafeLogoffActivated"
        );
        if pkt.packet_type == MachoPacketType::Notification {
            if let MachoAddress::Broadcast { broadcast_id, .. } = &pkt.destination {
                if broadcast_id == "OnSafeLogoffActivated" {
                    break;
                }
            }
        }
    }
    let online = ctx
        .db
        .get_online_session(user_id)
        .expect("online lock preserved at charsel");
    assert!(online.character_id.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1386_safe_logoff_teardown_grace_serves_v24_logoff_rpcs_and_rejects_mutations() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) =
        connect_and_authenticate(addr, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;

    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        2,
        "stationSvc",
        "Undock",
        Vec::new(),
    )
    .await;
    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(sys_id as i64), PyRep::None],
    )
    .await;
    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        4,
        "ship",
        "SafeLogoff",
        Vec::new(),
    )
    .await;

    ctx.advance_secs(30);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let rep = tokio::time::timeout(remaining, read_framed_pyrep(&mut s))
            .await
            .expect("timed out waiting for OnSafeLogoffActivated")
            .expect("read error")
            .expect("EOF");
        let pkt = MachoPacket::from_pyrep(&rep).unwrap();
        if pkt.packet_type == MachoPacketType::Notification {
            if let MachoAddress::Broadcast { broadcast_id, .. } = &pkt.destination {
                if broadcast_id == "OnSafeLogoffActivated" {
                    break;
                }
            }
        }
    }
    let session_id = ctx
        .db
        .get_online_session(user_id)
        .expect("online lock at charsel")
        .session_id;

    // 1. All 3 V24 GameUI.DoCharacterLogoff teardown RPCs (plus bound skillMgr2.GetSkills and
    //    charUnboundMgr.GetCharacterSelectionData) succeed with CallRsp and zero CharacterNotSelected.
    let (skill_rsp, skill_notifs) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        10,
        "skillMgr2",
        None,
        "GetMySkillHandler",
        Vec::new(),
    )
    .await;
    assert_eq!(skill_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        skill_rsp.named_payload.contains_key("OID+"),
        "skillMgr2.GetMySkillHandler must include OID+ header"
    );
    assert!(
        skill_notifs
            .iter()
            .all(|p| p.packet_type != MachoPacketType::SessionChangeNotification)
    );
    let temp_skill_bid = ctx
        .bound_objects
        .read()
        .unwrap()
        .values()
        .find(|e| e.session_id == session_id && e.service == "skillMgr2")
        .map(|e| e.bound_id)
        .expect("temporary skillMgr2 bound object created");

    let (bound_skills_rsp, _) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        11,
        "machoNet",
        Some(temp_skill_bid as i64),
        "GetSkills",
        Vec::new(),
    )
    .await;
    assert_eq!(bound_skills_rsp.packet_type, MachoPacketType::CallRsp);

    let (cash_rsp, _) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        12,
        "account",
        None,
        "GetCashBalance",
        vec![PyRep::Integer(0)],
    )
    .await;
    assert_eq!(cash_rsp.packet_type, MachoPacketType::CallRsp);

    let (bm_rsp, _) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        13,
        "accessGroupBookmarkMgr",
        None,
        "GetMyActiveBookmarks",
        Vec::new(),
    )
    .await;
    assert_eq!(bm_rsp.packet_type, MachoPacketType::CallRsp);

    let (charsel_rsp, _) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        14,
        "charUnboundMgr",
        None,
        "GetCharacterSelectionData",
        Vec::new(),
    )
    .await;
    assert_eq!(charsel_rsp.packet_type, MachoPacketType::CallRsp);

    // 2. Disallowed / corp / mutating RPCs during teardown grace MUST still fail with CharacterNotSelected.
    let (corp_cash_rsp, _) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        15,
        "account",
        None,
        "GetCashBalance",
        vec![PyRep::Integer(1)],
    )
    .await;
    assert_eq!(corp_cash_rsp.packet_type, MachoPacketType::ErrorResponse);
    assert!(test_1386_packet_contains_ascii(
        &corp_cash_rsp.to_pyrep(),
        "CharacterNotSelected"
    ));

    let (save_q_rsp, _) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        16,
        "machoNet",
        Some(temp_skill_bid as i64),
        "SaveSkillQueue",
        vec![PyRep::List(Vec::new())],
    )
    .await;
    assert_eq!(save_q_rsp.packet_type, MachoPacketType::ErrorResponse);

    let (undock_rsp, _) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        17,
        "ship",
        None,
        "Undock",
        Vec::new(),
    )
    .await;
    assert_eq!(undock_rsp.packet_type, MachoPacketType::ErrorResponse);
    assert!(test_1386_packet_contains_ascii(
        &undock_rsp.to_pyrep(),
        "CharacterNotSelected"
    ));

    // Online lock remained at charsel throughout teardown grace.
    assert!(ctx
        .db
        .get_online_session(user_id)
        .expect("online lock still present")
        .character_id
        .is_none());

    // 3. Re-entering via SelectCharacterID clears the temporary teardown skillMgr2 binding cleanly.
    let (reenter_rsp, _) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        18,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    assert_eq!(reenter_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !ctx.bound_objects.read().unwrap().contains_key(&temp_skill_bid),
        "SelectCharacterID must clear temporary teardown skillMgr2 binding"
    );
}

#[tokio::test]
async fn test_1386_safe_logoff_teardown_grace_expires_and_clears_temp_bindings() {
    let ctx = ServerContext::new();
    let now_unix = 1_800_000_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    let user_id = 1_u64;
    let sid = 88_138_699_u64;
    let char_id = 90_000_001_i64;
    ctx.db
        .acquire_online_lock(user_id, sid, None, true, now_unix)
        .unwrap();

    let mut session = SessionState::new(sid);
    session.user_id = Some(user_id);
    session.character_id = None;

    let skill_bid = 555_001_u64;
    let other_bid = 555_002_u64;
    {
        let mut bound = ctx.bound_objects.write().unwrap();
        bound.insert(
            skill_bid,
            crate::router::BoundObjectEntry {
                bound_id: skill_bid,
                session_id: sid,
                user_id,
                character_id: char_id,
                service: "skillMgr2".into(),
                object_id: char_id,
                oid_str: "555001".into(),
                ref_id: 0,
            },
        );
        bound.insert(
            other_bid,
            crate::router::BoundObjectEntry {
                bound_id: other_bid,
                session_id: sid,
                user_id,
                character_id: 0,
                service: "publicGatewaySvc".into(),
                object_id: 0,
                oid_str: "555002".into(),
                ref_id: 0,
            },
        );
    }

    // 1. Expired grace clears itself, rejects read-only RPC, purges temp skillMgr2 binding,
    //    and preserves unrelated charsel bindings.
    let mut expired = Some(SafeLogoffTeardownGrace {
        char_id,
        deadline: tokio::time::Instant::now(),
        temp_bindings_created: true,
    });
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    assert!(resolve_safe_logoff_teardown_grace(
        &ctx,
        &session,
        &mut expired,
        "account",
        None,
        "GetCashBalance",
        &[PyRep::Integer(0)],
        &HashMap::new(),
    )
    .is_none());
    assert!(expired.is_none());
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&skill_bid));
    assert!(ctx.bound_objects.read().unwrap().contains_key(&other_bid));

    // 2. Active grace fails closed if another session displaces the account's online lock.
    ctx.db
        .acquire_online_lock(user_id, sid + 1, Some(char_id), true, now_unix + 1)
        .unwrap();
    let mut displaced = Some(SafeLogoffTeardownGrace {
        char_id,
        deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(10),
        temp_bindings_created: false,
    });
    assert!(resolve_safe_logoff_teardown_grace(
        &ctx,
        &session,
        &mut displaced,
        "accessGroupBookmarkMgr",
        None,
        "GetMyActiveBookmarks",
        &[],
        &HashMap::new(),
    )
    .is_none());
    assert!(displaced.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1434_teardown_grace_suppresses_quasar_session_change_and_preserves_cached_notice() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) =
        connect_and_authenticate(addr, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;

    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        2,
        "stationSvc",
        "Undock",
        Vec::new(),
    )
    .await;
    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(sys_id as i64), PyRep::None],
    )
    .await;

    let session_id = ctx
        .db
        .get_online_session(user_id)
        .expect("online lock in space")
        .session_id;
    let mut sub_sess = SessionState::new(session_id);
    sub_sess.user_id = Some(user_id);
    sub_sess.character_id = Some(char_id);
    let mut notice_rx =
        ctx.quasar_gateway
            .subscribe_session(&sub_sess, Vec::new(), ctx.now_unix());

    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        4,
        "ship",
        "SafeLogoff",
        Vec::new(),
    )
    .await;

    ctx.advance_secs(30);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let rep = tokio::time::timeout(remaining, read_framed_pyrep(&mut s))
            .await
            .expect("timed out waiting for OnSafeLogoffActivated")
            .expect("read error")
            .expect("EOF");
        let pkt = MachoPacket::from_pyrep(&rep).unwrap();
        if pkt.packet_type == MachoPacketType::Notification {
            if let MachoAddress::Broadcast { broadcast_id, .. } = &pkt.destination {
                if broadcast_id == "OnSafeLogoffActivated" {
                    break;
                }
            }
        }
    }

    // Wait for `handle_safe_logoff_to_charsel` to update the live Quasar subscriber to charsel (`character_id = None`).
    let mut transitioned = false;
    for _ in 0..200 {
        if ctx
            .quasar_gateway
            .subscribers
            .read()
            .unwrap()
            .get(&session_id)
            .is_some_and(|sub| {
                sub.character_id.is_none()
                    && sub.corporation_id.is_none()
                    && sub.solar_system_id.is_none()
            })
        {
            transitioned = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        transitioned,
        "live Quasar subscriber did not clear character tenancy on SafeLogoffCharsel"
    );

    // Drain any pre-logoff Quasar notices emitted while the character was still in space.
    while notice_rx.try_recv().is_ok() {}
    let _ = ctx.quasar_gateway.poll_notices(session_id);

    // Publish a character-scoped Quasar notice while the session is at charsel: it must be
    // cached in `NoticeRoutingCache` (within the 10s window) rather than delivered immediately.
    let sentinel_type_url = "type.googleapis.com/eve_public.debug.Notice".to_string();
    let sentinel_bytes = vec![0xCA, 0xFE, 0x14, 0x34];
    let (_, delivered_immediately) = ctx
        .quasar_gateway
        .publish_notice(
            crate::quasar_gateway::NoticeScope::Character(char_id as u32),
            eve_proto::quasar::GoogleAny {
                type_url: sentinel_type_url.clone(),
                value: sentinel_bytes.clone(),
            },
            ctx.now_unix(),
        )
        .expect("publish cached character notice");
    assert_eq!(delivered_immediately, 0);
    assert!(
        ctx.quasar_gateway
            .routing_cache
            .read()
            .unwrap()
            .entries
            .iter()
            .any(|e| {
                e.scope == crate::quasar_gateway::NoticeScope::Character(char_id as u32)
                    && e.notice
                        .payload
                        .as_ref()
                        .is_some_and(|p| p.type_url == sentinel_type_url && p.value == sentinel_bytes)
            }),
        "cached Character notice must be present in NoticeRoutingCache before teardown-grace RPCs"
    );

    // Invoke teardown-grace RPCs: each must succeed AND leave Quasar tenancy + NoticeRoutingCache untouched.
    let (skill_rsp, _) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        10,
        "skillMgr2",
        None,
        "GetMySkillHandler",
        Vec::new(),
    )
    .await;
    assert_eq!(skill_rsp.packet_type, MachoPacketType::CallRsp);

    let (cash_rsp, _) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        11,
        "account",
        None,
        "GetCashBalance",
        vec![PyRep::Integer(0)],
    )
    .await;
    assert_eq!(cash_rsp.packet_type, MachoPacketType::CallRsp);

    let (bm_rsp, _) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        12,
        "accessGroupBookmarkMgr",
        None,
        "GetMyActiveBookmarks",
        Vec::new(),
    )
    .await;
    assert_eq!(bm_rsp.packet_type, MachoPacketType::CallRsp);

    assert!(
        matches!(
            notice_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "teardown-grace RPCs must NOT deliver cached Character Quasar notices to charsel stream"
    );
    assert!(
        ctx.quasar_gateway.poll_notices(session_id).is_empty(),
        "teardown-grace RPCs must NOT enqueue cached Character Quasar notices into charsel inbox"
    );
    assert_eq!(
        ctx.quasar_gateway
            .subscribers
            .read()
            .unwrap()
            .get(&session_id)
            .map(|sub| (sub.character_id, sub.corporation_id, sub.solar_system_id)),
        Some((None, None, None)),
        "live Quasar subscriber must stay cleanly at character_id == None during teardown-grace RPCs"
    );
    assert!(
        ctx.quasar_gateway
            .routing_cache
            .read()
            .unwrap()
            .entries
            .iter()
            .any(|e| {
                e.scope == crate::quasar_gateway::NoticeScope::Character(char_id as u32)
                    && e.notice
                        .payload
                        .as_ref()
                        .is_some_and(|p| p.type_url == sentinel_type_url && p.value == sentinel_bytes)
            }),
        "cached Character notice must remain undrained in NoticeRoutingCache after teardown-grace RPCs"
    );

    // Re-selecting the character clears teardown grace; the next normal RPC (unsuppressed)
    // attaches Quasar character tenancy via `dispatch_rpc_inner` and flushes the cached notice.
    let (reenter_rsp, _) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        13,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    assert_eq!(reenter_rsp.packet_type, MachoPacketType::CallRsp);
    let (post_reenter_cash_rsp, _) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        14,
        "account",
        None,
        "GetCashBalance",
        vec![PyRep::Integer(0)],
    )
    .await;
    assert_eq!(post_reenter_cash_rsp.packet_type, MachoPacketType::CallRsp);
    assert_eq!(
        ctx.quasar_gateway
            .subscribers
            .read()
            .unwrap()
            .get(&session_id)
            .and_then(|sub| sub.character_id),
        Some(char_id as u32),
        "post-SelectCharacterID normal RPC must legitimately attach Quasar character tenancy"
    );
    let mut rx_notices = Vec::new();
    while let Ok(n) = notice_rx.try_recv() {
        rx_notices.push(n);
    }
    let polled_notices = ctx.quasar_gateway.poll_notices(session_id);
    assert!(
        rx_notices.iter().any(|n| {
            n.payload
                .as_ref()
                .is_some_and(|p| p.type_url == sentinel_type_url && p.value == sentinel_bytes)
        }),
        "cached Character notice must be delivered to Quasar stream once SelectCharacterID succeeds"
    );
    assert!(
        polled_notices.iter().any(|n| {
            n.payload
                .as_ref()
                .is_some_and(|p| p.type_url == sentinel_type_url && p.value == sentinel_bytes)
        }),
        "cached Character notice must be delivered to Quasar inbox once SelectCharacterID succeeds"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1434_self_gm_advance_clock_wire_delivers_safe_logoff_without_remove_ball() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let gm_char_id = 90_000_003_i64;
    let sys_id = 30_000_142_u32;
    ctx.db
        .provision_gm_account("admin", "admin-secret", gm_char_id, "ISD Polaris");

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) =
        connect_and_authenticate(addr, "admin", "admin-secret").await;

    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(gm_char_id)],
    )
    .await;
    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        2,
        "stationSvc",
        "Undock",
        Vec::new(),
    )
    .await;
    let (_bind_rsp, bind_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(sys_id as i64), PyRep::None],
    )
    .await;
    assert!(
        bind_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "DoDestinyUpdate")),
        "MachoBindObject must initialize live DoDestinyUpdate stream"
    );

    let jita = ctx.systems.get_or_spawn(sys_id);
    let active_ship_id = ctx.db.get_character(gm_char_id).unwrap().active_ship_id;
    assert!(jita.get_ball_snapshot(active_ship_id).await.unwrap().is_some());

    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        4,
        "ship",
        "SafeLogoff",
        Vec::new(),
    )
    .await;

    // Flush SafeLogoff's pre-timer Stop / SetSpeedFraction(0.0) tick before self-GM AdvanceClock.
    let _ = jita.step_manual(1.0).await.unwrap();
    write_framed_pyrep(
        &mut s,
        &test_1386_make_ping(client_id, user_id, 5).to_pyrep(),
    )
    .await
    .unwrap();
    loop {
        let rep = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read_framed_pyrep(&mut s),
        )
        .await
        .expect("timed out waiting for pre-completion PingRsp")
        .expect("read error waiting for pre-completion PingRsp")
        .expect("socket closed before pre-completion PingRsp");
        if MachoPacket::from_pyrep(&rep)
            .is_some_and(|p| p.packet_type == MachoPacketType::PingRsp)
        {
            break;
        }
    }

    // Complete SafeLogoff via self-GM `timeControl.AdvanceClock(30)` on the SAME TCP connection,
    // exercising the `!post_rpc_charsel_pending` guard in `execute_call_req_wire`.
    let adv_req = make_call_req(
        client_id,
        user_id,
        6,
        "timeControl",
        None,
        "AdvanceClock",
        vec![PyRep::Integer(30)],
    );
    write_framed_pyrep(&mut s, &adv_req.to_pyrep()).await.unwrap();

    let mut got_call_rsp = false;
    let mut got_safe_logoff = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !(got_call_rsp && got_safe_logoff) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let rep = tokio::time::timeout(remaining, read_framed_pyrep(&mut s))
            .await
            .expect("timed out waiting for AdvanceClock CallRsp and OnSafeLogoffActivated")
            .expect("read error after self-GM AdvanceClock")
            .expect("TCP closed instead of clean SafeLogoff");
        assert!(
            GPSTransportClosed::from_pyrep(&rep).is_none(),
            "self-GM SafeLogoff must not send GPSTransportClosed: {rep:?}"
        );
        assert!(
            !test_1386_packet_contains_ascii(&rep, "DoDestinyUpdate")
                && !test_1386_packet_contains_ascii(&rep, "RemoveBall"),
            "self-GM AdvanceClock SafeLogoff must NOT leak DoDestinyUpdate/RemoveBall: {rep:?}"
        );
        let pkt = MachoPacket::from_pyrep(&rep).expect("decode post-AdvanceClock frame");
        assert_ne!(
            pkt.packet_type,
            MachoPacketType::SessionChangeNotification,
            "self-GM SafeLogoff must not emit SessionChangeNotification"
        );
        match pkt.packet_type {
            MachoPacketType::CallRsp => got_call_rsp = true,
            MachoPacketType::Notification => {
                if let MachoAddress::Broadcast { broadcast_id, .. } = &pkt.destination {
                    if broadcast_id == "OnSafeLogoffActivated" {
                        got_safe_logoff = true;
                    }
                }
            }
            MachoPacketType::ErrorResponse => {
                panic!("unexpected ErrorResponse from self-GM AdvanceClock: {pkt:?}");
            }
            _ => {}
        }
    }

    assert!(
        jita.get_ball_snapshot(active_ship_id)
            .await
            .unwrap()
            .is_none(),
        "ship ball must be removed on self-GM safe logoff"
    );
    let online = ctx
        .db
        .get_online_session(user_id)
        .expect("online lock preserved at charsel");
    assert!(online.character_id.is_none());

    let (charsel_rsp, _) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        7,
        "charUnboundMgr",
        None,
        "GetCharacterSelectionData",
        Vec::new(),
    )
    .await;
    assert_eq!(charsel_rsp.packet_type, MachoPacketType::CallRsp);
}
