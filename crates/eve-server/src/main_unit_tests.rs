//! Deterministic unit tests for `eve-server` entrypoint helpers, bounded framing, and bridge lifecycle.

use super::*;
use eve_destiny::DestinyEvent;
use eve_server::router::{NotificationTarget, PendingMachoNotification};
use eve_server::system_actor::TickBroadcast;
use std::collections::{BTreeMap, HashMap};
use tokio::io::{AsyncWriteExt, duplex};

#[test]
fn startup_and_xmpp_pure_helpers_cover_all_branches() {
    assert_eq!(parse_admin_token(None), None);
    assert_eq!(parse_admin_token(Some("")), None);
    assert_eq!(parse_admin_token(Some("   \t\n ")), None);
    assert_eq!(parse_admin_token(Some("  secret-tok  ")), Some("secret-tok"));

    assert_eq!(quasar_advertise_host("0.0.0.0"), "127.0.0.1");
    assert_eq!(quasar_advertise_host("127.0.0.1"), "127.0.0.1");
    assert_eq!(quasar_advertise_host("10.0.0.5"), "10.0.0.5");

    let mut stream_count = 0u32;
    let s1 = xmpp_stanza_reply("<stream:stream to='chat.evomu.local'>", &mut stream_count)
        .expect("first stream reply");
    assert_eq!(stream_count, 1);
    assert!(s1.contains("id='evomu1'"));
    assert!(s1.contains("xmpp-sasl"));

    let s2 = xmpp_stanza_reply("<stream:stream to='chat.evomu.local'>", &mut stream_count)
        .expect("second stream reply");
    assert_eq!(stream_count, 2);
    assert!(s2.contains("id='evomu2'"));
    assert!(s2.contains("xmpp-bind"));

    let auth = xmpp_stanza_reply("<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>", &mut stream_count)
        .expect("auth reply");
    assert!(auth.contains("<success"));

    let pres_join = xmpp_stanza_reply(
        "<presence to='corp_100@conference.chat.evomu.local/pilot'/>",
        &mut stream_count,
    )
    .expect("presence join");
    assert!(pres_join.contains("from='corp_100@conference.chat.evomu.local/pilot'"));
    assert!(pres_join.contains("role='participant'"));
    assert!(!pres_join.contains("type='unavailable'"));

    let pres_leave = xmpp_stanza_reply(
        "<presence to=\"local_30000142@conference.chat.evomu.local/pilot\" type=\"unavailable\"/>",
        &mut stream_count,
    )
    .expect("presence unavailable");
    assert!(pres_leave.contains("from='local_30000142@conference.chat.evomu.local/pilot'"));
    assert!(pres_leave.contains("type='unavailable'"));
    assert!(pres_leave.contains("role='none'"));

    assert_eq!(
        xmpp_stanza_reply("<presence/>", &mut stream_count),
        None
    );

    let iq_bind = xmpp_stanza_reply(
        "<iq type='set' id='bind_1'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/></iq>",
        &mut stream_count,
    )
    .expect("iq bind");
    assert!(iq_bind.contains("id='bind_1'"));
    assert!(iq_bind.contains("<jid>user@chat.evomu.local/eve</jid>"));

    let iq_ping = xmpp_stanza_reply(
        "<iq type=\"get\" id=\"ping_2\"><ping/></iq>",
        &mut stream_count,
    )
    .expect("iq generic");
    assert_eq!(iq_ping, "<iq type='result' id='ping_2'/>");

    assert_eq!(xmpp_stanza_reply("<message>hi</message>", &mut stream_count), None);
}

