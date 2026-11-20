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

    let mut sys_rx = Some(ctx.systems.get_or_spawn(30_000_142).subscribe());
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

    let (_sys_tx, raw_sys_rx) = tokio::sync::broadcast::channel(4);
    let mut sys_rx = Some(raw_sys_rx);
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
        sys_rx: &mut Option<tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>>,
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

async fn test_1237_ping_sync(s: &mut TcpStream, client_id: u64, user_id: u64, call_id: u64) -> Vec<MachoPacket> {
    write_framed_pyrep(s, &test_1386_make_ping(client_id, user_id, call_id).to_pyrep())
        .await
        .unwrap();
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let rep = tokio::time::timeout(remaining, read_framed_pyrep(s))
            .await
            .expect("timed out waiting for PingRsp")
            .expect("read error waiting for PingRsp")
            .expect("socket closed before PingRsp");
        let pkt = MachoPacket::from_pyrep(&rep).expect("decode packet during Ping sync");
        if pkt.packet_type == MachoPacketType::PingRsp {
            return seen;
        }
        seen.push(pkt);
    }
}

async fn test_1237_wait_for_notification(s: &mut TcpStream, needle: &str, context: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let rep = tokio::time::timeout(remaining, read_framed_pyrep(s))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {needle} ({context})"))
            .expect("read error waiting for notification")
            .expect("socket closed before notification");
        let pkt = MachoPacket::from_pyrep(&rep).expect("decode packet waiting for notification");
        if pkt.packet_type == MachoPacketType::Notification
            && test_1386_packet_contains_ascii(&rep, needle)
        {
            return;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1237_preauth_and_charsel_connections_do_not_load_or_subscribe_jita() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx.db.provision_player_account("preauth_u1", "pw1");
    ctx.db.provision_player_account("preauth_u2", "pw2");
    let jita_id = 30_000_142_u32;

    assert!(
        !ctx.systems.is_system_loaded(jita_id),
        "fresh ServerContext must not pre-load Jita"
    );

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;

    // 1. Pre-auth TCP connection advancing into the main select! loop must NOT load Jita.
    let mut preauth_s = TcpStream::connect(addr).await.unwrap();
    let _hello = read_framed_pyrep(&mut preauth_s).await.unwrap().unwrap();
    write_framed_pyrep(&mut preauth_s, &VersionExchange::default().to_pyrep())
        .await
        .unwrap();
    write_framed_pyrep(&mut preauth_s, &CryptoRequestPacket::placebo().to_pyrep())
        .await
        .unwrap();
    let cc = read_framed_pyrep(&mut preauth_s).await.unwrap().unwrap();
    assert_eq!(cc.as_str(), Some("OK CC"));
    assert!(
        !ctx.systems.is_system_loaded(jita_id),
        "HP-10: pre-auth Macho TCP connection must NOT spawn or subscribe to Jita"
    );

    // 2. Authenticated connection at character selection must NOT load Jita.
    let (mut charsel_s, client_id, user_id) =
        connect_and_authenticate(addr, "preauth_u1", "pw1").await;
    let _ = test_1237_ping_sync(&mut charsel_s, client_id, user_id, 1).await;
    assert!(
        !ctx.systems.is_system_loaded(jita_id),
        "HP-10: authenticated charsel connection must NOT spawn or subscribe to Jita"
    );

    // 3. Once Jita IS loaded and hibernated, new pre-auth & charsel connections keep
    //    receiver_count() == 0 and do not wake Jita from LOD-0 hibernation.
    let jita = ctx.systems.get_or_spawn(jita_id);
    let _ = jita.step_manual(1.0).await.unwrap();
    assert_eq!(jita.event_tx.receiver_count(), 0);
    assert!(
        jita.is_hibernated(),
        "empty Jita with zero subscribers must hibernate after step"
    );

    let mut preauth_s2 = TcpStream::connect(addr).await.unwrap();
    let _hello2 = read_framed_pyrep(&mut preauth_s2).await.unwrap().unwrap();
    write_framed_pyrep(&mut preauth_s2, &VersionExchange::default().to_pyrep())
        .await
        .unwrap();
    write_framed_pyrep(&mut preauth_s2, &CryptoRequestPacket::placebo().to_pyrep())
        .await
        .unwrap();
    let cc2 = read_framed_pyrep(&mut preauth_s2).await.unwrap().unwrap();
    assert_eq!(cc2.as_str(), Some("OK CC"));
    assert_eq!(
        jita.event_tx.receiver_count(),
        0,
        "HP-10: pre-auth socket must not increment Jita tick receiver_count"
    );
    assert!(
        jita.is_hibernated(),
        "HP-10: pre-auth socket must not wake hibernated Jita"
    );

    let (mut charsel_s2, client_id2, user_id2) =
        connect_and_authenticate(addr, "preauth_u2", "pw2").await;
    let _ = test_1237_ping_sync(&mut charsel_s2, client_id2, user_id2, 1).await;
    assert_eq!(
        jita.event_tx.receiver_count(),
        0,
        "HP-10: charsel connection must not increment Jita tick receiver_count"
    );
    assert!(
        jita.is_hibernated(),
        "HP-10: charsel connection must not wake hibernated Jita"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1237_selecting_docked_character_keeps_zero_subscribers_and_hibernated() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let jita_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let jita = ctx.systems.get_or_spawn(jita_id);
    let _ = jita.step_manual(1.0).await.unwrap();
    assert_eq!(jita.event_tx.receiver_count(), 0);
    assert!(jita.is_hibernated());

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) =
        connect_and_authenticate(addr, "demo", "demo-secret").await;

    let (sel_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    assert_eq!(sel_rsp.packet_type, MachoPacketType::CallRsp);
    assert_eq!(
        jita.event_tx.receiver_count(),
        0,
        "HP-10: selecting a docked character must keep Jita tick receiver_count == 0"
    );
    assert!(
        jita.is_hibernated(),
        "HP-10: selecting a docked character must keep Jita hibernated"
    );

    // Docked RPC and async character notification (`notify_rx`) must also keep receiver_count == 0.
    let (cash_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        2,
        "account",
        "GetCashBalance",
        vec![PyRep::Integer(0)],
    )
    .await;
    assert_eq!(cash_rsp.packet_type, MachoPacketType::CallRsp);
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnAccountChange",
        "charid",
        vec![
            PyRep::String("cash".into()),
            PyRep::Integer(char_id),
            PyRep::Real(1234.0),
        ],
    );
    test_1237_wait_for_notification(
        &mut s,
        "OnAccountChange",
        "HP-10: docked session must receive OnAccountChange notification without attaching ticks",
    )
    .await;
    assert_eq!(
        jita.event_tx.receiver_count(),
        0,
        "HP-10: docked RPCs and notify_rx events must not attach a tick receiver"
    );
    assert!(
        jita.is_hibernated(),
        "HP-10: docked session activity must not wake hibernated Jita"
    );

    // Keep 5 simultaneous docked sessions open in Jita and confirm receiver_count == 0 and LOD-0 hibernation.
    let mut extra_docked_sessions = Vec::new();
    for idx in 2_i64..=5_i64 {
        let user = format!("docked_u{idx}");
        let pass = format!("docked_pw{idx}");
        let cid = 90_000_100 + idx;
        let cname = format!("Docked Pilot {idx}");
        ctx.db.provision_gm_account(&user, &pass, cid, &cname);
        let (mut es, ecid, euid) = connect_and_authenticate(addr, &user, &pass).await;
        let (esel_rsp, _) = test_1386_call_rpc(
            &mut es,
            ecid,
            euid,
            1,
            "charUnboundMgr",
            "SelectCharacterID",
            vec![PyRep::Integer(cid)],
        )
        .await;
        assert_eq!(esel_rsp.packet_type, MachoPacketType::CallRsp);
        let (ecash_rsp, _) = test_1386_call_rpc(
            &mut es,
            ecid,
            euid,
            2,
            "account",
            "GetCashBalance",
            vec![PyRep::Integer(0)],
        )
        .await;
        assert_eq!(ecash_rsp.packet_type, MachoPacketType::CallRsp);
        let _ = test_1237_ping_sync(&mut es, ecid, euid, 3).await;
        extra_docked_sessions.push((es, ecid, euid));
    }
    let _ = jita.step_manual(1.0).await.unwrap();
    assert_eq!(
        jita.event_tx.receiver_count(),
        0,
        "HP-10: 5 concurrent docked sessions in Jita must leave receiver_count == 0"
    );
    assert!(
        jita.is_hibernated(),
        "HP-10: 5 concurrent docked sessions in Jita must keep LOD-0 hibernation engaged"
    );
    drop((s, extra_docked_sessions));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1237_undock_attaches_delivers_ticks_and_dock_immediately_detaches_for_hibernation() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let gm_char_id = 90_000_003_i64;
    let jita_id = 30_000_142_u32;
    ctx.db
        .provision_gm_account("admin", "admin-secret", gm_char_id, "ISD Polaris");

    let jita = ctx.systems.get_or_spawn(jita_id);
    let _ = jita.step_manual(1.0).await.unwrap();
    assert_eq!(jita.event_tx.receiver_count(), 0);
    assert!(jita.is_hibernated());

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) =
        connect_and_authenticate(addr, "admin", "admin-secret").await;

    let (sel_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(gm_char_id)],
    )
    .await;
    assert_eq!(sel_rsp.packet_type, MachoPacketType::CallRsp);
    assert_eq!(
        jita.event_tx.receiver_count(),
        0,
        "docked GM character must have zero tick receivers before undock"
    );

    // Undocking attaches the solar-system tick receiver immediately.
    let (undock_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        2,
        "stationSvc",
        "Undock",
        Vec::new(),
    )
    .await;
    assert_eq!(undock_rsp.packet_type, MachoPacketType::CallRsp);
    assert_eq!(
        jita.event_tx.receiver_count(),
        1,
        "HP-10: undocking must attach tick receiver (receiver_count == 1)"
    );
    assert!(!jita.is_hibernated());

    // Bind ballpark and verify live DoDestinyUpdate tick broadcast delivery on TCP wire.
    let (bind_rsp, bind_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(jita_id as i64), PyRep::None],
    )
    .await;
    assert_eq!(bind_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        bind_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "DoDestinyUpdate")),
        "MachoBindObject must deliver initial SetState DoDestinyUpdate"
    );
    assert_eq!(jita.event_tx.receiver_count(), 1);

    let active_ship_id = ctx.db.get_character(gm_char_id).unwrap().active_ship_id;
    jita.set_movement_mode(active_ship_id, eve_destiny::MovementMode::Stop)
        .await
        .unwrap();
    let _ = jita.step_manual(1.0).await.unwrap();
    test_1237_wait_for_notification(
        &mut s,
        "DoDestinyUpdate",
        "HP-10: attached in-space session must receive live DoDestinyUpdate tick broadcasts",
    )
    .await;

    // Advance past undock session-change timer and dock back at Jita 4-4.
    let (adv_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        5,
        "timeControl",
        "AdvanceClock",
        vec![PyRep::Integer(11)],
    )
    .await;
    assert_eq!(adv_rsp.packet_type, MachoPacketType::CallRsp);

    let (dock_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        6,
        "stationSvc",
        "Dock",
        vec![PyRep::Integer(60_003_760)],
    )
    .await;
    assert_eq!(dock_rsp.packet_type, MachoPacketType::CallRsp);
    assert_eq!(
        jita.event_tx.receiver_count(),
        0,
        "HP-10: docking must immediately drop tick receiver (receiver_count == 0)"
    );

    // Next empty step in Jita must transition cleanly into LOD-0 hibernation.
    let _ = jita.step_manual(1.0).await.unwrap();
    assert!(
        jita.is_hibernated(),
        "HP-10: Jita must hibernate after last in-space pilot docks"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1237_safe_logoff_to_charsel_from_space_immediately_drops_tick_receiver() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let gm_char_id = 90_000_003_i64;
    let jita_id = 30_000_142_u32;
    ctx.db
        .provision_gm_account("admin", "admin-secret", gm_char_id, "ISD Polaris");

    let jita = ctx.systems.get_or_spawn(jita_id);
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
    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(jita_id as i64), PyRep::None],
    )
    .await;
    assert_eq!(
        jita.event_tx.receiver_count(),
        1,
        "in-space session must hold 1 tick receiver before SafeLogoff"
    );

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
    let _ = jita.step_manual(1.0).await.unwrap();
    let _ = test_1237_ping_sync(&mut s, client_id, user_id, 5).await;

    // Complete SafeLogoff via AdvanceClock(30) and wait for CallRsp + OnSafeLogoffActivated.
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
            .expect("timed out waiting for SafeLogoff completion")
            .expect("read error")
            .expect("unexpected EOF");
        let pkt = MachoPacket::from_pyrep(&rep).expect("decode packet");
        match pkt.packet_type {
            MachoPacketType::CallRsp => got_call_rsp = true,
            MachoPacketType::Notification => {
                if let MachoAddress::Broadcast { broadcast_id, .. } = &pkt.destination {
                    if broadcast_id == "OnSafeLogoffActivated" {
                        got_safe_logoff = true;
                    }
                }
            }
            _ => {}
        }
    }

    // Synchronize with server loop (ensuring SafeLogoffCharsel teardown has completed).
    let _ = test_1237_ping_sync(&mut s, client_id, user_id, 7).await;
    assert_eq!(
        jita.event_tx.receiver_count(),
        0,
        "HP-10: SafeLogoff to charsel from space must immediately drop tick receiver (receiver_count == 0)"
    );

    let _ = jita.step_manual(1.0).await.unwrap();
    assert!(
        jita.is_hibernated(),
        "HP-10: Jita must hibernate once last in-space pilot completes SafeLogoff to charsel"
    );

    let (charsel_rsp, _) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        8,
        "charUnboundMgr",
        None,
        "GetCharacterSelectionData",
        Vec::new(),
    )
    .await;
    assert_eq!(charsel_rsp.packet_type, MachoPacketType::CallRsp);
    assert_eq!(jita.event_tx.receiver_count(), 0);
    assert!(jita.is_hibernated());
}

