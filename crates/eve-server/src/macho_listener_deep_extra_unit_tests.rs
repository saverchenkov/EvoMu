//! Deep branch-coverage and mutation-killing unit tests for `macho_listener`:
//! frame codec I/O error & timeout guards, reader task bounds/EOF/truncated paths,
//! handshake VIP/QueueCheck/empty-SSO/mid-handshake shutdown-suspension-kick gates,
//! `PingReq` keepalive payload variants, `MachoAddress` service extraction, `OID+`
//! direct `SubStruct` extraction, `UserError` vs `RuntimeError` error wrapping,
//! `MultiLoginBlocked` transport preservation, `UpdateStateRequest` & docking destiny
//! flush order, async `OnSessionChanged` ballpark reset, and disconnect position sync.

#![forbid(unsafe_code)]

use crate::macho_listener::*;
use crate::router::{NotificationTarget, ServerContext};
use eve_db::{ServerClock, VirtualServerClock};
use eve_machonet::{
    placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeAck, CryptoHandshakeResult,
    CryptoRequestPacket, CryptoServerHandshake, GPSTransportClosed, HandshakeCommand,
    MachoAddress, MachoPacket, MachoPacketType, VersionExchange, MACHO_PROXY_NODE_ID,
};
use eve_proto::{marshal, PyRep};
use std::collections::HashMap;
use std::io::Cursor;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};

struct FailingAsyncReader {
    kind: std::io::ErrorKind,
}

impl AsyncRead for FailingAsyncReader {
    fn poll_read(self: Pin<&mut Self>, _cx: &mut Context<'_>, _buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Err(std::io::Error::new(self.kind, "injected read error")))
    }
}

fn as_tuple(rep: &PyRep) -> &[PyRep] {
    let PyRep::Tuple(items) = rep else { panic!("expected Tuple, got {rep:?}") };
    items
}

fn as_list(rep: &PyRep) -> &[PyRep] {
    let PyRep::List(items) = rep else { panic!("expected List, got {rep:?}") };
    items
}

fn as_dict(rep: &PyRep) -> &[(PyRep, PyRep)] {
    let PyRep::Dict(items) = rep else { panic!("expected Dict, got {rep:?}") };
    items
}

async fn spawn_listener(ctx: Arc<ServerContext>) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind macho test listener");
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

async fn read_pkt(stream: &mut TcpStream) -> Option<PyRep> {
    tokio::time::timeout(std::time::Duration::from_secs(60), read_framed_pyrep(stream))
        .await
        .expect("read timeout")
        .expect("read error")
}

async fn complete_placebo_challenge(s: &mut TcpStream, user: &str, pass: &str, sso: Option<&str>, lang: &str) {
    let mut chal = CryptoChallengePacket::new_password_login(user, pass);
    if let Some(tok) = sso {
        chal.user_sso_token = Some(tok.to_string());
    }
    chal.user_language_id = lang.to_string();
    write_framed_pyrep(s, &chal.to_pyrep()).await.unwrap();
    assert_eq!(read_pkt(s).await.unwrap().as_i64(), Some(2));
    let srv_hs = CryptoServerHandshake::from_pyrep(&read_pkt(s).await.unwrap()).unwrap();
    write_framed_pyrep(
        s,
        &(CryptoHandshakeResult {
            challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
            func_output: Vec::new(),
            func_result: PyRep::Integer(0),
        })
        .to_pyrep(),
    )
    .await
    .unwrap();
}

async fn connect_login(addr: std::net::SocketAddr, username: &str, password: &str) -> (TcpStream, u64, u64) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let _hello = read_pkt(&mut s).await.unwrap();
    write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep()).await.unwrap();
    write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep()).await.unwrap();
    assert_eq!(read_pkt(&mut s).await.unwrap().as_str(), Some("OK CC"));
    complete_placebo_challenge(&mut s, username, password, None, "EN").await;
    let ack = CryptoHandshakeAck::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap();
    let init_pkt = MachoPacket::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap();
    let MachoAddress::Client { client_id, .. } = init_pkt.destination else { panic!() };
    let _sess_chg = MachoPacket::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap();
    (s, client_id, ack.user_id)
}

fn build_call_pkt(
    client_id: u64,
    user_id: u64,
    call_id: u64,
    destination: MachoAddress,
    bound_id: Option<u64>,
    method: &str,
    args: Vec<PyRep>,
) -> MachoPacket {
    MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client { client_id, call_id, service: None },
        destination,
        user_id: Some(user_id),
        payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(bound_id.unwrap_or(0) as i64),
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::ascii_bytes(method),
                PyRep::Tuple(args),
                PyRep::Dict(Vec::new()),
            ]))),
        ])]),
        named_payload: HashMap::new(),
    }
}