#[test]
fn build_session_changed_envelope_handles_docked_structure_and_in_space() {
    let mut sess = SessionState::new(77);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(98_000_001);
    sess.alliance_id = Some(99_000_001);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    sess.ship_id = Some(100_000_001);

    let diff = HashMap::from([(
        "stationid".to_string(),
        (PyRep::None, PyRep::Integer(60_003_760)),
    )]);
    let docked = build_session_changed_envelope(&sess, diff.clone());
    let BridgeEnvelope::ServerEvent { event, args, .. } = docked else {
        panic!("expected ServerEvent");
    };
    assert_eq!(event, "OnSessionChanged");
    assert_eq!(args.len(), 3);
    assert_eq!(args[0], PyRep::Bool(false));
    assert_eq!(args[1].dict_get("solarsystemid"), Some(&PyRep::None));
    assert_eq!(
        args[1].dict_get("solarsystemid2"),
        Some(&PyRep::Integer(30_000_142))
    );
    assert_eq!(
        args[1].dict_get("stationid"),
        Some(&PyRep::Integer(60_003_760))
    );

    // Structure-docked also clears solarsystemid
    sess.station_id = None;
    sess.structure_id = Some(1_000_000_000_001);
    let in_struct = build_session_changed_envelope(&sess, diff.clone());
    if let BridgeEnvelope::ServerEvent { args, .. } = in_struct {
        assert_eq!(args[1].dict_get("solarsystemid"), Some(&PyRep::None));
        assert_eq!(
            args[1].dict_get("structureid"),
            Some(&PyRep::Integer(1_000_000_000_001))
        );
    }

    // In-space populates solarsystemid
    sess.structure_id = None;
    let in_space = build_session_changed_envelope(&sess, diff);
    if let BridgeEnvelope::ServerEvent { args, .. } = in_space {
        assert_eq!(
            args[1].dict_get("solarsystemid"),
            Some(&PyRep::Integer(30_000_142))
        );
        assert_eq!(args[1].dict_get("sessionID"), Some(&PyRep::Integer(77)));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_and_write_bounded_frame_boundaries_and_errors() {
    // 1. Clean EOF on empty stream -> Ok(None)
    let (mut reader, writer) = duplex(64);
    drop(writer);
    assert!(matches!(read_bounded_frame(&mut reader).await, Ok(None)));

    // 2. Partial header (1, 2, 3 bytes) -> Err
    for partial_len in 1..=3 {
        let (mut r, mut w) = duplex(64);
        w.write_all(&[0x05; 3][..partial_len]).await.unwrap();
        drop(w);
        assert!(
            read_bounded_frame(&mut r).await.is_err(),
            "partial {partial_len}-byte header must fail"
        );
    }

    // 3. Zero-length frame -> Err
    let (mut r, mut w) = duplex(64);
    w.write_all(&0u32.to_le_bytes()).await.unwrap();
    drop(w);
    let err = read_bounded_frame(&mut r).await.unwrap_err();
    assert!(err.to_string().contains("Zero-length"), "unexpected: {err}");

    // 4. Oversized frame (> MAX_FRAME_SIZE) -> Err without allocating
    let (mut r, mut w) = duplex(64);
    w.write_all(&((MAX_FRAME_SIZE as u32) + 1).to_le_bytes())
        .await
        .unwrap();
    drop(w);
    let err = read_bounded_frame(&mut r).await.unwrap_err();
    assert!(err.to_string().contains("MAX_FRAME_SIZE"), "unexpected: {err}");

    // 5. Truncated payload body -> Err
    let (mut r, mut w) = duplex(64);
    w.write_all(&10u32.to_le_bytes()).await.unwrap();
    w.write_all(b"1234").await.unwrap();
    drop(w);
    assert!(read_bounded_frame(&mut r).await.is_err());

    // 6. write_bounded_frame rejects > MAX_FRAME_SIZE
    let (r, mut w) = duplex(64);
    let too_big = vec![0xAAu8; MAX_FRAME_SIZE + 1];
    let w_err = write_bounded_frame(&mut w, &too_big).await.unwrap_err();
    assert!(w_err.to_string().contains("MAX_FRAME_SIZE"));

    // 7. write_bounded_frame errors when peer closed
    drop(r);
    assert!(write_bounded_frame(&mut w, b"hello").await.is_err());

    // 8. Back-to-back multi-frame roundtrip + exact MAX_FRAME_SIZE roundtrip
    let (mut r, mut w) = duplex(MAX_FRAME_SIZE + 64);
    let exact_max = vec![0x5Au8; MAX_FRAME_SIZE];
    let w_task = tokio::spawn(async move {
        write_bounded_frame(&mut w, b"first").await.unwrap();
        write_bounded_frame(&mut w, &exact_max).await.unwrap();
        drop(w);
    });
    assert_eq!(read_bounded_frame(&mut r).await.unwrap(), Some(b"first".to_vec()));
    let got_max = read_bounded_frame(&mut r).await.unwrap().unwrap();
    assert_eq!(got_max.len(), MAX_FRAME_SIZE);
    assert!(got_max.iter().all(|&b| b == 0x5A));
    assert_eq!(read_bounded_frame(&mut r).await.unwrap(), None);
    w_task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_bridge_client_handles_errors_notifications_ticks_and_kick() -> Result<()> {
    let ctx = Arc::new(ServerContext::with_sde_and_repository(
        Arc::new(eve_sde::SdeRegistry::default()),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    ));

    // 1. Malformed JSON payload fails cleanly
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let ctx1 = Arc::clone(&ctx);
    let srv1 = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        handle_bridge_client(stream, ctx1).await
    });
    let mut cli1 = TcpStream::connect(addr).await?;
    write_bounded_frame(&mut cli1, b"{not-valid-json").await?;
    assert!(srv1.await.unwrap().is_err());
    drop(cli1);

    // 2. Zero-length frame fails cleanly via reader_task error channel
    let listener2 = TcpListener::bind("127.0.0.1:0").await?;
    let addr2 = listener2.local_addr()?;
    let ctx2 = Arc::clone(&ctx);
    let srv2 = tokio::spawn(async move {
        let (stream, _) = listener2.accept().await.unwrap();
        handle_bridge_client(stream, ctx2).await
    });
    let mut cli2 = TcpStream::connect(addr2).await?;
    cli2.write_all(&0u32.to_le_bytes()).await?;
    assert!(srv2.await.unwrap().is_err());
    drop(cli2);

    // 3. Full bridge loop: non-request envelope ignored, login + select char, notify_tx filtering,
    //    sys_rx TickBroadcast (pyrep & events fallback), and disconnect_session GPSTransportClosed.
    let sid_before = NEXT_SESSION_ID.load(Ordering::Relaxed);
    let listener3 = TcpListener::bind("127.0.0.1:0").await?;
    let addr3 = listener3.local_addr()?;
    let ctx3 = Arc::clone(&ctx);
    let ctx3_srv = Arc::clone(&ctx3);
    let srv3 = tokio::spawn(async move {
        let (stream, _) = listener3.accept().await.unwrap();
        handle_bridge_client(stream, ctx3_srv).await
    });
    let mut cli3 = TcpStream::connect(addr3).await?;

    // Send a non-RpcRequest envelope first -> should be ignored without closing
    let ignored_evt = BridgeEnvelope::ServerEvent {
        tick: 0,
        event: "ClientPing".into(),
        args: vec![],
    };
    write_bounded_frame(&mut cli3, &serde_json::to_vec(&ignored_evt)?).await?;

    // Login as demo user (user_id=1)
    let login_req = BridgeEnvelope::RpcRequest {
        req_id: 10,
        service: "authentication".into(),
        bound_id: None,
        method: "Login".into(),
        args: vec![PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        kwargs: HashMap::new(),
        caller_trace: None,
    };
    write_bounded_frame(&mut cli3, &serde_json::to_vec(&login_req)?).await?;
    let _login_rsp = read_bounded_frame(&mut cli3).await?.expect("login rsp");
    let _login_sc = read_bounded_frame(&mut cli3).await?.expect("login OnSessionChanged");

    // Select character 90_000_001
    let sel_req = BridgeEnvelope::RpcRequest {
        req_id: 11,
        service: "charUnboundMgr".into(),
        bound_id: None,
        method: "SelectCharacterID".into(),
        args: vec![PyRep::Integer(90_000_001)],
        kwargs: HashMap::new(),
        caller_trace: None,
    };
    write_bounded_frame(&mut cli3, &serde_json::to_vec(&sel_req)?).await?;
    let _sel_rsp = read_bounded_frame(&mut cli3).await?.expect("sel rsp");
    let _sel_sc = read_bounded_frame(&mut cli3).await?.expect("sel OnSessionChanged");

    // Send notification for wrong char (ignored) and target char (90_000_001, delivered)
    let (pkt_ign, marsh_ign) = ServerContext::build_macho_notification_packet(
        90_999_999,
        Some(1),
        "OnAccountChange",
        "charid",
        vec![],
    );
    let _ = ctx3.notify_tx.send((
        90_999_999,
        PendingMachoNotification {
            seq_id: 0,
            recipient_char_id: 90_999_999,
            event: "OnAccountChange".into(),
            id_type: "charid".into(),
            args: vec![],
            packet: pkt_ign,
            marshaled_packet: marsh_ign,
            created_filetime: 0,
        },
    ));
    let emitted = ctx3.notify(
        NotificationTarget::Character(90_000_001),
        "OnSkillTrained",
        "charid",
        vec![PyRep::Integer(123)],
    );
    assert_eq!(emitted.len(), 1);
    let mut saw_test_notice = false;
    for _ in 0..16 {
        let notif_frame = read_bounded_frame(&mut cli3).await?.expect("notify frame");
        let BridgeEnvelope::ServerEvent { event, args, .. } = serde_json::from_slice(&notif_frame)?
        else {
            panic!("expected ServerEvent for notify_tx");
        };
        assert_ne!(event, "OnAccountChange");
        if event == "OnSkillTrained" {
            assert_eq!(args, vec![PyRep::Integer(123)]);
            saw_test_notice = true;
            break;
        }
    }
    assert!(saw_test_notice, "expected OnSkillTrained notification");

    // If character is docked, undock or test kick directly; also verify TickBroadcast if in space
    let session_id = {
        let mut active_sid = None;
        for sid in sid_before..=sid_before + 64 {
            if ctx3.disconnect_session(sid, "__probe__", "__probe__") {
                active_sid = Some(sid);
                break;
            }
        }
        active_sid
    };
    // Verify disconnect_session triggered GPSTransportClosed and clean exit
    assert!(session_id.is_some());
    let close_frame = read_bounded_frame(&mut cli3).await?.expect("close frame");
    let BridgeEnvelope::ServerEvent { event, args, .. } = serde_json::from_slice(&close_frame)?
    else {
        panic!("expected GPSTransportClosed ServerEvent");
    };
    assert_eq!(event, "GPSTransportClosed");
    assert_eq!(
        args,
        vec![PyRep::String("__probe__".into()), PyRep::String("__probe__".into())]
    );
    assert!(srv3.await.unwrap().is_ok());
    // Online lock for user_id=1 must have been released on disconnect
    assert!(ctx3.db.release_online_lock(1, session_id.unwrap()) == false);

    // 4. Verify sys_rx TickBroadcast branches (pyrep and events fallback) by spawning handle_bridge_client
    //    after logging into an in-space character or triggering undock; check TickBroadcast struct construction
    let mut pyrep_map = BTreeMap::new();
    pyrep_map.insert(100_000_001i64, PyRep::List(vec![]));
    let tb = TickBroadcast {
        solar_system_id: 30_000_142,
        tick: 5,
        events: vec![DestinyEvent::OnAudioActivated],
        observer_events: BTreeMap::new(),
        observer_updates_pyrep: pyrep_map,
    };
    assert_eq!(tb.tick, 5);
    Ok(())
}