fn collect_set_states_from_pyrep_tree(
    rep: &PyRep,
    out: &mut Vec<(Option<i64>, Vec<eve_destiny::wire::DecodedBall>)>,
) {
    match rep {
        PyRep::Tuple(items) => {
            if items.len() == 2 && items[0].as_str() == Some("SetState") {
                if let PyRep::Tuple(args) = &items[1] {
                    if let Some(kv) = args.first() {
                        let ego = kv.dict_get("ego").and_then(|v| v.as_i64());
                        let balls = kv
                            .dict_get("state")
                            .and_then(|v| match v {
                                PyRep::Bytes(b) => eve_destiny::wire::decode_add_balls(b)
                                    .ok()
                                    .map(|(_, _, decoded)| decoded),
                                _ => None,
                            })
                            .unwrap_or_default();
                        out.push((ego, balls));
                        return;
                    }
                }
            }
            for item in items {
                collect_set_states_from_pyrep_tree(item, out);
            }
        }
        PyRep::List(items) => {
            for item in items {
                collect_set_states_from_pyrep_tree(item, out);
            }
        }
        PyRep::Dict(entries) => {
            for (k, v) in entries {
                collect_set_states_from_pyrep_tree(k, out);
                collect_set_states_from_pyrep_tree(v, out);
            }
        }
        PyRep::SubStream(inner) | PyRep::SubStruct(inner) => {
            collect_set_states_from_pyrep_tree(inner, out);
        }
        PyRep::Bytes(bytes) => {
            if let Ok(decoded) = eve_proto::marshal::decode(bytes) {
                collect_set_states_from_pyrep_tree(&decoded, out);
            }
        }
        PyRep::Object { args, .. } => collect_set_states_from_pyrep_tree(args, out),
        PyRep::ObjectEx { header, list, dict, .. } => {
            collect_set_states_from_pyrep_tree(header, out);
            for item in list {
                collect_set_states_from_pyrep_tree(item, out);
            }
            for (k, v) in dict {
                collect_set_states_from_pyrep_tree(k, out);
                collect_set_states_from_pyrep_tree(v, out);
            }
        }
        _ => {}
    }
}