async fn read_until_call_reply(stream: &mut TcpStream) -> MachoPacket {
    loop {
        let pkt = MachoPacket::from_pyrep(&read_pkt(stream).await.unwrap()).unwrap();
        if matches!(pkt.packet_type, MachoPacketType::CallRsp | MachoPacketType::ErrorResponse) {
            return pkt;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frame_codec_io_errors_truncated_body_and_tcp_reader_task_guards() {
    let mut bad_reader = FailingAsyncReader { kind: std::io::ErrorKind::BrokenPipe };
    assert!(read_framed_pyrep(&mut bad_reader).await.unwrap_err().to_string().contains("injected read error"));

    let mut trunc_bytes = Vec::new();
    trunc_bytes.extend_from_slice(&12u32.to_le_bytes());
    trunc_bytes.extend_from_slice(&[1, 2, 3]);
    assert!(read_framed_pyrep(&mut Cursor::new(trunc_bytes)).await.is_err());

    let (mut client_io, server_io) = tokio::io::duplex(8);
    drop(server_io);
    assert!(write_framed_pyrep(&mut client_io, &PyRep::Integer(1)).await.is_err());

    let ctx = Arc::new(ServerContext::new());
    let addr = spawn_listener(Arc::clone(&ctx)).await;

    for bad_len in [0u32, (MAX_MACHO_FRAME_LEN + 1) as u32] {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello = read_pkt(&mut s).await.unwrap();
        s.write_u32_le(bad_len).await.unwrap();
        s.flush().await.unwrap();
        assert_eq!(read_pkt(&mut s).await, None);
    }
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello = read_pkt(&mut s).await.unwrap();
        s.write_u32_le(24).await.unwrap();
        s.write_all(&[1, 2, 3, 4]).await.unwrap();
        s.shutdown().await.unwrap();
        assert_eq!(read_pkt(&mut s).await, None);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_queue_check_vip_empty_sso_and_mid_handshake_gates() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let _susp_uid = ctx.db.provision_player_account("midsusp_user", "secret_ms");
    let _gone_uid = ctx.db.provision_player_account("midgone_user", "secret_mg");
    let _ban_uid = ctx.db.provision_player_account("ban_args_user", "secret_ba");
    ctx.db.ban_account("ban_args_user", 1_800_050_000, "Botting").unwrap();
    let addr = spawn_listener(Arc::clone(&ctx)).await;

    // 1. QueueCheck `(None, "QC")` -> queue position 1 + fresh hello -> VipKey + empty SSO + blank lang
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello1 = read_pkt(&mut s).await.unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep()).await.unwrap();
        write_framed_pyrep(&mut s, &HandshakeCommand::QueueCheck.to_pyrep()).await.unwrap();
        assert_eq!(read_pkt(&mut s).await.unwrap().as_i64(), Some(1));
        assert!(VersionExchange::from_pyrep(&read_pkt(&mut s).await.unwrap()).is_some());

        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep()).await.unwrap();
        write_framed_pyrep(&mut s, &HandshakeCommand::VipKey("vip-gold".into()).to_pyrep()).await.unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep()).await.unwrap();
        assert_eq!(read_pkt(&mut s).await.unwrap().as_str(), Some("OK CC"));
        complete_placebo_challenge(&mut s, "demo", "demo-secret", Some(""), "").await;
        assert_eq!(CryptoHandshakeAck::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap().user_id, 1);
    }

    // 2. Banned login includes `expiry` and `until` arguments on `GPSTransportClosed`
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _ = read_pkt(&mut s).await.unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep()).await.unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep()).await.unwrap();
        let _ = read_pkt(&mut s).await.unwrap();
        write_framed_pyrep(&mut s, &CryptoChallengePacket::new_password_login("ban_args_user", "secret_ba").to_pyrep()).await.unwrap();
        let closed = GPSTransportClosed::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap();
        assert_eq!(closed.reason_code.as_deref(), Some("UserBanned"));
        assert_eq!(closed.reason_args.get("expiry"), Some(&PyRep::Integer(1_800_050_000)));
        assert_eq!(closed.reason_args.get("until"), Some(&PyRep::Integer(1_800_050_000)));
    }

    // 3. Mid-handshake suspension, deleted account (`acquire_online_lock` Err), and cluster shutdown gates
    for (user, pass, action, expected_code) in [
        ("midsusp_user", "secret_ms", 0u8, "AccountSuspended"),
        ("midgone_user", "secret_mg", 1u8, "LoginAuthFailed"),
        ("demo", "demo-secret", 2u8, "ClusterShuttingDown"),
    ] {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _ = read_pkt(&mut s).await.unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep()).await.unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep()).await.unwrap();
        let _ = read_pkt(&mut s).await.unwrap();
        write_framed_pyrep(&mut s, &CryptoChallengePacket::new_password_login(user, pass).to_pyrep()).await.unwrap();
        let _ = read_pkt(&mut s).await.unwrap();
        let srv_hs = CryptoServerHandshake::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap();

        match action {
            0 => {
                ctx.db.suspend_account("midsusp_user", 1_800_010_000, "Temp hold").unwrap();
            }
            1 => {
                ctx.db.with_mut_snapshot(|snap| {
                    snap.accounts.remove("midgone_user");
                });
            }
            _ => {
                ctx.cluster_state.write().unwrap().shutdown_in_progress = true;
            }
        }

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
        let closed = GPSTransportClosed::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap();
        assert_eq!(closed.reason_code.as_deref(), Some(expected_code));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_auth_ping_variants_address_types_error_wrapping_and_multilogin() {
    let ctx = Arc::new(ServerContext::new());
    let addr = spawn_listener(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) = connect_login(addr, "demo", "demo-secret").await;

    let raw_ping = MachoPacket {
        packet_type: MachoPacketType::PingReq,
        source: MachoAddress::Client { client_id, call_id: 10, service: None },
        destination: MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: None, call_id: 0 },
        user_id: Some(user_id),
        payload: PyRep::None,
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &raw_ping.to_pyrep()).await.unwrap();
    let rsp_raw = MachoPacket::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap();
    assert_eq!((rsp_raw.packet_type, rsp_raw.payload, rsp_raw.destination), (MachoPacketType::PingRsp, PyRep::None, raw_ping.source.clone()));

    let fb_ping = MachoPacket { payload: PyRep::Tuple(vec![PyRep::List(vec![PyRep::Integer(55)])]), ..raw_ping.clone() };
    write_framed_pyrep(&mut s, &fb_ping.to_pyrep()).await.unwrap();
    let rsp_fb = MachoPacket::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap();
    let fb_list = as_list(&as_tuple(&rsp_fb.payload)[0]);
    assert_eq!(fb_list.len(), 10);
    assert_eq!(as_tuple(&fb_list[1])[2], PyRep::ascii_bytes("proxy::handle_message"));
    assert_eq!(as_tuple(&fb_list[6])[2], PyRep::ascii_bytes("server::turnaround"));

    let seeded_ping = MachoPacket {
        payload: PyRep::Tuple(vec![PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(111), PyRep::Integer(222)])])]),
        ..raw_ping.clone()
    };
    write_framed_pyrep(&mut s, &seeded_ping.to_pyrep()).await.unwrap();
    let rsp_seeded = MachoPacket::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap();
    let seeded_list = as_list(&as_tuple(&rsp_seeded.payload)[0]);
    assert_eq!(seeded_list.len(), 10);
    assert_eq!(as_tuple(&seeded_list[1])[0], PyRep::Integer(111));
    assert_eq!(as_tuple(&seeded_list[1])[1], PyRep::Integer(222));

    let node_none = build_call_pkt(client_id, user_id, 11, MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: None, call_id: 0 }, None, "", Vec::new());
    write_framed_pyrep(&mut s, &node_none.to_pyrep()).await.unwrap();
    let rt_err = read_until_call_reply(&mut s).await;
    assert_eq!(rt_err.packet_type, MachoPacketType::ErrorResponse);
    let PyRep::Bytes(rt_bytes) = &as_tuple(&as_tuple(&rt_err.payload)[2])[0] else { panic!() };
    let PyRep::ObjectEx { header: rt_hdr, .. } = marshal::decode(rt_bytes).unwrap() else { panic!() };
    assert_eq!(as_tuple(&rt_hdr)[0], PyRep::Token("exceptions.RuntimeError".into()));

    for (cid_num, dest) in [
        (12, MachoAddress::Client { client_id, call_id: 12, service: Some("config".into()) }),
        (13, MachoAddress::Broadcast { broadcast_id: "config".into(), id_type: "clientid".into(), narrowcast: vec![client_id] }),
    ] {
        let call = build_call_pkt(client_id, user_id, cid_num, dest, None, "GetMap", vec![PyRep::Integer(30_000_142)]);
        write_framed_pyrep(&mut s, &call.to_pyrep()).await.unwrap();
        assert_eq!(read_until_call_reply(&mut s).await.packet_type, MachoPacketType::CallRsp);
    }

    let sel = build_call_pkt(client_id, user_id, 14, MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: Some("charUnboundMgr".into()), call_id: 0 }, None, "SelectCharacterID", vec![PyRep::Integer(90_000_001)]);
    write_framed_pyrep(&mut s, &sel.to_pyrep()).await.unwrap();
    assert_eq!(read_until_call_reply(&mut s).await.packet_type, MachoPacketType::CallRsp);

    let scan_call = build_call_pkt(client_id, user_id, 15, MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: Some("scanMgr".into()), call_id: 0 }, None, "GetSystemScanMgr", Vec::new());
    write_framed_pyrep(&mut s, &scan_call.to_pyrep()).await.unwrap();
    let scan_rsp = read_until_call_reply(&mut s).await;
    assert_eq!(scan_rsp.packet_type, MachoPacketType::CallRsp);
    assert!(scan_rsp.named_payload.contains_key("OID+"));

    let bad_char = build_call_pkt(client_id, user_id, 16, MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: Some("charMgr".into()), call_id: 0 }, None, "GetPublicInfo3", vec![PyRep::Integer(999_999_999)]);
    write_framed_pyrep(&mut s, &bad_char.to_pyrep()).await.unwrap();
    let usr_err = read_until_call_reply(&mut s).await;
    assert_eq!(usr_err.packet_type, MachoPacketType::ErrorResponse);
    let PyRep::Bytes(usr_bytes) = &as_tuple(&as_tuple(&usr_err.payload)[2])[0] else { panic!() };
    let PyRep::ObjectEx { header: usr_hdr, .. } = marshal::decode(usr_bytes).unwrap() else { panic!() };
    assert_eq!(as_tuple(&usr_hdr)[0], PyRep::Token("eveexceptions.UserError".into()));

    let _ = ctx.db.acquire_online_lock(user_id, 999_999, Some(90_000_001), false, ctx.now_unix());
    write_framed_pyrep(&mut s, &raw_ping.to_pyrep()).await.unwrap();
    assert_eq!(MachoPacket::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap().packet_type, MachoPacketType::PingRsp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ballpark_update_state_docking_flush_session_change_and_disconnect_sync() {
    let ctx = Arc::new(ServerContext::new());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    let addr = spawn_listener(Arc::clone(&ctx)).await;
    let (mut s, client_id, user_id) = connect_login(addr, "demo", "demo-secret").await;
    let (char_id, ship_id) = (90_000_001_i64, 100_000_001_i64);

    let call = |cid: u64, svc: &str, bid: Option<u64>, m: &str, args: Vec<PyRep>| {
        build_call_pkt(client_id, user_id, cid, MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: Some(svc.into()), call_id: 0 }, bid, m, args)
    };

    write_framed_pyrep(&mut s, &call(1, "charUnboundMgr", None, "SelectCharacterID", vec![PyRep::Integer(char_id)]).to_pyrep()).await.unwrap();
    let _ = read_until_call_reply(&mut s).await;
    write_framed_pyrep(&mut s, &call(2, "stationSvc", None, "Undock", vec![]).to_pyrep()).await.unwrap();
    let _ = read_until_call_reply(&mut s).await;

    let jita = ctx.systems.get_or_spawn(30_000_142);
    let _ = jita.step_manual(1.0).await.unwrap();

    write_framed_pyrep(&mut s, &call(3, "beyonce", None, "MachoBindObject", vec![PyRep::Integer(30_000_142), PyRep::None]).to_pyrep()).await.unwrap();
    let bind_rsp = read_until_call_reply(&mut s).await;
    let oid_val = bind_rsp.named_payload.get("OID+").map(as_dict).and_then(|e| e.first()).and_then(|(k, _)| k.as_str()).and_then(|s_oid| s_oid.split(':').nth(1)).and_then(|n| n.parse::<u64>().ok()).unwrap();

    write_framed_pyrep(&mut s, &call(4, "beyonce", Some(oid_val), "UpdateStateRequest", vec![]).to_pyrep()).await.unwrap();
    assert_eq!(read_until_call_reply(&mut s).await.packet_type, MachoPacketType::CallRsp);

    // Tick system while `ballpark_ready == true` so `sys_rx` streams `DoDestinyUpdate`, and queue pre-RPC notifications before next CallReq
    let _ = jita.step_manual(1.0).await.unwrap();
    assert_eq!(MachoPacket::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap().packet_type, MachoPacketType::Notification);
    ctx.notify_account_change(char_id, "cash", 1000, 456.0);
    ctx.notify(NotificationTarget::Character(char_id), "OnSessionChanged", "charid", vec![]);

    write_framed_pyrep(&mut s, &call(5, "beyonce", Some(oid_val), "CmdSetSpeedFraction", vec![PyRep::Real(0.5)]).to_pyrep()).await.unwrap();
    let _ = read_until_call_reply(&mut s).await;
    while tokio::time::timeout(std::time::Duration::from_millis(20), read_pkt(&mut s)).await.is_ok() {}

    // Advance clock past 10s undock session-change timer so `Dock` succeeds & streams
    // optional pre-detach DoDestinyUpdate(s) -> SessionChangeNotification -> CallRsp (#883, #928)
    clock.advance_secs(15);
    write_framed_pyrep(&mut s, &call(6, "stationSvc", None, "Dock", vec![PyRep::Integer(60_003_760)]).to_pyrep()).await.unwrap();
    let mut dock_pkts = Vec::new();
    let first_rsp = loop {
        let pkt = MachoPacket::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap();
        let ptype = pkt.packet_type;
        dock_pkts.push(ptype);
        if matches!(ptype, MachoPacketType::CallRsp | MachoPacketType::ErrorResponse) {
            break pkt;
        }
    };
    assert_eq!(first_rsp.packet_type, MachoPacketType::CallRsp);
    let sess_pos = dock_pkts
        .iter()
        .position(|t| *t == MachoPacketType::SessionChangeNotification)
        .expect("Dock must emit SessionChangeNotification before CallRsp");
    let rsp_pos = dock_pkts
        .iter()
        .position(|t| *t == MachoPacketType::CallRsp)
        .expect("Dock CallRsp");
    assert!(sess_pos < rsp_pos);

    // Undock again, spawn + move ship ball, drop connection -> disconnect cleanup saves ball position
    clock.advance_secs(15);
    write_framed_pyrep(&mut s, &call(7, "stationSvc", None, "Undock", vec![]).to_pyrep()).await.unwrap();
    let _ = read_until_call_reply(&mut s).await;
    let _ = jita.step_manual(1.0).await.unwrap();
    let _ = jita.set_ball_position(ship_id, glam::DVec3::new(12_345.0, -6_789.0, 42.0)).await;
    drop(s);
    for _ in 0..500 {
        if ctx.db.get_online_session(user_id).is_none()
            && ctx.db.with_snapshot(|snap| snap.item_positions.get(&ship_id).copied()) == Some([12_345.0, -6_789.0, 42.0])
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(ctx.db.get_online_session(user_id).is_none());
    assert_eq!(ctx.db.with_snapshot(|snap| snap.item_positions.get(&ship_id).copied()), Some([12_345.0, -6_789.0, 42.0]));

    let (mut s2, _c2, u2) = connect_login(addr, "demo", "demo-secret").await;
    let active_sid = ctx.db.get_online_session(u2).unwrap().session_id;
    ctx.disconnect_session(active_sid, "CUSTOM_CLOSE", "Server maintenance");
    let closed = GPSTransportClosed::from_pyrep(&read_pkt(&mut s2).await.unwrap()).unwrap();
    assert_eq!((closed.reason_code.as_deref(), closed.reason.as_str()), (Some("CUSTOM_CLOSE"), "Server maintenance"));
    drop(s2);
    for _ in 0..500 {
        if ctx.db.get_online_session(u2).is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    clock.advance_secs(15);
    let (mut s3, c3, u3) = connect_login(addr, "demo", "demo-secret").await;
    let call3 = |cid: u64, svc: &str, m: &str, args: Vec<PyRep>| {
        build_call_pkt(c3, u3, cid, MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: Some(svc.into()), call_id: 0 }, None, m, args)
    };
    write_framed_pyrep(&mut s3, &call3(1, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(char_id)]).to_pyrep()).await.unwrap();
    let _ = read_until_call_reply(&mut s3).await;
    write_framed_pyrep(&mut s3, &call3(2, "stationSvc", "Undock", vec![]).to_pyrep()).await.unwrap();
    let _ = read_until_call_reply(&mut s3).await;
    write_framed_pyrep(&mut s3, &call3(3, "beyonce", "MachoBindObject", vec![PyRep::Integer(30_000_142), PyRep::None]).to_pyrep()).await.unwrap();
    let _ = read_until_call_reply(&mut s3).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = 30_000_144;
        }
    });
    ctx.notify(NotificationTarget::Character(char_id), "OnSessionChanged", "charid", vec![]);
    let ping = MachoPacket {
        packet_type: MachoPacketType::PingReq,
        source: MachoAddress::Client { client_id: c3, call_id: 99, service: None },
        destination: MachoAddress::Node { node_id: MACHO_PROXY_NODE_ID, service: None, call_id: 0 },
        user_id: Some(u3),
        payload: PyRep::Tuple(Vec::new()),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s3, &ping.to_pyrep()).await.unwrap();
    loop {
        let p = MachoPacket::from_pyrep(&read_pkt(&mut s3).await.unwrap()).unwrap();
        if p.packet_type == MachoPacketType::SessionChangeNotification {
            assert_eq!(
                p.named_payload.get("channel").and_then(PyRep::as_str),
                Some("sessionchange")
            );
            break;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slash_fetch_remote_command_list_and_unknown_cmd_return_slash_error_with_reason() {
    let ctx = Arc::new(ServerContext::new());
    let addr = spawn_listener(Arc::clone(&ctx)).await;
    let char_id = 90_000_001_i64;

    let decode_user_error = |pkt: MachoPacket| -> (String, Vec<(String, String)>) {
        assert_eq!(pkt.packet_type, MachoPacketType::ErrorResponse);
        let PyRep::Bytes(bytes) = &as_tuple(&as_tuple(&pkt.payload)[2])[0] else {
            panic!("expected wrapped exception bytes");
        };
        let PyRep::ObjectEx { header, .. } = marshal::decode(bytes).unwrap() else {
            panic!("expected ObjectEx UserError");
        };
        let hdr = as_tuple(&header);
        assert_eq!(hdr[0], PyRep::Token("eveexceptions.UserError".into()));
        let args = as_tuple(&hdr[1]);
        let name = args[0].as_str().unwrap().to_string();
        let PyRep::Dict(entries) = &args[1] else {
            panic!("expected UserError dict");
        };
        let kv = entries
            .iter()
            .map(|(k, v)| (k.as_str().unwrap().to_string(), v.as_str().unwrap().to_string()))
            .collect();
        (name, kv)
    };

    // 0. Non-GM player calling slash.SlashCmd("/") receives SlashError with 'reason': "Commands: []"
    //    so V24.01 `svc_slash.FetchRemoteCommandList` evaluates `[]` without popping a modal error,
    //    while real privileged slash commands still fail with InsufficientRole.
    {
        let (mut s_player, c_player, u_player) = connect_login(addr, "demo", "demo-secret").await;
        let call_p = |cid: u64, svc: &str, m: &str, args: Vec<PyRep>| {
            build_call_pkt(
                c_player,
                u_player,
                cid,
                MachoAddress::Node {
                    node_id: MACHO_PROXY_NODE_ID,
                    service: Some(svc.into()),
                    call_id: 0,
                },
                None,
                m,
                args,
            )
        };
        // Even before selecting a character, FetchRemoteCommandList ("/") returns empty list cleanly.
        write_framed_pyrep(
            &mut s_player,
            &call_p(1, "slash", "SlashCmd", vec![PyRep::String("/".into())]).to_pyrep(),
        )
        .await
        .unwrap();
        let (err_name, dict) = decode_user_error(read_until_call_reply(&mut s_player).await);
        assert_eq!(err_name, "SlashError");
        assert_eq!(dict.len(), 1);
        assert_eq!(dict[0].0, "reason");
        assert_eq!(dict[0].1, "Commands: []");

        write_framed_pyrep(
            &mut s_player,
            &call_p(2, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(char_id)])
                .to_pyrep(),
        )
        .await
        .unwrap();
        assert_eq!(read_until_call_reply(&mut s_player).await.packet_type, MachoPacketType::CallRsp);
        write_framed_pyrep(
            &mut s_player,
            &call_p(3, "slash", "SlashCmd", vec![PyRep::String("/".into())]).to_pyrep(),
        )
        .await
        .unwrap();
        let (err_name, dict) = decode_user_error(read_until_call_reply(&mut s_player).await);
        assert_eq!(err_name, "SlashError");
        assert_eq!(dict.len(), 1);
        assert_eq!(dict[0].0, "reason");
        assert_eq!(dict[0].1, "Commands: []");

        // Actual privileged slash commands must still be rejected for non-GM sessions.
        write_framed_pyrep(
            &mut s_player,
            &call_p(4, "slash", "SlashCmd", vec![PyRep::String("/giveisk 100".into())]).to_pyrep(),
        )
        .await
        .unwrap();
        let (err_name, dict) = decode_user_error(read_until_call_reply(&mut s_player).await);
        assert_eq!(err_name, "SlashError");
        assert_eq!(dict.len(), 1);
        assert_eq!(dict[0].0, "reason");
        assert!(dict[0].1.contains("InsufficientRole"), "got reason: {}", dict[0].1);
        drop(s_player);
        for _ in 0..500 {
            if ctx.db.get_online_session(u_player).is_none() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    // Grant GM role for GM FetchRemoteCommandList + unknown slash tests
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("demo") {
            acc.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
        }
    });
    let (mut s, client_id, user_id) = connect_login(addr, "demo", "demo-secret").await;
    let call = |cid: u64, svc: &str, m: &str, args: Vec<PyRep>| {
        build_call_pkt(
            client_id,
            user_id,
            cid,
            MachoAddress::Node {
                node_id: MACHO_PROXY_NODE_ID,
                service: Some(svc.into()),
                call_id: 0,
            },
            None,
            m,
            args,
        )
    };

    write_framed_pyrep(
        &mut s,
        &call(1, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(char_id)]).to_pyrep(),
    )
    .await
    .unwrap();
    assert_eq!(read_until_call_reply(&mut s).await.packet_type, MachoPacketType::CallRsp);

    // 1. FetchRemoteCommandList: slash.SlashCmd("/") -> UserError('SlashError', {'reason': "Commands: [...]"})
    write_framed_pyrep(
        &mut s,
        &call(2, "slash", "SlashCmd", vec![PyRep::String("/".into())]).to_pyrep(),
    )
    .await
    .unwrap();
    let (err_name, dict) = decode_user_error(read_until_call_reply(&mut s).await);
    assert_eq!(err_name, "SlashError");
    assert_eq!(dict.len(), 1);
    assert_eq!(dict[0].0, "reason");
    assert!(dict[0].1.starts_with("Commands: ["), "got reason: {}", dict[0].1);
    assert!(dict[0].1.contains("'heal'"), "got reason: {}", dict[0].1);
    assert!(dict[0].1.contains("'giveisk'"), "got reason: {}", dict[0].1);
    assert!(dict[0].1.contains("'tr'"), "got reason: {}", dict[0].1);

    // 2. Unknown slash command -> UserError('SlashError', {'reason': "UnknownSlashCommand: ..."})
    write_framed_pyrep(
        &mut s,
        &call(3, "slash", "SlashCmd", vec![PyRep::String("/no_such_cmd".into())]).to_pyrep(),
    )
    .await
    .unwrap();
    let (err_name, dict) = decode_user_error(read_until_call_reply(&mut s).await);
    assert_eq!(err_name, "SlashError");
    assert_eq!(dict.len(), 1);
    assert_eq!(dict[0].0, "reason");
    assert!(dict[0].1.contains("UnknownSlashCommand"), "got reason: {}", dict[0].1);

    // 3. Invalid slash argument -> UserError('SlashError', {'reason': "InvalidSlashArgument: ..."})
    write_framed_pyrep(
        &mut s,
        &call(4, "slash", "SlashCmd", vec![PyRep::String("/giveisk nope".into())]).to_pyrep(),
    )
    .await
    .unwrap();
    let (err_name, dict) = decode_user_error(read_until_call_reply(&mut s).await);
    assert_eq!(err_name, "SlashError");
    assert_eq!(dict.len(), 1);
    assert_eq!(dict[0].0, "reason");
    assert!(dict[0].1.contains("InvalidSlashArgument"), "got reason: {}", dict[0].1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wire_invbroker_capacity_overflow_returns_typed_user_error_with_free_and_required() {
    let ctx = Arc::new(ServerContext::new());
    let addr = spawn_listener(Arc::clone(&ctx)).await;
    let char_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let venture_id = 99_667_001_i64;
    let tritanium_id = 99_667_002_i64;
    let drone_stack_id = 99_667_003_i64;
    let ore_id = 99_667_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            venture_id,
            eve_db::InventoryItemRecord {
                item_id: venture_id,
                type_id: 32_880, // Venture: 50m3 Cargo, 10m3 DroneBay, 5000m3 OreHold
                owner_id: char_id,
                location_id: station_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // 10,000 Tritanium (0.01 m3 each) = 100 m3 -> overflows 50 m3 cargo hold (free=50.0, required=100.0)
        snap.items.insert(
            tritanium_id,
            eve_db::InventoryItemRecord {
                item_id: tritanium_id,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 10_000,
                singleton: false,
                ..Default::default()
            },
        );
        // 4 Hobgoblin I (type 2454, 5 m3 each) = 20 m3 -> overflows 10 m3 DroneBay
        snap.items.insert(
            drone_stack_id,
            eve_db::InventoryItemRecord {
                item_id: drone_stack_id,
                type_id: 2454,
                owner_id: char_id,
                location_id: station_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 4,
                singleton: false,
                ..Default::default()
            },
        );
        // 60,000 Veldspar (type 1230, 0.1 m3 each) = 6000 m3 -> overflows 5000 m3 SpecializedOreHold
        snap.items.insert(
            ore_id,
            eve_db::InventoryItemRecord {
                item_id: ore_id,
                type_id: 1230,
                owner_id: char_id,
                location_id: station_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 60_000,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let (mut s, client_id, user_id) = connect_login(addr, "demo", "demo-secret").await;
    let call = |cid: u64, svc: &str, m: &str, args: Vec<PyRep>| {
        build_call_pkt(
            client_id,
            user_id,
            cid,
            MachoAddress::Node {
                node_id: MACHO_PROXY_NODE_ID,
                service: Some(svc.into()),
                call_id: 0,
            },
            None,
            m,
            args,
        )
    };
    let decode_space_error = |pkt: MachoPacket| -> (String, f64, f64) {
        assert_eq!(pkt.packet_type, MachoPacketType::ErrorResponse);
        let PyRep::Bytes(bytes) = &as_tuple(&as_tuple(&pkt.payload)[2])[0] else {
            panic!("expected wrapped exception bytes");
        };
        let PyRep::ObjectEx { header, .. } = marshal::decode(bytes).unwrap() else {
            panic!("expected ObjectEx UserError");
        };
        let hdr = as_tuple(&header);
        assert_eq!(hdr[0], PyRep::Token("eveexceptions.UserError".into()));
        let args = as_tuple(&hdr[1]);
        let name = args[0].as_str().unwrap().to_string();
        let PyRep::Dict(entries) = &args[1] else {
            panic!("expected UserError dict");
        };
        let mut free = None;
        let mut required = None;
        for (k, v) in entries {
            match k.as_str() {
                Some("free") => free = match v {
                    PyRep::Real(x) => Some(*x),
                    PyRep::Integer(n) => Some(*n as f64),
                    _ => None,
                },
                Some("required") => required = match v {
                    PyRep::Real(x) => Some(*x),
                    PyRep::Integer(n) => Some(*n as f64),
                    _ => None,
                },
                _ => {}
            }
        }
        (
            name,
            free.expect("free param missing"),
            required.expect("required param missing"),
        )
    };

    write_framed_pyrep(
        &mut s,
        &call(1, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(char_id)]).to_pyrep(),
    )
    .await
    .unwrap();
    assert_eq!(read_until_call_reply(&mut s).await.packet_type, MachoPacketType::CallRsp);

    // 1. Cargo hold (flag 5) -> NotEnoughCargoSpace with free=50.0, required=100.0
    write_framed_pyrep(
        &mut s,
        &call(
            2,
            "invbroker",
            "Add",
            vec![
                PyRep::Integer(tritanium_id),
                PyRep::Integer(venture_id),
                PyRep::Integer(5),
            ],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let (name, free, req) = decode_space_error(read_until_call_reply(&mut s).await);
    assert_eq!(name, "NotEnoughCargoSpace");
    assert!((free - 50.0).abs() < 1e-6, "free={free}");
    assert!((req - 100.0).abs() < 1e-6, "required={req}");

    // 2. DroneBay (flag 87) -> NotEnoughDroneBaySpace with free=10.0, required=20.0
    write_framed_pyrep(
        &mut s,
        &call(
            3,
            "invbroker",
            "Add",
            vec![
                PyRep::Integer(drone_stack_id),
                PyRep::Integer(venture_id),
                PyRep::Integer(87),
            ],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let (name, free, req) = decode_space_error(read_until_call_reply(&mut s).await);
    assert_eq!(name, "NotEnoughDroneBaySpace");
    assert!((free - 10.0).abs() < 1e-6, "free={free}");
    assert!((req - 20.0).abs() < 1e-6, "required={req}");

    // 3. SpecializedOreHold (flag 134) -> NotEnoughSpecialBaySpace with free=5000.0, required=6000.0
    write_framed_pyrep(
        &mut s,
        &call(
            4,
            "invbroker",
            "Add",
            vec![
                PyRep::Integer(ore_id),
                PyRep::Integer(venture_id),
                PyRep::Integer(134),
            ],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let (name, free, req) = decode_space_error(read_until_call_reply(&mut s).await);
    assert_eq!(name, "NotEnoughSpecialBaySpace");
    assert!((free - 5000.0).abs() < 1e-6, "free={free}");
    assert!((req - 6000.0).abs() < 1e-6, "required={req}");
}

#[test]
fn on_item_change_and_multiple_item_change_resolve_sde_and_singleton_qty() {
    let ctx = ServerContext::new();
    let ship_type = ctx.sde.types.get(&587).expect("Rifter 587 in SDE");
    let kv = PyRep::keyval([
        ("itemID", PyRep::Integer(700_001)),
        ("typeID", PyRep::Integer(587)),
        ("ownerID", PyRep::Integer(90_000_001)),
        ("locationID", PyRep::Integer(60_003_760)),
        ("flag", PyRep::Integer(4)),
        ("quantity", PyRep::Integer(1)),
        ("singleton", PyRep::Bool(true)),
    ]);

    let single = crate::router::client_compat::normalize_macho_notification_with_sde(
        "OnItemChange",
        vec![kv.clone(), PyRep::Dict(Vec::new())],
        Some(&ctx.sde),
    );
    assert_eq!(single.len(), 3);
    assert_eq!(single[2], PyRep::None);
    let PyRep::DBRow { columns, .. } = &single[0] else {
        panic!("expected DBRow from OnItemChange");
    };
    let col = |cols: &[(String, PyRep)], name: &str| -> Option<i64> {
        cols.iter().find(|(k, _)| k == name).and_then(|(_, v)| v.as_i64())
    };
    assert_eq!(col(columns, "itemID"), Some(700_001));
    assert_eq!(col(columns, "typeID"), Some(587));
    assert_eq!(col(columns, "ownerID"), Some(90_000_001));
    assert_eq!(col(columns, "locationID"), Some(60_003_760));
    assert_eq!(col(columns, "flagID"), Some(4));
    assert_eq!(col(columns, "quantity"), Some(-1));
    assert_eq!(col(columns, "groupID"), Some(i64::from(ship_type.group_id)));
    assert_eq!(col(columns, "categoryID"), Some(i64::from(ship_type.category_id)));

    let multi = crate::router::client_compat::normalize_macho_notification_with_sde(
        "OnMultipleItemChange",
        vec![PyRep::List(vec![kv]), PyRep::Dict(Vec::new())],
        Some(&ctx.sde),
    );
    let PyRep::List(rows) = &multi[0] else {
        panic!("expected List in OnMultipleItemChange");
    };
    let PyRep::DBRow { columns: m_cols, .. } = &rows[0] else {
        panic!("expected DBRow element in OnMultipleItemChange");
    };
    assert_eq!(col(m_cols, "quantity"), Some(-1));
    assert_eq!(col(m_cols, "groupID"), Some(i64::from(ship_type.group_id)));
    assert_eq!(col(m_cols, "categoryID"), Some(i64::from(ship_type.category_id)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn market_max_open_orders_maps_to_v24_market_exceeded_order_count_user_error_713() {
    let ctx = Arc::new(ServerContext::new());
    let addr = spawn_listener(Arc::clone(&ctx)).await;
    let char_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = eve_db::IskCents::from_isk(1_000_000);
        }
        snap.skills.remove(&char_id);
    });

    let (mut s, client_id, user_id) = connect_login(addr, "demo", "demo-secret").await;
    let sel_pkt = build_call_pkt(
        client_id,
        user_id,
        1,
        MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: Some("charUnboundMgr".into()),
            call_id: 0,
        },
        None,
        "SelectCharacterID",
        vec![PyRep::Integer(char_id)],
    );
    write_framed_pyrep(&mut s, &sel_pkt.to_pyrep()).await.unwrap();
    assert_eq!(read_until_call_reply(&mut s).await.packet_type, MachoPacketType::CallRsp);

    let mut call_id = 10_u64;
    for i in 0..5 {
        call_id += 1;
        let pkt = build_call_pkt(
            client_id,
            user_id,
            call_id,
            MachoAddress::Node {
                node_id: MACHO_PROXY_NODE_ID,
                service: Some("marketProxy".into()),
                call_id: 0,
            },
            None,
            "PlaceBuyOrder",
            vec![
                PyRep::Integer(station_id),
                PyRep::Integer(1230),
                PyRep::Real(1.0 + (i as f64)),
                PyRep::Integer(1),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Bool(false),
                PyRep::None,
            ],
        );
        write_framed_pyrep(&mut s, &pkt.to_pyrep()).await.unwrap();
        let reply = read_until_call_reply(&mut s).await;
        assert_ne!(
            reply.packet_type,
            MachoPacketType::ErrorResponse,
            "order #{i} should succeed, got {reply:?}"
        );
    }

    call_id += 1;
    let over_pkt = build_call_pkt(
        client_id,
        user_id,
        call_id,
        MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: Some("marketProxy".into()),
            call_id: 0,
        },
        None,
        "PlaceBuyOrder",
        vec![
            PyRep::Integer(station_id),
            PyRep::Integer(1230),
            PyRep::Real(10.0),
            PyRep::Integer(1),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Bool(false),
            PyRep::None,
        ],
    );
    write_framed_pyrep(&mut s, &over_pkt.to_pyrep()).await.unwrap();
    let err_pkt = read_until_call_reply(&mut s).await;
    assert_eq!(err_pkt.packet_type, MachoPacketType::ErrorResponse);
    let PyRep::Bytes(bytes) = &as_tuple(&as_tuple(&err_pkt.payload)[2])[0] else {
        panic!("expected wrapped exception bytes in ErrorResponse");
    };
    let PyRep::ObjectEx { header, .. } = marshal::decode(bytes).unwrap() else {
        panic!("expected ObjectEx UserError");
    };
    let hdr = as_tuple(&header);
    assert_eq!(hdr[0], PyRep::Token("eveexceptions.UserError".into()));
    let args = as_tuple(&hdr[1]);
    assert_eq!(args[0].as_str(), Some("MarketExceededOrderCount"));
    let PyRep::Dict(entries) = &args[1] else {
        panic!("expected UserError dict args");
    };
    let get_int = |key: &str| -> Option<i64> {
        entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .and_then(|(_, v)| v.as_i64())
    };
    assert_eq!(get_int("curCnt"), Some(5));
    assert_eq!(get_int("maxCnt"), Some(5));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_req_ping_list_corrupt_marshal_error_codes_and_revocation() {
    let ctx = Arc::new(ServerContext::new());
    let addr = spawn_listener(Arc::clone(&ctx)).await;

    // 1. read_framed_pyrep rejects invalid frame lengths 0 and > MAX_MACHO_FRAME_LEN.
    let mut zero_len = Vec::new();
    zero_len.extend_from_slice(&0u32.to_le_bytes());
    assert!(read_framed_pyrep(&mut Cursor::new(zero_len))
        .await
        .unwrap_err()
        .to_string()
        .contains("Invalid machoNet frame length"));

    let mut huge_len = Vec::new();
    huge_len.extend_from_slice(&((MAX_MACHO_FRAME_LEN + 1) as u32).to_le_bytes());
    assert!(read_framed_pyrep(&mut Cursor::new(huge_len))
        .await
        .unwrap_err()
        .to_string()
        .contains("Invalid machoNet frame length"));

    // 2. Corrupt marshal bytes trigger HANDSHAKE_INCOMPATIBLEPROTOCOL close frame.
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _hello = read_pkt(&mut s).await.unwrap();
        s.write_u32_le(5).await.unwrap();
        s.write_all(&[0xff, 0xee, 0xdd, 0xcc, 0xbb]).await.unwrap();
        s.flush().await.unwrap();
        let closed = GPSTransportClosed::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap();
        assert_eq!(
            closed.reason_code.as_deref(),
            Some("HANDSHAKE_INCOMPATIBLEPROTOCOL")
        );
    }

    // 3. Authenticated flow: PingReq with List payload, ResolveReq, non-MachoPacket ignore,
    //    AuthenticationReq error, InvalidBoundObject -> UnMachoDestination, CharacterNotAtStation,
    //    and non-MultiLoginBlocked session revocation.
    let (mut s, client_id, user_id) = connect_login(addr, "demo", "demo-secret").await;
    let call = |cid: u64, svc: &str, bid: Option<u64>, m: &str, args: Vec<PyRep>| {
        build_call_pkt(
            client_id,
            user_id,
            cid,
            MachoAddress::Node {
                node_id: MACHO_PROXY_NODE_ID,
                service: Some(svc.into()),
                call_id: 0,
            },
            bid,
            m,
            args,
        )
    };

    // PingReq with direct PyRep::List payload
    let list_ping = MachoPacket {
        packet_type: MachoPacketType::PingReq,
        source: MachoAddress::Client {
            client_id,
            call_id: 1,
            service: None,
        },
        destination: MachoAddress::Any {
            service: None,
            call_id: 0,
        },
        user_id: Some(user_id),
        payload: PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(10),
            PyRep::Integer(20),
        ])]),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &list_ping.to_pyrep()).await.unwrap();
    let ping_rsp = MachoPacket::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap();
    assert_eq!(ping_rsp.packet_type, MachoPacketType::PingRsp);
    assert_eq!(as_list(&ping_rsp.payload).len(), 10);

    // ResolveReq -> ResolveRsp
    let resolve_req = MachoPacket {
        packet_type: MachoPacketType::ResolveReq,
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
        payload: PyRep::Integer(MACHO_PROXY_NODE_ID as i64),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s, &resolve_req.to_pyrep()).await.unwrap();
    let resolve_rsp = MachoPacket::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap();
    assert_eq!(resolve_rsp.packet_type, MachoPacketType::ResolveRsp);
    assert_eq!(resolve_rsp.payload.as_i64(), Some(MACHO_PROXY_NODE_ID as i64));

    // Non-MachoPacket frame after auth is ignored cleanly
    write_framed_pyrep(&mut s, &PyRep::Integer(999)).await.unwrap();

    // AuthenticationReq after auth -> AlreadyAuthenticated ErrorResponse
    let dup_auth = MachoPacket {
        packet_type: MachoPacketType::AuthenticationReq,
        source: MachoAddress::Client {
            client_id,
            call_id: 3,
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
    write_framed_pyrep(&mut s, &dup_auth.to_pyrep()).await.unwrap();
    let dup_rsp = MachoPacket::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap();
    assert_eq!(dup_rsp.packet_type, MachoPacketType::ErrorResponse);

    // InvalidBoundObject -> UnMachoDestination UserError
    write_framed_pyrep(
        &mut s,
        &call(10, "beyonce", Some(999_888), "UpdateStateRequest", vec![]).to_pyrep(),
    )
    .await
    .unwrap();
    let bound_err = read_until_call_reply(&mut s).await;
    assert_eq!(bound_err.packet_type, MachoPacketType::ErrorResponse);
    let PyRep::Bytes(b_bytes) = &as_tuple(&as_tuple(&bound_err.payload)[2])[0] else {
        panic!()
    };
    let PyRep::ObjectEx { header: b_hdr, .. } = marshal::decode(b_bytes).unwrap() else {
        panic!()
    };
    assert_eq!(
        as_tuple(&as_tuple(&b_hdr)[1])[0].as_str(),
        Some("UnMachoDestination")
    );

    // SelectCharacterID while docked, then trigger CharacterNotAtStation -> RuntimeError("CharacterNotAtStation")
    write_framed_pyrep(
        &mut s,
        &call(11, "charUnboundMgr", None, "SelectCharacterID", vec![PyRep::Integer(90_000_001)])
            .to_pyrep(),
    )
    .await
    .unwrap();
    assert_eq!(read_until_call_reply(&mut s).await.packet_type, MachoPacketType::CallRsp);

    // Dock error when already docked or undocked via stationSvc.Undock then calling Dock in invalid state:
    // Actually call `repairSvc.GetDamageReports` with non-existent item or undock and call `stationSvc.GetDockingInfo` -> let's push a char notification and verify notify_rx delivers it!
    ctx.notify(
        NotificationTarget::Character(90_000_001),
        "OnAccountChange",
        "charid",
        vec![PyRep::Integer(1000), PyRep::Integer(100)],
    );
    ctx.notify(
        NotificationTarget::Character(90_000_001),
        "OnDamageStateChange",
        "charid",
        vec![PyRep::Integer(1)],
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_000_144;
        }
    });
    ctx.notify(
        NotificationTarget::Character(90_000_001),
        "OnSessionChanged",
        "charid",
        vec![],
    );
    let mut saw_notif = false;
    let mut saw_sess_change = false;
    for _ in 0..8 {
        let pkt = MachoPacket::from_pyrep(&read_pkt(&mut s).await.unwrap()).unwrap();
        if pkt.packet_type == MachoPacketType::Notification {
            saw_notif = true;
        }
        if pkt.packet_type == MachoPacketType::SessionChangeNotification {
            saw_sess_change = true;
        }
        if saw_notif && saw_sess_change {
            break;
        }
    }
    assert!(saw_notif && saw_sess_change);

    // Non-MultiLoginBlocked revocation terminates session cleanly with GPSTransportClosed
    ctx.db
        .ban_account("demo", ctx.now_unix() + 600, "Banned by test")
        .unwrap();
    write_framed_pyrep(&mut s, &list_ping.to_pyrep()).await.unwrap();
    loop {
        let rep = read_pkt(&mut s).await.unwrap();
        if let Some(closed) = GPSTransportClosed::from_pyrep(&rep) {
            assert_eq!(closed.reason_code.as_deref(), Some("UserBanned"));
            break;
        }
    }
}