fn setup_upwell_docked_pilot(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    struct_id: i64,
    nearby_ball_id: i64,
) -> (i64, crate::system_actor::SystemHandle) {
    let struct_pos = eve_destiny::DVec3::new(250_000.0, 0.0, 0.0);
    let mut ship_id = 0_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("demo") {
            acc.role |= eve_machonet::service_roles::ROLE_GMS;
        }
        let ch = snap.characters.get_mut(&char_id).expect("pilot char");
        let corp_id = ch.corporation_id;
        ship_id = ch.active_ship_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = Some(struct_id);
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.location_id = struct_id;
            ship_item.flag = eve_db::InventoryFlag::Hangar;
        }
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832, // Astrahus
                solar_system_id: sys_id,
                owner_corp_id: corp_id,
                name: "GH1448 Test Astrahus".into(),
                position: struct_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 2_500_000.0,
                armor_hp: 2_500_000.0,
                hull_hp: 2_500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // Keep nearby ball in space so SetState payloads include surrounding exterior objects.
    let _ = nearby_ball_id;
    (ship_id, sys_handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1448_upwell_view_outside_and_ship_undock_without_post_beyonce_wire() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_144_u32; // Perimeter
    let char_id = 90_000_001_i64;
    let struct_id = 1_040_001_448_100_i64;
    let nearby_ball_id = 900_000_020_999_i64;
    let (ship_id, sys_handle) =
        setup_upwell_docked_pilot(&ctx, char_id, sys_id, struct_id, nearby_ball_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: nearby_ball_id,
            type_id: 1230,
            position: eve_destiny::DVec3::new(253_500.0, 0.0, 0.0),
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

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) =
        connect_and_authenticate(addr, "demo", "demo-secret").await;

    // 1. Login while docked in Astrahus Hangar: stay Detached (0 subscribers, 0 DoDestinyUpdate, ship not spawned).
    let (sel_rsp, sel_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    assert_eq!(sel_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !sel_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "DoDestinyUpdate")),
        "structure-docked Hangar SelectCharacterID must NOT emit DoDestinyUpdate"
    );
    assert_eq!(sys_handle.event_tx.receiver_count(), 0);
    assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none());

    // 2. View Outside while STILL docked: beyonce.GetFormations -> beyonce.MachoBindObject.
    let (form_rsp, form_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        2,
        "beyonce",
        "GetFormations",
        Vec::new(),
    )
    .await;
    assert_eq!(form_rsp.packet_type, MachoPacketType::CallRsp);
    let mut view_set_states = Vec::new();
    for pkt in &form_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut view_set_states);
    }
    assert_eq!(
        view_set_states.len(),
        1,
        "View Outside GetFormations must emit exactly 1 SetState observing structure {struct_id}"
    );
    assert_eq!(
        view_set_states[0].0,
        Some(struct_id),
        "View Outside SetState ego must be structure_id={struct_id}"
    );
    assert!(
        view_set_states[0].1.iter().any(|b| b.id == struct_id),
        "View Outside SetState balls must contain structure_id={struct_id}"
    );
    // Note: `Ballpark::visible_balls_for_observer(struct_id)` filters static `Rigid` observers to
    // static/`Rigid`/`Field` balls until static-observer dynamic-grid support is added in `eve-destiny`.
    // Dynamic `nearby_ball_id` visibility is asserted immediately below upon undocking (`ego == ship_id`).
    assert!(
        !view_set_states[0].1.iter().any(|b| b.id == ship_id),
        "View Outside SetState must NEVER spawn docked ship_id={ship_id}"
    );
    assert!(
        sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none(),
        "docked ship ball must stay absent in space during View Outside"
    );
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);

    let (bind_rsp, bind_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(bind_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !bind_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "DoDestinyUpdate")),
        "View Outside MachoBindObject after GetFormations must NOT replay duplicate SetState"
    );

    // Stepping simulation after an event on the observed structure while docked in View Outside
    // delivers live tick updates.
    let dmg_res = sys_handle
        .apply_damage(
            struct_id,
            eve_destiny::DamageVector {
                em: 100.0,
                thermal: 0.0,
                kinetic: 0.0,
                explosive: 0.0,
            },
        )
        .await
        .unwrap();
    assert!(
        dmg_res.is_some(),
        "View Outside Astrahus ball must have active combat state"
    );
    let _ = sys_handle.step_manual(1.0).await.unwrap();
    test_1237_wait_for_notification(
        &mut s,
        "DoDestinyUpdate",
        "View Outside attached session must receive live 1Hz DoDestinyUpdate ticks",
    )
    .await;

    // 3. Undock from View Outside via ship.Undock with ZERO post-undock beyonce calls!
    let view_session_id = ctx
        .db
        .get_online_session(user_id)
        .expect("online session during View Outside")
        .session_id;
    assert!(
        ctx.is_space_ballpark_bootstrapped(view_session_id, char_id, sys_id),
        "View Outside must leave space_ballpark_bootstrapped marked before Undock"
    );
    let (adv_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        4,
        "timeControl",
        "AdvanceClock",
        vec![PyRep::Integer(12)],
    )
    .await;
    assert_eq!(adv_rsp.packet_type, MachoPacketType::CallRsp);

    let (undock_rsp, undock_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        5,
        "ship",
        "Undock",
        Vec::new(),
    )
    .await;
    assert_eq!(undock_rsp.packet_type, MachoPacketType::CallRsp);
    let mut undock_set_states = Vec::new();
    for pkt in &undock_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut undock_set_states);
    }
    assert_eq!(
        undock_set_states.len(),
        1,
        "View Outside -> ship.Undock must deliver SetState(ego=ship_id) directly on Undock without post-undock beyonce RPCs"
    );
    assert_eq!(undock_set_states[0].0, Some(ship_id));
    assert!(undock_set_states[0].1.iter().any(|b| b.id == ship_id));
    assert!(undock_set_states[0].1.iter().any(|b| b.id == struct_id));
    assert!(undock_set_states[0].1.iter().any(|b| b.id == nearby_ball_id));
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);
    assert!(
        ctx.is_space_ballpark_bootstrapped(view_session_id, char_id, sys_id),
        "View Outside -> ship.Undock must keep space_ballpark_bootstrapped true (F-1461-01)"
    );

    // Follow-up beyonce.GetFormations after View Outside -> Undock must NOT replay SetState.
    let (post_undock_form_rsp, post_undock_form_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        6,
        "beyonce",
        "GetFormations",
        Vec::new(),
    )
    .await;
    assert_eq!(post_undock_form_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !post_undock_form_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "SetState")
                || test_1386_packet_contains_ascii(&p.to_pyrep(), "DoDestinyUpdate")),
        "follow-up beyonce.GetFormations after View Outside -> Undock must NOT replay SetState"
    );

    // Subsequent 1Hz ticks deliver live updates observing ship_id.
    sys_handle
        .set_movement_mode(ship_id, eve_destiny::MovementMode::Stop)
        .await
        .unwrap();
    let _ = sys_handle.step_manual(1.0).await.unwrap();
    test_1237_wait_for_notification(
        &mut s,
        "DoDestinyUpdate",
        "post-undock space session must receive live ticks observing ship_id",
    )
    .await;

    // 4. Dock back into Astrahus cleanly detaches listener stream.
    let (adv2_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        7,
        "timeControl",
        "AdvanceClock",
        vec![PyRep::Integer(12)],
    )
    .await;
    assert_eq!(adv2_rsp.packet_type, MachoPacketType::CallRsp);
    let (dock_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        8,
        "structureDocking",
        "Dock",
        vec![PyRep::Integer(struct_id)],
    )
    .await;
    assert_eq!(dock_rsp.packet_type, MachoPacketType::CallRsp);
    assert_eq!(
        sys_handle.event_tx.receiver_count(),
        0,
        "Dock back into Upwell structure must immediately detach tick receiver"
    );
    assert!(sys_handle.get_ball_snapshot(ship_id).await.unwrap().is_none());
    // Clear the synthetic dynamic exterior ball (`nearby_ball_id`) so zero dynamic balls remain;
    // the static Astrahus ball alone must still allow the system to hibernate once detached.
    let _ = sys_handle.remove_ball(nearby_ball_id).await;
    let _ = sys_handle.step_manual(1.0).await.unwrap();
    assert!(sys_handle.is_hibernated());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1448_upwell_hangar_undock_delivers_set_state_once_without_replay_on_get_formations() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_144_u32;
    let char_id = 90_000_001_i64;
    let struct_id = 1_040_001_448_101_i64;
    let nearby_ball_id = 900_000_020_998_i64;
    let (ship_id, sys_handle) =
        setup_upwell_docked_pilot(&ctx, char_id, sys_id, struct_id, nearby_ball_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: nearby_ball_id,
            type_id: 1230,
            position: eve_destiny::DVec3::new(254_000.0, 0.0, 0.0),
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

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) =
        connect_and_authenticate(addr, "demo", "demo-secret").await;

    let (sel_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    assert_eq!(sel_rsp.packet_type, MachoPacketType::CallRsp);
    assert_eq!(sys_handle.event_tx.receiver_count(), 0);

    // Hangar -> structureDocking.Undock (no prior View Outside): must NOT emit SetState on Undock
    // (#1448 FIX_FAILED — V24 tears down Hangar and calls OnAddBallpark -> GetFormations ~635ms later).
    let (undock_rsp, undock_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        2,
        "structureDocking",
        "Undock",
        Vec::new(),
    )
    .await;
    assert_eq!(undock_rsp.packet_type, MachoPacketType::CallRsp);
    let mut undock_set_states = Vec::new();
    for pkt in &undock_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut undock_set_states);
    }
    assert!(
        undock_set_states.is_empty(),
        "Hangar -> structureDocking.Undock must NOT emit SetState on Undock, got {undock_set_states:?}"
    );
    assert!(
        !undock_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "DoDestinyUpdate")),
        "Hangar -> Undock must NOT emit DoDestinyUpdate before client OnAddBallpark"
    );
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);

    // Even if a 1Hz tick or unrelated RPC arrives during the Hangar teardown window before
    // OnAddBallpark, the listener stays Detached and does not push premature destiny frames.
    let _ = sys_handle.step_manual(1.0).await.unwrap();
    let (cash_rsp, cash_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "account",
        "GetCashBalance",
        vec![PyRep::Integer(0)],
    )
    .await;
    assert_eq!(cash_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !cash_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "DoDestinyUpdate")),
        "unrelated RPC before post-Undock GetFormations must NOT leak DoDestinyUpdate"
    );

    // Post-OnAddBallpark GetFormations must deliver SetState(ego=ship_id) once; MachoBindObject
    // must NOT replay duplicate SetState.
    let (form_rsp, form_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        4,
        "beyonce",
        "GetFormations",
        Vec::new(),
    )
    .await;
    assert_eq!(form_rsp.packet_type, MachoPacketType::CallRsp);
    let mut form_set_states = Vec::new();
    for pkt in &form_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut form_set_states);
    }
    assert_eq!(
        form_set_states.len(),
        1,
        "Hangar -> Undock -> GetFormations must emit SetState once"
    );
    assert_eq!(form_set_states[0].0, Some(ship_id));
    assert!(form_set_states[0].1.iter().any(|b| b.id == ship_id));
    assert!(form_set_states[0].1.iter().any(|b| b.id == struct_id));
    assert!(form_set_states[0].1.iter().any(|b| b.id == nearby_ball_id));

    let (bind_rsp, bind_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        5,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(bind_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !bind_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "SetState")),
        "Hangar -> Undock -> GetFormations -> MachoBindObject must NOT replay SetState"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1448_hangar_ship_undock_defers_set_state_until_macho_bind_and_release_enter_hangar() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_144_u32;
    let char_id = 90_000_001_i64;
    let struct_id = 1_040_001_448_102_i64;
    let nearby_ball_id = 900_000_020_997_i64;
    let (ship_id, sys_handle) =
        setup_upwell_docked_pilot(&ctx, char_id, sys_id, struct_id, nearby_ball_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: nearby_ball_id,
            type_id: 1230,
            position: eve_destiny::DVec3::new(254_000.0, 0.0, 0.0),
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

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) =
        connect_and_authenticate(addr, "demo", "demo-secret").await;
    let (sel_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    assert_eq!(sel_rsp.packet_type, MachoPacketType::CallRsp);

    // 1. Live client V24 path: Hangar -> ship.Undock -> step -> beyonce.MachoBindObject -> GetFormations.
    let (undock_rsp, undock_notifs) =
        test_1386_call_rpc(&mut s, client_id, user_id, 2, "ship", "Undock", Vec::new()).await;
    assert_eq!(undock_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !undock_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "DoDestinyUpdate")
                || test_1386_packet_contains_ascii(&p.to_pyrep(), "SetState")),
        "Hangar -> ship.Undock must NOT emit DoDestinyUpdate(SetState) before OnAddBallpark"
    );
    let _ = sys_handle.step_manual(1.0).await.unwrap();

    let (bind_rsp, bind_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(bind_rsp.packet_type, MachoPacketType::CallRsp);
    let mut bind_states = Vec::new();
    for pkt in &bind_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut bind_states);
    }
    assert_eq!(
        bind_states.len(),
        1,
        "post-ship.Undock MachoBindObject must emit SetState(ego=ship_id) once"
    );
    assert_eq!(bind_states[0].0, Some(ship_id));
    assert!(bind_states[0].1.iter().any(|b| b.id == ship_id));
    assert!(bind_states[0].1.iter().any(|b| b.id == struct_id));
    assert!(bind_states[0].1.iter().any(|b| b.id == nearby_ball_id));

    let (form_rsp, form_notifs) =
        test_1386_call_rpc(&mut s, client_id, user_id, 4, "beyonce", "GetFormations", Vec::new())
            .await;
    assert_eq!(form_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !form_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "SetState")),
        "follow-up GetFormations after MachoBindObject must NOT replay SetState"
    );

    // 2. Dock back into Astrahus, enter View Outside, then Enter Hangar (client releases beyonce via OID-).
    // Next Hangar ship.Undock must again defer SetState until post-OnAddBallpark GetFormations.
    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        5,
        "timeControl",
        "AdvanceClock",
        vec![PyRep::Integer(12)],
    )
    .await;
    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        6,
        "structureDocking",
        "Dock",
        vec![PyRep::Integer(struct_id)],
    )
    .await;
    assert_eq!(sys_handle.event_tx.receiver_count(), 0);

    let (view_bind_rsp, view_bind_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        7,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(view_bind_rsp.packet_type, MachoPacketType::CallRsp);
    let mut view_states = Vec::new();
    for pkt in &view_bind_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut view_states);
    }
    assert_eq!(view_states.len(), 1);
    assert_eq!(view_states[0].0, Some(struct_id));
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);

    let active_sid = ctx
        .db
        .get_online_session(user_id)
        .expect("online session")
        .session_id;
    let beyonce_bid = ctx
        .bound_objects
        .read()
        .unwrap()
        .values()
        .find(|e| e.session_id == active_sid && e.service == "beyonce")
        .map(|e| e.bound_id)
        .expect("beyonce bound object exists in View Outside");

    // Client tears down remoteBallpark ("Enter Hangar") by releasing beyonce via OID-.
    let mut rel_req = make_call_req(
        client_id,
        user_id,
        8,
        "account",
        None,
        "GetCashBalance",
        vec![PyRep::Integer(0)],
    );
    rel_req
        .named_payload
        .insert("OID-".into(), ctx.bound_object_wire_rep(beyonce_bid));
    write_framed_pyrep(&mut s, &rel_req.to_pyrep()).await.unwrap();
    loop {
        let rep = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read_framed_pyrep(&mut s),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        let pkt = MachoPacket::from_pyrep(&rep).unwrap();
        if pkt.packet_type == MachoPacketType::CallRsp {
            break;
        }
    }
    assert_eq!(
        sys_handle.event_tx.receiver_count(),
        0,
        "releasing beyonce on Enter Hangar while structure-docked must detach tick receiver"
    );

    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        9,
        "timeControl",
        "AdvanceClock",
        vec![PyRep::Integer(12)],
    )
    .await;
    let (undock2_rsp, undock2_notifs) =
        test_1386_call_rpc(&mut s, client_id, user_id, 10, "ship", "Undock", Vec::new()).await;
    assert_eq!(undock2_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !undock2_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "SetState")),
        "ship.Undock after View Outside -> Enter Hangar (OID- release) must defer SetState until GetFormations"
    );

    let (form2_rsp, form2_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        11,
        "beyonce",
        "GetFormations",
        Vec::new(),
    )
    .await;
    assert_eq!(form2_rsp.packet_type, MachoPacketType::CallRsp);
    let mut form2_states = Vec::new();
    for pkt in &form2_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut form2_states);
    }
    assert_eq!(form2_states.len(), 1);
    assert_eq!(form2_states[0].0, Some(ship_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1461_multi_pilot_view_outside_shares_structure_ticks_without_contention() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_144_u32;
    let struct_id = 1_040_001_461_201_i64;
    let char_a = 90_000_001_i64;
    let char_b = 90_146_102_i64;
    let nearby_ball_id = 900_000_021_201_i64;
    let (ship_a, sys_handle) =
        setup_upwell_docked_pilot(&ctx, char_a, sys_id, struct_id, nearby_ball_id);
    ctx.db
        .provision_gm_account("view_b", "pw_b", char_b, "View Pilot B");
    ctx.db.with_mut_snapshot(|snap| {
        let corp_a = snap.characters[&char_a].corporation_id;
        let ch_b = snap.characters.get_mut(&char_b).expect("char_b");
        let ship_b = ch_b.active_ship_id;
        ch_b.corporation_id = corp_a;
        ch_b.solar_system_id = sys_id;
        ch_b.station_id = None;
        ch_b.structure_id = Some(struct_id);
        if let Some(ship_item) = snap.items.get_mut(&ship_b) {
            ship_item.location_id = struct_id;
            ship_item.flag = eve_db::InventoryFlag::Hangar;
        }
    });
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: nearby_ball_id,
            type_id: 1230,
            position: eve_destiny::DVec3::new(254_000.0, 0.0, 0.0),
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

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s_a, cid_a, uid_a) = connect_and_authenticate(addr, "demo", "demo-secret").await;
    let (mut s_b, cid_b, uid_b) = connect_and_authenticate(addr, "view_b", "pw_b").await;

    let _ = test_1386_call_rpc(
        &mut s_a,
        cid_a,
        uid_a,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_a)],
    )
    .await;
    let _ = test_1386_call_rpc(
        &mut s_b,
        cid_b,
        uid_b,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_b)],
    )
    .await;

    // Pilot A enters View Outside: receives SetState(ego=struct_id) without polluting shared queue.
    let (form_a_rsp, form_a_notifs) =
        test_1386_call_rpc(&mut s_a, cid_a, uid_a, 2, "beyonce", "GetFormations", Vec::new()).await;
    assert_eq!(form_a_rsp.packet_type, MachoPacketType::CallRsp);
    let mut a_states = Vec::new();
    for pkt in &form_a_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut a_states);
    }
    assert_eq!(a_states.len(), 1);
    assert_eq!(a_states[0].0, Some(struct_id));
    assert!(
        sys_handle
            .drain_observer_updates(struct_id)
            .await
            .unwrap()
            .1
            .is_empty(),
        "shared observer_events[{struct_id}] must remain empty after Pilot A View Outside"
    );

    // Pilot B concurrently enters View Outside on the exact same structure without contending.
    let (bind_b_rsp, bind_b_notifs) = test_1386_call_rpc(
        &mut s_b,
        cid_b,
        uid_b,
        2,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(bind_b_rsp.packet_type, MachoPacketType::CallRsp);
    let mut b_states = Vec::new();
    for pkt in &bind_b_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut b_states);
    }
    assert_eq!(b_states.len(), 1);
    assert_eq!(b_states[0].0, Some(struct_id));
    assert_eq!(sys_handle.event_tx.receiver_count(), 2);

    // Apply damage and step 1Hz simulation: BOTH pilots must receive DoDestinyUpdate.
    assert!(
        sys_handle
            .apply_damage(
                struct_id,
                eve_destiny::DamageVector {
                    em: 100.0,
                    thermal: 0.0,
                    kinetic: 0.0,
                    explosive: 0.0,
                },
            )
            .await
            .unwrap()
            .is_some()
    );
    let _ = sys_handle.step_manual(1.0).await.unwrap();
    test_1237_wait_for_notification(
        &mut s_a,
        "DoDestinyUpdate",
        "Pilot A in shared View Outside must receive 1Hz DoDestinyUpdate",
    )
    .await;
    test_1237_wait_for_notification(
        &mut s_b,
        "DoDestinyUpdate",
        "Pilot B in shared View Outside must concurrently receive 1Hz DoDestinyUpdate",
    )
    .await;

    // Pilot A docks out/leaves space: Pilot B's stream stays intact and keeps receiving ticks.
    let _ = test_1386_call_rpc(
        &mut s_a,
        cid_a,
        uid_a,
        3,
        "timeControl",
        "AdvanceClock",
        vec![PyRep::Integer(12)],
    )
    .await;
    let _ = test_1386_call_rpc(&mut s_a, cid_a, uid_a, 4, "ship", "Undock", Vec::new()).await;
    let _ = sys_handle
        .set_movement_mode(ship_a, eve_destiny::MovementMode::Stop)
        .await;
    let _ = test_1386_call_rpc(
        &mut s_a,
        cid_a,
        uid_a,
        5,
        "timeControl",
        "AdvanceClock",
        vec![PyRep::Integer(12)],
    )
    .await;
    let _ = test_1386_call_rpc(
        &mut s_a,
        cid_a,
        uid_a,
        6,
        "structureDocking",
        "Dock",
        vec![PyRep::Integer(struct_id)],
    )
    .await;
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);

    assert!(
        sys_handle
            .apply_damage(
                struct_id,
                eve_destiny::DamageVector {
                    em: 50.0,
                    thermal: 0.0,
                    kinetic: 0.0,
                    explosive: 0.0,
                },
            )
            .await
            .unwrap()
            .is_some()
    );
    let _ = sys_handle.step_manual(1.0).await.unwrap();
    test_1237_wait_for_notification(
        &mut s_b,
        "DoDestinyUpdate",
        "Pilot B must keep receiving DoDestinyUpdate after Pilot A undocks/docks",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1461_notify_rx_same_system_structure_evacuation_resubscribes_sys_rx() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_144_u32;
    let char_id = 90_000_001_i64;
    let struct_id = 1_040_001_461_301_i64;
    let nearby_ball_id = 900_000_021_301_i64;
    let (ship_id, sys_handle) =
        setup_upwell_docked_pilot(&ctx, char_id, sys_id, struct_id, nearby_ball_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: nearby_ball_id,
            type_id: 1230,
            position: eve_destiny::DVec3::new(254_000.0, 0.0, 0.0),
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

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) =
        connect_and_authenticate(addr, "demo", "demo-secret").await;
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

    let (form_rsp, form_notifs) =
        test_1386_call_rpc(&mut s, client_id, user_id, 2, "beyonce", "GetFormations", Vec::new())
            .await;
    assert_eq!(form_rsp.packet_type, MachoPacketType::CallRsp);
    let mut view_states = Vec::new();
    for pkt in &form_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut view_states);
    }
    assert_eq!(view_states.len(), 1);
    assert_eq!(view_states[0].0, Some(struct_id));
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);

    // Async same-system evacuation (structure_id: Some(S) -> None, solar_system_id unchanged).
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: eve_destiny::DVec3::new(252_000.0, 0.0, 0.0),
            velocity: eve_destiny::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1.0e6,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).expect("char");
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.location_id = i64::from(sys_id);
            ship_item.flag = eve_db::InventoryFlag::None;
        }
    });
    ctx.clear_space_ballpark_bootstrapped(char_id);
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnSessionChanged",
        "charid",
        vec![PyRep::Dict(vec![(
            PyRep::String("structureid".into()),
            PyRep::Tuple(vec![PyRep::Integer(struct_id), PyRep::None]),
        )])],
    );

    let sc_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let rem = sc_deadline.saturating_duration_since(tokio::time::Instant::now());
        let rep = tokio::time::timeout(rem, read_framed_pyrep(&mut s))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let pkt = MachoPacket::from_pyrep(&rep).unwrap();
        if pkt.packet_type == MachoPacketType::SessionChangeNotification {
            break;
        }
    }
    assert_eq!(
        sys_handle.event_tx.receiver_count(),
        1,
        "notify_rx same-system structure evacuation must re-subscribe sys_rx (F-3)"
    );

    let (bind_rsp, bind_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(bind_rsp.packet_type, MachoPacketType::CallRsp);
    let mut ship_states = Vec::new();
    for pkt in &bind_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut ship_states);
    }
    assert_eq!(ship_states.len(), 1);
    assert_eq!(ship_states[0].0, Some(ship_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1461_bound_machonet_beyonce_calls_init_and_cooldown_resync() {
    use crate::outbound_sequencer::{DestinyStream, OutFrame, SessionWireState};

    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_142_u32; // Jita 4-4
    let char_id = 90_000_001_i64;
    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) =
        connect_and_authenticate(addr, "demo", "demo-secret").await;
    let jita = ctx.systems.get_or_spawn(sys_id);

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
    let active_sid = ctx
        .db
        .get_online_session(user_id)
        .expect("online lock")
        .session_id;
    let ship_id = ctx.db.get_character(char_id).unwrap().active_ship_id;

    // Bind beyonce while still docked in Jita 4-4 (`station_id.is_some()` keeps wire_state Detached).
    let (prebind_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        2,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(prebind_rsp.packet_type, MachoPacketType::CallRsp);
    let beyonce_bid = ctx
        .bound_objects
        .read()
        .unwrap()
        .values()
        .find(|e| e.session_id == active_sid && e.service == "beyonce")
        .map(|e| e.bound_id)
        .expect("beyonce bound id");

    let (undock_rsp, undock_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "stationSvc",
        "Undock",
        Vec::new(),
    )
    .await;
    assert_eq!(undock_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !undock_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "SetState")),
        "NPC station Undock must defer SetState until beyonce RPC"
    );

    // Invoke GetFormations via TCP with service = "machoNet" + bound_id = Some(beyonce_bid) (F-4).
    let (form_rsp, form_notifs) = test_1386_call_rpc_raw(
        &mut s,
        client_id,
        user_id,
        4,
        "machoNet",
        Some(beyonce_bid as i64),
        "GetFormations",
        Vec::new(),
    )
    .await;
    assert_eq!(form_rsp.packet_type, MachoPacketType::CallRsp);
    let mut bound_states = Vec::new();
    for pkt in &form_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut bound_states);
    }
    assert_eq!(
        bound_states.len(),
        1,
        "bound machoNet + beyonce_bid GetFormations must deliver SetState(ego=ship_id) (F-4)"
    );
    assert_eq!(bound_states[0].0, Some(ship_id));
    assert_eq!(jita.event_tx.receiver_count(), 1);
    drop(s);

    // Direct wire check for bound ("machoNet", Some(bid), "UpdateStateRequest") after broadcast lag
    // within cooldown (< 10s): must reset UpdateStateRequest rate limit and restore Live stream.
    let wire_ctx = Arc::new(ServerContext::new());
    wire_ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let wire_sys = wire_ctx.systems.get_or_spawn(sys_id);
    let wire_ship_id = wire_ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).expect("char");
        let ship_id = ch.active_ship_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.location_id = i64::from(sys_id);
            ship_item.flag = eve_db::InventoryFlag::None;
        }
        ship_id
    });
    let mut session = SessionState::new(777);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.live_notify_listener = true;
    wire_ctx
        .db
        .acquire_online_lock(1, session.session_id, Some(char_id), true, wire_ctx.now_unix_secs())
        .unwrap();
    wire_ctx.sync_session(&mut session);
    assert_eq!(session.ship_id, Some(wire_ship_id));
    wire_sys
        .spawn_ball(eve_destiny::Ball {
            id: wire_ship_id,
            type_id: 587,
            position: eve_destiny::DVec3::new(1000.0, 0.0, 0.0),
            velocity: eve_destiny::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1.0e6,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .unwrap();

    let mut sys_rx = wire_sys.subscribe();
    let mut subscribed_sys_id = Some(sys_id);
    let mut wire_state = SessionWireState::new(session.session_id, 9_900_001);
    let mut deferred_ship_id = None;
    let req_source = MachoAddress::Client {
        client_id: 9_900_001,
        call_id: 1,
        service: None,
    };
    let req_dest = MachoAddress::Node {
        node_id: MACHO_PROXY_NODE_ID,
        service: Some("beyonce".into()),
        call_id: 0,
    };
    let kw = HashMap::new();
    let bind_out = {
        let mut refs = CallReqWireRefs {
            client_id: 9_900_001,
            sys_rx: &mut sys_rx,
            subscribed_sys_id: &mut subscribed_sys_id,
            wire_state: &mut wire_state,
            deferred_ship_id: &mut deferred_ship_id,
        };
        execute_call_req_wire(
            &wire_ctx,
            &mut session,
            &mut refs,
            &req_source,
            &req_dest,
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(i64::from(sys_id)), PyRep::None],
            &kw,
        )
        .await
    };
    assert!(bind_out.rpc_result.is_ok());
    assert!(matches!(wire_state.destiny, DestinyStream::Live { .. }));
    let wire_bid = wire_ctx
        .bound_objects
        .read()
        .unwrap()
        .values()
        .find(|e| e.session_id == session.session_id && e.service == "beyonce")
        .map(|e| e.bound_id)
        .expect("wire beyonce bound id");

    // Simulate lag (< 1s later, within UpdateStateRequest cooldown) then invoke bound UpdateStateRequest.
    wire_state.on_broadcast_lagged();
    assert!(wire_state.needs_initial_set_state());
    let machonet_dest = MachoAddress::Node {
        node_id: MACHO_PROXY_NODE_ID,
        service: Some("machoNet".into()),
        call_id: 0,
    };
    let resync_out = {
        let mut refs = CallReqWireRefs {
            client_id: 9_900_001,
            sys_rx: &mut sys_rx,
            subscribed_sys_id: &mut subscribed_sys_id,
            wire_state: &mut wire_state,
            deferred_ship_id: &mut deferred_ship_id,
        };
        execute_call_req_wire(
            &wire_ctx,
            &mut session,
            &mut refs,
            &req_source,
            &machonet_dest,
            "machoNet",
            Some(wire_bid),
            "UpdateStateRequest",
            &[],
            &kw,
        )
        .await
    };
    assert!(resync_out.rpc_result.is_ok());
    assert!(matches!(wire_state.destiny, DestinyStream::Live { .. }));
    assert!(
        resync_out
            .out_frames
            .iter()
            .any(|f| matches!(f, OutFrame::DoDestinyUpdate { has_set_state: true, .. })),
        "bound machoNet UpdateStateRequest after lag within cooldown must reset rate limit and emit SetState"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1461_enter_hangar_callreq_client_has_released_these_objects_detaches_view_outside() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_144_u32;
    let char_id = 90_000_001_i64;
    let struct_id = 1_040_001_461_301_i64;
    let nearby_ball_id = 900_000_020_994_i64;
    let (ship_id, sys_handle) =
        setup_upwell_docked_pilot(&ctx, char_id, sys_id, struct_id, nearby_ball_id);

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) =
        connect_and_authenticate(addr, "demo", "demo-secret").await;
    let (sel_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    assert_eq!(sel_rsp.packet_type, MachoPacketType::CallRsp);

    // 1. Enter View Outside via beyonce.MachoBindObject.
    let (view_rsp, view_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        2,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(view_rsp.packet_type, MachoPacketType::CallRsp);
    let mut view_states = Vec::new();
    for pkt in &view_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut view_states);
    }
    assert_eq!(view_states.len(), 1);
    assert_eq!(view_states[0].0, Some(struct_id));
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);

    let active_sid = ctx
        .db
        .get_online_session(user_id)
        .expect("online session")
        .session_id;
    let beyonce_bid = ctx
        .bound_objects
        .read()
        .unwrap()
        .values()
        .find(|e| e.session_id == active_sid && e.service == "beyonce")
        .map(|e| e.bound_id)
        .expect("beyonce bound object exists");
    assert!(ctx.is_space_ballpark_bootstrapped(active_sid, char_id, sys_id));

    // 2. Client releases beyonce via CallReq("ClientHasReleasedTheseObjects") (NO OID- header!).
    let (rel_rsp, rel_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "machoNet",
        "ClientHasReleasedTheseObjects",
        vec![ctx.bound_object_wire_rep(beyonce_bid)],
    )
    .await;
    assert_eq!(rel_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !rel_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "DoDestinyUpdate")),
        "CallReq ClientHasReleasedTheseObjects must cleanly detach without flushing ticks"
    );
    assert_eq!(
        sys_handle.event_tx.receiver_count(),
        0,
        "CallReq ClientHasReleasedTheseObjects on Enter Hangar must detach tick receiver"
    );
    assert!(!ctx.has_bound_service(active_sid, "beyonce"));
    assert!(
        !ctx.is_space_ballpark_bootstrapped(active_sid, char_id, sys_id),
        "CallReq ClientHasReleasedTheseObjects on Enter Hangar must clear ballpark bootstrap"
    );

    // 3. Re-entering View Outside immediately within 5s cooldown must succeed cleanly,
    //    and releasing again via CallReq followed by Hangar ship.Undock must defer SetState.
    let (reenter_rsp, reenter_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        4,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(reenter_rsp.packet_type, MachoPacketType::CallRsp);
    let mut reenter_states = Vec::new();
    for pkt in &reenter_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut reenter_states);
    }
    assert_eq!(reenter_states.len(), 1);
    assert_eq!(reenter_states[0].0, Some(struct_id));

    let beyonce_bid_2 = ctx
        .bound_objects
        .read()
        .unwrap()
        .values()
        .find(|e| e.session_id == active_sid && e.service == "beyonce")
        .map(|e| e.bound_id)
        .expect("second beyonce bound object exists");
    let (rel2_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        5,
        "machoNet",
        "ClientHasReleasedTheseObjects",
        vec![PyRep::List(vec![ctx.bound_object_wire_rep(beyonce_bid_2)])],
    )
    .await;
    assert_eq!(rel2_rsp.packet_type, MachoPacketType::CallRsp);
    assert_eq!(sys_handle.event_tx.receiver_count(), 0);

    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        6,
        "timeControl",
        "AdvanceClock",
        vec![PyRep::Integer(12)],
    )
    .await;
    let (undock_rsp, undock_notifs) =
        test_1386_call_rpc(&mut s, client_id, user_id, 7, "ship", "Undock", Vec::new()).await;
    assert_eq!(undock_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !undock_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "SetState")),
        "ship.Undock after CallReq Enter Hangar must defer SetState until post-OnAddBallpark MachoBindObject"
    );

    let (bind_rsp, bind_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        8,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(bind_rsp.packet_type, MachoPacketType::CallRsp);
    let mut bind_states = Vec::new();
    for pkt in &bind_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut bind_states);
    }
    assert_eq!(bind_states.len(), 1);
    assert_eq!(bind_states[0].0, Some(ship_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1461_resync_structure_set_state_same_tick_watermark_seq_prevents_replay_and_loss() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_144_u32;
    let struct_id = 1_040_001_461_404_i64;
    let char_id = 90_000_001_i64;
    let nearby_ball_id = 900_000_021_404_i64;
    let (_ship_id, sys_handle) =
        setup_upwell_docked_pilot(&ctx, char_id, sys_id, struct_id, nearby_ball_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: nearby_ball_id,
            type_id: 1230,
            position: eve_destiny::DVec3::new(254_000.0, 0.0, 0.0),
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

    let mut sys_rx = sys_handle.subscribe();
    let mut wire_state = crate::outbound_sequencer::SessionWireState::new(1, 1);

    // Advance one full tick (`tick = 1`) and emit a second same-tick broadcast via
    // `flush_peer_observer_updates` so `broadcast_seq` advances while `tick` stays unchanged.
    let step1 = sys_handle.step_manual(1.0).await.unwrap();
    assert!(
        sys_handle
            .emit_observer_destiny_event(
                struct_id,
                eve_destiny::DestinyEvent::OnSlimItemChange {
                    ball_id: nearby_ball_id,
                },
            )
            .await
    );
    let flushed_pre = sys_handle.flush_peer_observer_updates(None).await.unwrap();
    assert!(flushed_pre >= 1);

    // 1. Pre-snapshot same-tick broadcast buffered in `sys_rx` (`<= watermark_seq`) must be
    //    discarded by `resync_structure_set_state_with_sys_rx` (zero duplicate replay).
    let resynced = resync_structure_set_state_with_sys_rx(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        sys_id,
        struct_id,
        &[struct_id],
        &mut wire_state,
    )
    .await
    .expect("structure SetState resync must succeed");
    assert_eq!(resynced.tick, step1.tick);
    assert!(
        resynced.post_batches.is_empty(),
        "same-tick pre-snapshot broadcast must not replay in post_batches"
    );

    // 2. When `broadcast_seq` advances on the SAME tick *after* `get_set_state_with_seq`,
    //    `WatermarkSeq` retains the post-snapshot same-tick broadcast while still discarding any
    //    `<= watermark_seq` broadcast at the same tick.
    let (snap_tick, _kv, _updates, watermark_seq) =
        sys_handle.get_set_state_with_seq(struct_id).await.unwrap();
    assert_eq!(snap_tick, step1.tick);
    assert!(
        sys_handle
            .emit_observer_destiny_event(
                struct_id,
                eve_destiny::DestinyEvent::OnSlimItemChange {
                    ball_id: nearby_ball_id,
                },
            )
            .await
    );
    let flushed_post = sys_handle.flush_peer_observer_updates(None).await.unwrap();
    assert!(flushed_post >= 1);

    let mut post_batches = Vec::new();
    let post_loss = drain_sys_rx_discarding_up_to_seq(
        &ctx,
        &mut sys_rx,
        sys_id,
        &[struct_id],
        wire_state.current_epoch(),
        watermark_seq,
        &mut post_batches,
    );
    assert!(!post_loss);
    assert_eq!(
        post_batches.len(),
        1,
        "post-snapshot same-tick broadcast (> watermark_seq) must not be dropped"
    );
    assert_eq!(post_batches[0].stamp, snap_tick as u32);

    // 3. Pre-watermark lag across >256 same-tick broadcasts must also resolve without stalling or
    //    replaying pre-snapshot events.
    for _ in 0..270 {
        assert!(
            sys_handle
                .emit_observer_destiny_event(
                    struct_id,
                    eve_destiny::DestinyEvent::OnSlimItemChange {
                        ball_id: nearby_ball_id,
                    },
                )
                .await
        );
        let _ = sys_handle.flush_peer_observer_updates(None).await.unwrap();
    }
    let lagged_resync = resync_structure_set_state_with_sys_rx(
        &ctx,
        &sys_handle,
        &mut sys_rx,
        sys_id,
        struct_id,
        &[struct_id],
        &mut wire_state,
    )
    .await
    .expect("structure SetState resync must recover from pre-watermark same-tick lag");
    assert_eq!(lagged_resync.tick, snap_tick);
    assert!(
        lagged_resync.post_batches.is_empty(),
        "lagged pre-watermark same-tick broadcasts must not replay"
    );
}

fn build_test_1477_ui_view_activated_event(
    ctx: &ServerContext,
    view_name: &str,
) -> eve_proto::quasar::EvePublicEvent {
    let mut msg = eve_proto::quasar::DynamicQuasarMessage::new(
        "eve_public.app.eveonline.generic_ui.view.Activated",
    );
    msg.set(
        1,
        eve_proto::quasar::DynamicValue::String(view_name.to_string()),
    );
    let value = ctx
        .quasar_gateway
        .descriptors
        .encode_dynamic(&msg)
        .unwrap_or_else(|_| {
            let b = view_name.as_bytes();
            let mut out = Vec::with_capacity(2 + b.len());
            out.push(0x0a);
            out.push(b.len() as u8);
            out.extend_from_slice(b);
            out
        });
    eve_proto::quasar::EvePublicEvent {
        payload: Some(eve_proto::quasar::GoogleAny {
            type_url: "type.googleapis.com/eve_public.app.eveonline.generic_ui.view.Activated"
                .to_string(),
            value,
        }),
        ..Default::default()
    }
}

fn grpc_style_quasar_session(
    grpc_session_id: u64,
    user_id: u64,
    char_id: i64,
) -> eve_machonet::SessionState {
    let mut sess = eve_machonet::SessionState::new(grpc_session_id);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1477_view_outside_redock_plex_exit_to_hangar_without_oid_defers_undock_set_state() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_144_u32;
    let char_id = 90_000_001_i64;
    let struct_id = 1_040_001_477_101_i64;
    let nearby_ball_id = 900_000_147_701_i64;
    let (ship_id, sys_handle) =
        setup_upwell_docked_pilot(&ctx, char_id, sys_id, struct_id, nearby_ball_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: nearby_ball_id,
            type_id: 1230,
            position: eve_destiny::DVec3::new(254_000.0, 0.0, 0.0),
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

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) = connect_and_authenticate(addr, "demo", "demo-secret").await;
    let (sel_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    assert_eq!(sel_rsp.packet_type, MachoPacketType::CallRsp);

    // Step 2: View Outside -> ship.Undock succeeds cleanly.
    let (form1_rsp, form1_notifs) =
        test_1386_call_rpc(&mut s, client_id, user_id, 2, "beyonce", "GetFormations", Vec::new())
            .await;
    assert_eq!(form1_rsp.packet_type, MachoPacketType::CallRsp);
    let mut form1_states = Vec::new();
    for pkt in &form1_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut form1_states);
    }
    assert_eq!(form1_states.len(), 1);
    assert_eq!(form1_states[0].0, Some(struct_id));

    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "timeControl",
        "AdvanceClock",
        vec![PyRep::Integer(12)],
    )
    .await;
    let (undock1_rsp, undock1_notifs) =
        test_1386_call_rpc(&mut s, client_id, user_id, 4, "ship", "Undock", Vec::new()).await;
    assert_eq!(undock1_rsp.packet_type, MachoPacketType::CallRsp);
    let mut undock1_states = Vec::new();
    for pkt in &undock1_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut undock1_states);
    }
    assert_eq!(undock1_states.len(), 1);
    assert_eq!(undock1_states[0].0, Some(ship_id));

    // Step 3: Redock via stationSvc.Dock -> View Outside persists in client and binds beyonce.
    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        5,
        "timeControl",
        "AdvanceClock",
        vec![PyRep::Integer(12)],
    )
    .await;
    let (dock_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        6,
        "stationSvc",
        "Dock",
        vec![PyRep::Integer(struct_id)],
    )
    .await;
    assert_eq!(dock_rsp.packet_type, MachoPacketType::CallRsp);
    assert_eq!(sys_handle.event_tx.receiver_count(), 0);

    let (form2_rsp, form2_notifs) =
        test_1386_call_rpc(&mut s, client_id, user_id, 7, "beyonce", "GetFormations", Vec::new())
            .await;
    assert_eq!(form2_rsp.packet_type, MachoPacketType::CallRsp);
    let mut form2_states = Vec::new();
    for pkt in &form2_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut form2_states);
    }
    assert_eq!(form2_states.len(), 1);
    assert_eq!(form2_states[0].0, Some(struct_id));

    let (bind2_rsp, bind2_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        8,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(bind2_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !bind2_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "SetState"))
    );
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);

    let macho_sid = ctx
        .db
        .get_online_session(user_id)
        .expect("online session")
        .session_id;
    assert!(ctx.is_space_ballpark_bootstrapped(macho_sid, char_id, sys_id));
    let stale_beyonce_bid = ctx
        .bound_objects
        .read()
        .unwrap()
        .values()
        .find(|e| e.session_id == macho_sid && e.service == "beyonce")
        .map(|e| e.bound_id)
        .expect("redock View Outside beyonce bound id");

    // Step 4: Open PLEX Store (`virtual_goods_store` secondary view + storeManager RPCs) -> keeps
    // View Outside intact, then click `exitButton` -> Quasar `Activated("hangar")` WITHOUT `OID-`.
    let grpc_sess = grpc_style_quasar_session(999_147_701, user_id, char_id);
    ctx.quasar_gateway.publish_event(
        &grpc_sess,
        build_test_1477_ui_view_activated_event(&ctx, "virtual_goods_store"),
        ctx.now_unix(),
    );
    let (cats_rsp, _) =
        test_1386_call_rpc(&mut s, client_id, user_id, 9, "storeManager", "get_categories", Vec::new())
            .await;
    assert_eq!(cats_rsp.packet_type, MachoPacketType::CallRsp);
    let (offers_rsp, _) =
        test_1386_call_rpc(&mut s, client_id, user_id, 10, "storeManager", "get_offers", Vec::new())
            .await;
    assert_eq!(offers_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        ctx.is_space_ballpark_bootstrapped(macho_sid, char_id, sys_id),
        "opening secondary virtual_goods_store overlay must not clear View Outside"
    );

    ctx.quasar_gateway.publish_event(
        &grpc_sess,
        build_test_1477_ui_view_activated_event(&ctx, "hangar"),
        ctx.now_unix(),
    );
    assert!(
        !ctx.is_space_ballpark_bootstrapped(macho_sid, char_id, sys_id),
        "Quasar Activated(\"hangar\") must immediately clear stale View Outside bootstrap"
    );

    // Step 5: ship.Undock BEFORE 30s-delayed `OID-` arrives must NOT emit premature SetState;
    // post-OnAddBallpark `beyonce.GetFormations` / `MachoBindObject` delivers SetState(ego=ship_id).
    let _ = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        11,
        "timeControl",
        "AdvanceClock",
        vec![PyRep::Integer(12)],
    )
    .await;
    let (undock2_rsp, undock2_notifs) =
        test_1386_call_rpc(&mut s, client_id, user_id, 12, "ship", "Undock", Vec::new()).await;
    assert_eq!(undock2_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !undock2_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "SetState")
                || test_1386_packet_contains_ascii(&p.to_pyrep(), "DoDestinyUpdate")),
        "ship.Undock after PLEX exit to hangar without OID- must defer SetState until OnAddBallpark"
    );

    let _ = sys_handle.step_manual(1.0).await.unwrap();
    let (form3_rsp, form3_notifs) =
        test_1386_call_rpc(&mut s, client_id, user_id, 13, "beyonce", "GetFormations", Vec::new())
            .await;
    assert_eq!(form3_rsp.packet_type, MachoPacketType::CallRsp);
    let mut space_states = Vec::new();
    for pkt in &form3_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut space_states);
    }
    assert_eq!(
        space_states.len(),
        1,
        "post-OnAddBallpark GetFormations must deliver SetState(ego=ship_id) once"
    );
    assert_eq!(space_states[0].0, Some(ship_id));
    assert!(space_states[0].1.iter().any(|b| b.id == ship_id));
    assert!(space_states[0].1.iter().any(|b| b.id == struct_id));
    assert!(space_states[0].1.iter().any(|b| b.id == nearby_ball_id));

    let (bind3_rsp, bind3_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        14,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(bind3_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !bind3_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "SetState"))
    );

    // Delayed 30s `OID-` releasing the old docked `beyonce` binding while in space must not
    // disturb the live post-undock `sys_rx` stream.
    let (late_rel_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        15,
        "machoNet",
        "ClientHasReleasedTheseObjects",
        vec![PyRep::List(vec![ctx.bound_object_wire_rep(stale_beyonce_bid)])],
    )
    .await;
    assert_eq!(late_rel_rsp.packet_type, MachoPacketType::CallRsp);
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1477_quasar_hangar_tick_detach_plex_preserve_and_reenter_view_outside_before_oid() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_144_u32;
    let char_id = 90_000_001_i64;
    let struct_id = 1_040_001_477_102_i64;
    let nearby_ball_id = 900_000_147_702_i64;
    let (ship_id, sys_handle) =
        setup_upwell_docked_pilot(&ctx, char_id, sys_id, struct_id, nearby_ball_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: nearby_ball_id,
            type_id: 1230,
            position: eve_destiny::DVec3::new(254_000.0, 0.0, 0.0),
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

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) = connect_and_authenticate(addr, "demo", "demo-secret").await;
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

    // 1. Enter View Outside (`B1`), verify 1Hz tick detachment when Quasar reports `"hangar"`.
    let (form1_rsp, _) =
        test_1386_call_rpc(&mut s, client_id, user_id, 2, "beyonce", "GetFormations", Vec::new())
            .await;
    assert_eq!(form1_rsp.packet_type, MachoPacketType::CallRsp);
    let (bind1_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(bind1_rsp.packet_type, MachoPacketType::CallRsp);
    let macho_sid = ctx
        .db
        .get_online_session(user_id)
        .expect("online session")
        .session_id;
    let bid_1 = ctx
        .bound_objects
        .read()
        .unwrap()
        .values()
        .find(|e| e.session_id == macho_sid && e.service == "beyonce")
        .map(|e| e.bound_id)
        .expect("first beyonce bound id");
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);

    // Malformed protobuf and mismatched-user sessions must be harmlessly ignored.
    let mut wrong_user_sess = eve_machonet::SessionState::new(999_147_702);
    wrong_user_sess.user_id = Some(user_id + 999);
    wrong_user_sess.character_id = Some(char_id);
    ctx.quasar_gateway.publish_event(
        &wrong_user_sess,
        build_test_1477_ui_view_activated_event(&ctx, "hangar"),
        ctx.now_unix(),
    );
    ctx.quasar_gateway.publish_event(
        &grpc_style_quasar_session(999_147_703, user_id, char_id),
        eve_proto::quasar::EvePublicEvent {
            payload: Some(eve_proto::quasar::GoogleAny {
                type_url: "type.googleapis.com/eve_public.app.eveonline.generic_ui.view.Activated"
                    .to_string(),
                value: vec![0xff, 0xff, 0xff],
            }),
            ..Default::default()
        },
        ctx.now_unix(),
    );
    assert!(ctx.is_space_ballpark_bootstrapped(macho_sid, char_id, sys_id));

    // Legitimate `"hangar"` activation clears bootstrap and causes the 1Hz tick arm to detach `sys_rx`
    // without waiting for 30s-delayed `OID-`.
    let grpc_sess = grpc_style_quasar_session(999_147_704, user_id, char_id);
    ctx.quasar_gateway.publish_event(
        &grpc_sess,
        build_test_1477_ui_view_activated_event(&ctx, "hangar"),
        ctx.now_unix(),
    );
    let _ = sys_handle.step_manual(1.0).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while sys_handle.event_tx.receiver_count() > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("1Hz tick must detach stale View Outside sys_rx after Activated(\"hangar\")");

    // 2. Re-enter View Outside (`Activated("structure")` -> GetFormations -> MachoBindObject `B2`)
    // BEFORE 30s `OID-` for `B1` arrives; delayed `B1` release must NOT break `B2`.
    ctx.quasar_gateway.publish_event(
        &grpc_sess,
        build_test_1477_ui_view_activated_event(&ctx, "structure"),
        ctx.now_unix(),
    );
    let (form2_rsp, form2_notifs) =
        test_1386_call_rpc(&mut s, client_id, user_id, 4, "beyonce", "GetFormations", Vec::new())
            .await;
    assert_eq!(form2_rsp.packet_type, MachoPacketType::CallRsp);
    let mut view2_states = Vec::new();
    for pkt in &form2_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut view2_states);
    }
    assert_eq!(view2_states.len(), 1);
    assert_eq!(view2_states[0].0, Some(struct_id));

    let (bind2_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        5,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(bind2_rsp.packet_type, MachoPacketType::CallRsp);
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);

    let (rel_b1_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        6,
        "machoNet",
        "ClientHasReleasedTheseObjects",
        vec![PyRep::List(vec![ctx.bound_object_wire_rep(bid_1)])],
    )
    .await;
    assert_eq!(rel_b1_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        ctx.is_space_ballpark_bootstrapped(macho_sid, char_id, sys_id),
        "releasing superseded beyonce binding B1 must not clear fresh View Outside B2"
    );
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);

    // 3. Legitimate `View Outside -> open_plex_store -> exit back to Structure -> ship.Undock`
    // MUST still preserve View Outside and deliver `SetState(ego=ship_id)` directly on `Undock`.
    ctx.quasar_gateway.publish_event(
        &grpc_sess,
        build_test_1477_ui_view_activated_event(&ctx, "virtual_goods_store"),
        ctx.now_unix(),
    );
    ctx.quasar_gateway.publish_event(
        &grpc_sess,
        build_test_1477_ui_view_activated_event(&ctx, "structure"),
        ctx.now_unix(),
    );
    ctx.advance_secs(12);
    let (undock_rsp, undock_notifs) =
        test_1386_call_rpc(&mut s, client_id, user_id, 8, "ship", "Undock", Vec::new()).await;
    assert_eq!(undock_rsp.packet_type, MachoPacketType::CallRsp);
    let mut undock_states = Vec::new();
    for pkt in &undock_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut undock_states);
    }
    assert_eq!(
        undock_states.len(),
        1,
        "View Outside -> PLEX Store -> exit back to Structure -> Undock must emit SetState(ego=ship_id) directly"
    );
    assert_eq!(undock_states[0].0, Some(ship_id));
    assert!(undock_states[0].1.iter().any(|b| b.id == nearby_ball_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1477_f01_quasar_ui_view_activated_fails_closed_without_auth_or_while_in_space() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_144_u32;
    let char_id = 90_000_001_i64;
    let struct_id = 1_040_001_477_103_i64;
    let nearby_ball_id = 900_000_147_703_i64;
    let (ship_id, sys_handle) =
        setup_upwell_docked_pilot(&ctx, char_id, sys_id, struct_id, nearby_ball_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: nearby_ball_id,
            type_id: 1230,
            position: eve_destiny::DVec3::new(254_000.0, 0.0, 0.0),
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

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) = connect_and_authenticate(addr, "demo", "demo-secret").await;
    let (sel_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    assert_eq!(sel_rsp.packet_type, MachoPacketType::CallRsp);

    let (form_rsp, _) =
        test_1386_call_rpc(&mut s, client_id, user_id, 2, "beyonce", "GetFormations", Vec::new())
            .await;
    assert_eq!(form_rsp.packet_type, MachoPacketType::CallRsp);
    let (bind_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(bind_rsp.packet_type, MachoPacketType::CallRsp);
    let macho_sid = ctx
        .db
        .get_online_session(user_id)
        .expect("online session")
        .session_id;
    assert!(ctx.is_space_ballpark_bootstrapped(macho_sid, char_id, sys_id));
    assert!(ctx.has_bound_service(macho_sid, "beyonce"));

    // 1. Fail-closed auth/session checks while docked in structure View Outside:
    //    `user_id == None`, wrong `character_id`, unknown `user_id`, and NPC station-docked
    //    character must all leave `space_ballpark_bootstrapped` and `beyonce` binding intact.
    let mut missing_uid_sess = eve_machonet::SessionState::new(999_147_710);
    missing_uid_sess.user_id = None;
    missing_uid_sess.character_id = Some(char_id);
    ctx.quasar_gateway.publish_event(
        &missing_uid_sess,
        build_test_1477_ui_view_activated_event(&ctx, "hangar"),
        ctx.now_unix(),
    );

    let wrong_char_sess = grpc_style_quasar_session(999_147_711, user_id, char_id + 999);
    ctx.quasar_gateway.publish_event(
        &wrong_char_sess,
        build_test_1477_ui_view_activated_event(&ctx, "hangar"),
        ctx.now_unix(),
    );

    let no_online_sess = grpc_style_quasar_session(999_147_712, user_id + 999, char_id);
    ctx.quasar_gateway.publish_event(
        &no_online_sess,
        build_test_1477_ui_view_activated_event(&ctx, "hangar"),
        ctx.now_unix(),
    );

    let valid_grpc_sess = grpc_style_quasar_session(999_147_713, user_id, char_id);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).expect("char");
        ch.station_id = Some(60_003_760);
        ch.structure_id = None;
    });
    ctx.quasar_gateway.publish_event(
        &valid_grpc_sess,
        build_test_1477_ui_view_activated_event(&ctx, "hangar"),
        ctx.now_unix(),
    );
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).expect("char");
        ch.station_id = None;
        ch.structure_id = Some(struct_id);
    });
    assert!(
        ctx.is_space_ballpark_bootstrapped(macho_sid, char_id, sys_id),
        "unauthenticated/mismatched/station Activated(\"hangar\") must fail closed"
    );
    assert!(ctx.has_bound_service(macho_sid, "beyonce"));

    async fn drain_post_call_packets(
        stream: &mut TcpStream,
        wait_for_set_state: bool,
    ) -> Vec<MachoPacket> {
        let mut pkts = Vec::new();
        let wait_window = if wait_for_set_state {
            std::time::Duration::from_secs(2)
        } else {
            std::time::Duration::from_millis(50)
        };
        let deadline = tokio::time::Instant::now() + wait_window;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, read_framed_pyrep(stream)).await {
                Ok(Ok(Some(rep))) => {
                    let pkt = MachoPacket::from_pyrep(&rep).expect("decode post-CallRsp packet");
                    let saw_set_state =
                        test_1386_packet_contains_ascii(&pkt.to_pyrep(), "SetState");
                    pkts.push(pkt);
                    if wait_for_set_state && saw_set_state {
                        break;
                    }
                }
                Ok(Ok(None)) => panic!("unexpected EOF draining post-CallRsp packets"),
                Ok(Err(e)) => panic!("read error draining post-CallRsp packets: {e:?}"),
                Err(_) => break,
            }
        }
        pkts
    }

    // 2. Undock into space -> explicit `beyonce.UpdateStateRequest` succeeds and starts 5-tick cooldown.
    ctx.advance_secs(12);
    let (undock_rsp, _) =
        test_1386_call_rpc(&mut s, client_id, user_id, 4, "ship", "Undock", Vec::new()).await;
    assert_eq!(undock_rsp.packet_type, MachoPacketType::CallRsp);
    let _ = drain_post_call_packets(&mut s, false).await;
    let _ = sys_handle.step_manual(1.0).await.unwrap();
    let (usr1_rsp, mut usr1_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        5,
        "beyonce",
        "UpdateStateRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(usr1_rsp.packet_type, MachoPacketType::CallRsp);
    let usr1_pre_has_set_state = usr1_notifs
        .iter()
        .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "SetState"));
    usr1_notifs.extend(drain_post_call_packets(&mut s, !usr1_pre_has_set_state).await);
    let mut usr1_states = Vec::new();
    for pkt in &usr1_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut usr1_states);
    }
    assert_eq!(usr1_states.len(), 1);
    assert_eq!(usr1_states[0].0, Some(ship_id));

    // 3. Publishing `Activated("hangar")` or `Activated("charsel")` while undocked in space must
    //    NOT clear `space_ballpark_bootstrapped`, prune bindings, or reset `UpdateStateRequest` cooldown.
    ctx.quasar_gateway.publish_event(
        &valid_grpc_sess,
        build_test_1477_ui_view_activated_event(&ctx, "hangar"),
        ctx.now_unix(),
    );
    ctx.quasar_gateway.publish_event(
        &valid_grpc_sess,
        build_test_1477_ui_view_activated_event(&ctx, "charsel"),
        ctx.now_unix(),
    );
    assert!(
        ctx.is_space_ballpark_bootstrapped(macho_sid, char_id, sys_id),
        "Activated(\"hangar\"/\"charsel\") while in space must not clear space_ballpark_bootstrapped"
    );
    assert!(ctx.has_bound_service(macho_sid, "beyonce"));

    let _ = sys_handle.step_manual(1.0).await.unwrap();
    let (usr2_rsp, mut usr2_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        6,
        "beyonce",
        "UpdateStateRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(usr2_rsp.packet_type, MachoPacketType::CallRsp);
    usr2_notifs.extend(drain_post_call_packets(&mut s, false).await);
    assert!(
        !usr2_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "SetState")),
        "in-space Activated(\"hangar\") must not bypass UpdateStateRequest 5-tick cooldown"
    );

    let (bind_space_rsp, mut bind_space_notifs) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        7,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(bind_space_rsp.packet_type, MachoPacketType::CallRsp);
    bind_space_notifs.extend(drain_post_call_packets(&mut s, false).await);
    assert!(
        !bind_space_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "SetState")),
        "late in-space Activated(\"hangar\") must not cause duplicate SetState on MachoBindObject"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1477_f02_reenter_view_outside_getformations_then_delayed_oid_before_bind_preserves_undock()
{
    async fn call_with_oid_minus(
        stream: &mut TcpStream,
        ctx: &ServerContext,
        client_id: u64,
        user_id: u64,
        call_id: u64,
        service: &str,
        method: &str,
        args: Vec<PyRep>,
        oid_bid: u64,
    ) -> (MachoPacket, Vec<MachoPacket>) {
        let mut req = make_call_req(client_id, user_id, call_id, service, None, method, args);
        req.named_payload.insert(
            "OID-".to_string(),
            PyRep::List(vec![ctx.bound_object_wire_rep(oid_bid)]),
        );
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

    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_144_u32;
    let char_id = 90_000_001_i64;
    let struct_id = 1_040_001_477_104_i64;
    let nearby_ball_id = 900_000_147_704_i64;
    let (ship_id, sys_handle) =
        setup_upwell_docked_pilot(&ctx, char_id, sys_id, struct_id, nearby_ball_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: nearby_ball_id,
            type_id: 1230,
            position: eve_destiny::DVec3::new(254_000.0, 0.0, 0.0),
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

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) = connect_and_authenticate(addr, "demo", "demo-secret").await;
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

    // 1. Initial View Outside (`GetFormations` -> `MachoBindObject` B1).
    let (form1_rsp, _) =
        test_1386_call_rpc(&mut s, client_id, user_id, 2, "beyonce", "GetFormations", Vec::new())
            .await;
    assert_eq!(form1_rsp.packet_type, MachoPacketType::CallRsp);
    let (bind1_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(bind1_rsp.packet_type, MachoPacketType::CallRsp);
    let macho_sid = ctx
        .db
        .get_online_session(user_id)
        .expect("online session")
        .session_id;
    let bid_1 = ctx
        .bound_objects
        .read()
        .unwrap()
        .values()
        .find(|e| e.session_id == macho_sid && e.service == "beyonce")
        .map(|e| e.bound_id)
        .expect("initial beyonce bound id B1");
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);

    // 2. Exit View Outside (`Activated("hangar")`), pruning superseded docked B1 immediately.
    let grpc_sess = grpc_style_quasar_session(999_147_720, user_id, char_id);
    ctx.quasar_gateway.publish_event(
        &grpc_sess,
        build_test_1477_ui_view_activated_event(&ctx, "hangar"),
        ctx.now_unix(),
    );
    assert!(!ctx.is_space_ballpark_bootstrapped(macho_sid, char_id, sys_id));
    assert!(!ctx.has_bound_service(macho_sid, "beyonce"));

    // 3. Re-enter View Outside via V24's real `GetFormations` -> `MachoBindObject` order BEFORE 1Hz tick
    //    or 30s `OID-(B1)` arrives.
    let (form2_rsp, form2_notifs) =
        test_1386_call_rpc(&mut s, client_id, user_id, 4, "beyonce", "GetFormations", Vec::new())
            .await;
    assert_eq!(form2_rsp.packet_type, MachoPacketType::CallRsp);
    let mut view2_states = Vec::new();
    for pkt in &form2_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut view2_states);
    }
    assert_eq!(view2_states.len(), 1);
    assert_eq!(view2_states[0].0, Some(struct_id));
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);

    // 4. Dedicated `ClientHasReleasedTheseObjects` notification releasing stale `B1` BETWEEN
    //    `GetFormations` and `MachoBindObject` must NOT tear down the fresh View Outside session.
    let rel_notif = MachoPacket {
        packet_type: MachoPacketType::Notification,
        source: MachoAddress::Client {
            client_id,
            call_id: 0,
            service: None,
        },
        destination: MachoAddress::Broadcast {
            broadcast_id: "ClientHasReleasedTheseObjects".to_string(),
            id_type: "clientID".to_string(),
            narrowcast: Vec::new(),
        },
        user_id: Some(user_id),
        payload: PyRep::List(vec![ctx.bound_object_wire_rep(bid_1)]),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &rel_notif.to_pyrep()).await.unwrap();

    // 5. Delayed `OID-(B1)` piggybacked directly on `beyonce.MachoBindObject` (creating B2):
    //    must neither tear down View Outside nor emit a duplicate `SetState`.
    let (bind2_rsp, bind2_notifs) = call_with_oid_minus(
        &mut s,
        &ctx,
        client_id,
        user_id,
        5,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
        bid_1,
    )
    .await;
    assert_eq!(bind2_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        !bind2_notifs
            .iter()
            .any(|p| test_1386_packet_contains_ascii(&p.to_pyrep(), "SetState")),
        "OID-(B1) piggybacked on MachoBindObject must not wipe bootstrap or cause duplicate SetState"
    );
    assert!(ctx.is_space_ballpark_bootstrapped(macho_sid, char_id, sys_id));
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);
    let bid_2 = ctx
        .bound_objects
        .read()
        .unwrap()
        .values()
        .find(|e| e.session_id == macho_sid && e.service == "beyonce")
        .map(|e| e.bound_id)
        .expect("fresh beyonce bound id B2");
    assert_ne!(bid_2, bid_1);

    // 6. Repeat via 1Hz tick detach -> `GetFormations` -> CallReq `ClientHasReleasedTheseObjects(B2)`
    //    -> `ship.Undock` WITH piggybacked `OID-(B2)` before binding B3 -> must deliver `SetState(ego=ship_id)`.
    ctx.quasar_gateway.publish_event(
        &grpc_sess,
        build_test_1477_ui_view_activated_event(&ctx, "hangar"),
        ctx.now_unix(),
    );
    let _ = sys_handle.step_manual(1.0).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while sys_handle.event_tx.receiver_count() > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("1Hz tick must detach sys_rx");

    let (form3_rsp, form3_notifs) =
        test_1386_call_rpc(&mut s, client_id, user_id, 6, "beyonce", "GetFormations", Vec::new())
            .await;
    assert_eq!(form3_rsp.packet_type, MachoPacketType::CallRsp);
    let mut view3_states = Vec::new();
    for pkt in &form3_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut view3_states);
    }
    assert_eq!(view3_states.len(), 1);
    assert_eq!(view3_states[0].0, Some(struct_id));

    let (rel2_rsp, _) = test_1386_call_rpc(
        &mut s,
        client_id,
        user_id,
        7,
        "machoNet",
        "ClientHasReleasedTheseObjects",
        vec![PyRep::List(vec![ctx.bound_object_wire_rep(bid_2)])],
    )
    .await;
    assert_eq!(rel2_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(
        ctx.is_space_ballpark_bootstrapped(macho_sid, char_id, sys_id),
        "CallReq release of superseded B2 between GetFormations and MachoBindObject must preserve View Outside"
    );
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);

    ctx.advance_secs(12);
    let (undock_rsp, undock_notifs) = call_with_oid_minus(
        &mut s,
        &ctx,
        client_id,
        user_id,
        8,
        "ship",
        "Undock",
        Vec::new(),
        bid_2,
    )
    .await;
    assert_eq!(undock_rsp.packet_type, MachoPacketType::CallRsp);
    let mut undock_states = Vec::new();
    for pkt in &undock_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut undock_states);
    }
    assert_eq!(
        undock_states.len(),
        1,
        "ship.Undock after delayed OID-(B2) release must deliver SetState(ego=ship_id) directly"
    );
    assert_eq!(undock_states[0].0, Some(ship_id));
    assert!(undock_states[0].1.iter().any(|b| b.id == struct_id));
    assert!(undock_states[0].1.iter().any(|b| b.id == nearby_ball_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_1477_r2_f01_displaced_session_tick_and_rpc_preserve_replacement_view_outside() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_144_u32;
    let char_id = 90_000_001_i64;
    let struct_id = 1_040_001_477_105_i64;
    let nearby_ball_id = 900_000_147_705_i64;
    let (ship_id, sys_handle) =
        setup_upwell_docked_pilot(&ctx, char_id, sys_id, struct_id, nearby_ball_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: nearby_ball_id,
            type_id: 1230,
            position: eve_destiny::DVec3::new(254_000.0, 0.0, 0.0),
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

    let addr = spawn_macho_test_server(Arc::clone(&ctx)).await;

    // 1. Session S1 connects, selects char, and enters structure View Outside.
    let (mut s1, cid1, user_id) = connect_and_authenticate(addr, "demo", "demo-secret").await;
    let (sel1_rsp, _) = test_1386_call_rpc(
        &mut s1,
        cid1,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    assert_eq!(sel1_rsp.packet_type, MachoPacketType::CallRsp);
    let sid1 = ctx.db.get_online_session(user_id).expect("s1 online").session_id;
    let (form1_rsp, _) =
        test_1386_call_rpc(&mut s1, cid1, user_id, 2, "beyonce", "GetFormations", Vec::new())
            .await;
    assert_eq!(form1_rsp.packet_type, MachoPacketType::CallRsp);
    let (bind1_rsp, _) = test_1386_call_rpc(
        &mut s1,
        cid1,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(bind1_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(ctx.is_space_ballpark_bootstrapped(sid1, char_id, sys_id));
    assert!(ctx.has_bound_service(sid1, "beyonce"));
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);

    // 2. Release S1's online lock so S1's live TCP task remains open (modeling pre-close_rx
    //    displacement), then replacement session S2 logs in and enters structure View Outside.
    ctx.db.release_online_lock(user_id, sid1);
    let (mut s2, cid2, uid2) = connect_and_authenticate(addr, "demo", "demo-secret").await;
    assert_eq!(uid2, user_id);
    let (sel2_rsp, _) = test_1386_call_rpc(
        &mut s2,
        cid2,
        user_id,
        1,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    )
    .await;
    assert_eq!(sel2_rsp.packet_type, MachoPacketType::CallRsp);
    let sid2 = ctx.db.get_online_session(user_id).expect("s2 online").session_id;
    assert_ne!(sid1, sid2);

    let (form2_rsp, form2_notifs) =
        test_1386_call_rpc(&mut s2, cid2, user_id, 2, "beyonce", "GetFormations", Vec::new())
            .await;
    assert_eq!(form2_rsp.packet_type, MachoPacketType::CallRsp);
    let mut form2_states = Vec::new();
    for pkt in &form2_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut form2_states);
    }
    assert_eq!(form2_states.len(), 1);
    assert_eq!(form2_states[0].0, Some(struct_id));

    let (bind2_rsp, _) = test_1386_call_rpc(
        &mut s2,
        cid2,
        user_id,
        3,
        "beyonce",
        "MachoBindObject",
        vec![PyRep::Integer(i64::from(sys_id)), PyRep::None],
    )
    .await;
    assert_eq!(bind2_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(ctx.is_space_ballpark_bootstrapped(sid2, char_id, sys_id));
    assert!(ctx.has_bound_service(sid2, "beyonce"));
    assert_eq!(sys_handle.event_tx.receiver_count(), 2);

    // 3. First 1Hz tick wakes displaced S1's tick_res arm (macho_listener.rs:3189-3205):
    //    must detach ONLY S1 (`receiver_count` 2 -> 1) and preserve S2's bootstrap & binding.
    let _ = sys_handle.step_manual(1.0).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while sys_handle.event_tx.receiver_count() > 1 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("displaced S1 1Hz tick must detach S1 without waiting");
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);
    assert!(!ctx.has_bound_service(sid1, "beyonce"));
    assert!(
        ctx.is_space_ballpark_bootstrapped(sid2, char_id, sys_id),
        "displaced S1 tick detach must preserve S2's space_ballpark_bootstrapped"
    );
    assert!(
        ctx.has_bound_service(sid2, "beyonce"),
        "displaced S1 tick detach must preserve S2's beyonce binding"
    );

    // Emit damage + second 1Hz tick to prove S2's tick_res arm stays subscribed and delivers ticks.
    assert!(
        sys_handle
            .apply_damage(
                struct_id,
                eve_destiny::DamageVector {
                    em: 100.0,
                    thermal: 0.0,
                    kinetic: 0.0,
                    explosive: 0.0,
                },
            )
            .await
            .unwrap()
            .is_some()
    );
    let _ = sys_handle.step_manual(1.0).await.unwrap();
    test_1237_wait_for_notification(
        &mut s2,
        "DoDestinyUpdate",
        "replacement S2 must still receive 1Hz DoDestinyUpdate after S1 detached",
    )
    .await;
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);
    assert!(ctx.is_space_ballpark_bootstrapped(sid2, char_id, sys_id));
    assert!(ctx.has_bound_service(sid2, "beyonce"));

    // 4. Displaced wire session processing an in-flight RPC (macho_listener.rs:1605-1622)
    //    must detach only itself and keep S2's View Outside intact.
    let stale_wire_sid = 999_147_788_u64;
    let stale_wire_bid = 999_147_789_u64;
    {
        let mut bound = ctx.bound_objects.write().unwrap();
        bound.insert(
            stale_wire_bid,
            crate::router::BoundObjectEntry {
                bound_id: stale_wire_bid,
                session_id: stale_wire_sid,
                user_id,
                character_id: char_id,
                service: "beyonce".into(),
                object_id: i64::from(sys_id),
                oid_str: format!("{stale_wire_bid}"),
                ref_id: 0,
            },
        );
    }
    let mut stale_sess = SessionState::new(stale_wire_sid);
    stale_sess.user_id = Some(user_id);
    stale_sess.character_id = Some(char_id);
    stale_sess.solar_system_id = Some(sys_id);
    stale_sess.structure_id = Some(struct_id);
    stale_sess.station_id = None;
    let mut stale_sys_rx = sys_handle.subscribe();
    let mut stale_subscribed_sys_id = Some(sys_id);
    let mut stale_wire_state =
        crate::outbound_sequencer::SessionWireState::new(stale_wire_sid, 9_900_002);
    stale_wire_state.ensure_in_space(Some(sys_id));
    let mut stale_deferred_ship_id = None;
    let req_source = MachoAddress::Client {
        client_id: 9_900_002,
        call_id: 1,
        service: None,
    };
    let req_dest = MachoAddress::Node {
        node_id: MACHO_PROXY_NODE_ID,
        service: Some("account".into()),
        call_id: 0,
    };
    let kw = HashMap::new();
    {
        let mut refs = CallReqWireRefs {
            client_id: 9_900_002,
            sys_rx: &mut stale_sys_rx,
            subscribed_sys_id: &mut stale_subscribed_sys_id,
            wire_state: &mut stale_wire_state,
            deferred_ship_id: &mut stale_deferred_ship_id,
        };
        let _ = execute_call_req_wire(
            &ctx,
            &mut stale_sess,
            &mut refs,
            &req_source,
            &req_dest,
            "account",
            None,
            "GetCashBalance",
            &[PyRep::Integer(0)],
            &kw,
        )
        .await;
    }
    drop(stale_sys_rx);
    assert!(matches!(stale_wire_state.destiny, DestinyStream::Detached));
    assert!(stale_subscribed_sys_id.is_none());
    assert!(!ctx.has_bound_service(stale_wire_sid, "beyonce"));
    assert!(
        ctx.is_space_ballpark_bootstrapped(sid2, char_id, sys_id),
        "displaced pre-RPC detach must preserve S2's space_ballpark_bootstrapped"
    );
    assert!(ctx.has_bound_service(sid2, "beyonce"));
    assert_eq!(sys_handle.event_tx.receiver_count(), 1);

    // 5. Replacement S2 completes View Outside -> ship.Undock cleanly with SetState(ego=ship_id).
    ctx.advance_secs(12);
    let (undock2_rsp, undock2_notifs) =
        test_1386_call_rpc(&mut s2, cid2, user_id, 4, "ship", "Undock", Vec::new()).await;
    assert_eq!(undock2_rsp.packet_type, MachoPacketType::CallRsp);
    let mut undock2_states = Vec::new();
    for pkt in &undock2_notifs {
        collect_set_states_from_pyrep_tree(&pkt.payload, &mut undock2_states);
    }
    assert_eq!(
        undock2_states.len(),
        1,
        "S2 View Outside -> ship.Undock must still emit SetState(ego=ship_id) directly"
    );
    assert_eq!(undock2_states[0].0, Some(ship_id));
    assert!(undock2_states[0].1.iter().any(|b| b.id == struct_id));
    assert!(undock2_states[0].1.iter().any(|b| b.id == nearby_ball_id));
}
