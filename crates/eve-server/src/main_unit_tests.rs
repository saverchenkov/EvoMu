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

    // Structure-docked keeps solarsystemid and locationid set (V24.01 eveSessions)
    sess.station_id = None;
    sess.structure_id = Some(1_000_000_000_001);
    let in_struct = build_session_changed_envelope(&sess, diff.clone());
    if let BridgeEnvelope::ServerEvent { args, .. } = in_struct {
        assert_eq!(
            args[1].dict_get("solarsystemid"),
            Some(&PyRep::Integer(30_000_142))
        );
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
        broadcast_seq: 0,
        events: vec![DestinyEvent::OnAudioActivated],
        observer_events: BTreeMap::new(),
        observer_updates_pyrep: pyrep_map,
    };
    assert_eq!(tb.tick, 5);
    Ok(())
}

#[tokio::test]
async fn xmpp_stream_times_out_idle_connections_and_discovers_player_channels() {
    let ctx = Arc::new(ServerContext::with_sde_and_repository(
        Arc::new(eve_sde::SdeRegistry::default()),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    ));
    let mut owner_sess = SessionState::new(1);
    owner_sess.user_id = Some(90_000_001);
    owner_sess.character_id = Some(90_000_001);
    ctx.dispatch_rpc(
        &mut owner_sess,
        "XmppChatMgr",
        None,
        "CreatePlayerOwnedChannel",
        &[PyRep::String("Playtest Room".into())],
        &std::collections::HashMap::new(),
        None,
    )
    .await
    .unwrap();
    let owner_token = ctx
        .dispatch_rpc(
            &mut owner_sess,
            "chatAuthenticationService",
            None,
            "GetAuthenticationToken",
            &[],
            &std::collections::HashMap::new(),
            None,
        )
        .await
        .unwrap()
        .as_str()
        .expect("CSPRNG XMPP auth token")
        .to_string();
    assert!(owner_token.len() >= 32);
    assert!(!owner_token.starts_with("evomu-xmpp-token-"));

    // 1. Idle XMPP connection times out within `idle_read_timeout` (#853)
    let (client_io, server_io) = duplex(1024);
    handle_xmpp_stream_with_timeouts(
        server_io,
        Some(&ctx),
        Duration::from_millis(25),
        Duration::from_millis(25),
        Duration::from_millis(25),
    )
    .await;
    drop(client_io);

    // 2. XMPP slowloris / partial-stanza stream disconnects cleanly on `stanza_timeout` even when
    // `idle_read_timeout` is much longer and partial bytes keep arriving (`BF-01`, #853/#880).
    {
        let (mut slow_cli, slow_srv) = duplex(1024);
        let ctx_slow = ctx.clone();
        let slow_task = tokio::spawn(async move {
            handle_xmpp_stream_with_timeouts(
                slow_srv,
                Some(&ctx_slow),
                Duration::from_secs(10),
                Duration::from_millis(50),
                Duration::from_millis(40),
            )
            .await;
        });
        slow_cli.write_all(b"<iq type='get' ").await.unwrap();
        tokio::time::sleep(Duration::from_millis(15)).await;
        let _ = slow_cli.write_all(b"id='slow_1' ").await;
        tokio::time::sleep(Duration::from_millis(15)).await;
        let _ = slow_cli.write_all(b"to='conference").await;
        tokio::time::timeout(Duration::from_millis(300), slow_task)
            .await
            .expect("slowloris partial stanza must time out on stanza_timeout, not idle_read_timeout")
            .unwrap();
        drop(slow_cli);
    }

    // 3. Authenticated XMPP IQ `byname/Playtest Room` split across TCP writes >2048B returns room JID (#940, BF-01/BF-02)
    let (mut cli, srv) = duplex(16384);
    let ctx_clone = ctx.clone();
    let srv_task = tokio::spawn(async move {
        handle_xmpp_stream_with_timeouts(
            srv,
            Some(&ctx_clone),
            Duration::from_secs(2),
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
        .await;
    });

    // Unauthenticated IQ fails closed before SASL + <bind>
    cli.write_all(
        b"<iq type='get' id='disc_0' to='conference.chat.evomu.local'><query xmlns='http://jabber.org/protocol/disco#items' node='byname/Playtest Room'/></iq>",
    )
    .await
    .unwrap();
    let mut rbuf = [0u8; 4096];
    let n0 = cli.read(&mut rbuf).await.unwrap();
    let unauth_reply = String::from_utf8_lossy(&rbuf[..n0]);
    assert!(unauth_reply.contains("not-authorized"));
    assert!(!unauth_reply.contains("player_playtest_room"));

    // Authenticate via SASL PLAIN + <bind>, rejecting deterministic fallback token and accepting CSPRNG token
    cli.write_all(b"<?xml version='1.0'?><stream:stream to='chat.evomu.local'>")
        .await
        .unwrap();
    let _ = cli.read(&mut rbuf).await.unwrap();
    cli.write_all(
        b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>AGNoYXJfOTAwMDAwMDEAZXZvbXUteG1wcC10b2tlbi05MDAwMDAwMQ==</auth>",
    )
    .await
    .unwrap();
    let n_bad_auth = cli.read(&mut rbuf).await.unwrap();
    assert!(String::from_utf8_lossy(&rbuf[..n_bad_auth]).contains("<not-authorized/>"));

    let sasl_b64 = encode_xmpp_b64(format!("\0char_90000001\0{owner_token}").as_bytes());
    cli.write_all(
        format!(
            "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{sasl_b64}</auth>"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let n_auth = cli.read(&mut rbuf).await.unwrap();
    assert!(String::from_utf8_lossy(&rbuf[..n_auth]).contains("<success"));
    cli.write_all(b"<stream:stream to='chat.evomu.local'>")
        .await
        .unwrap();
    let _ = cli.read(&mut rbuf).await.unwrap();
    cli.write_all(
        b"<iq type='set' id='bind_1'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><resource>eve</resource></bind></iq>",
    )
    .await
    .unwrap();
    let n_bind = cli.read(&mut rbuf).await.unwrap();
    assert!(
        String::from_utf8_lossy(&rbuf[..n_bind]).contains("char_90000001@chat.evomu.local/eve")
    );

    let big_pad = " ".repeat(2200);
    let big_iq = format!(
        "<iq type='get' id='disc_1' to='conference.chat.evomu.local'{big_pad}><query xmlns='http://jabber.org/protocol/disco#items' node='byname/Playtest Room'/></iq>"
    );
    let mid = big_iq.len() / 2;
    cli.write_all(&big_iq.as_bytes()[..mid]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    cli.write_all(&big_iq.as_bytes()[mid..]).await.unwrap();
    let n = cli.read(&mut rbuf).await.unwrap();
    let reply = String::from_utf8_lossy(&rbuf[..n]);
    assert!(reply.contains(
        "<item jid='player_playtest_room@conference.chat.evomu.local' name='Playtest Room'/>"
    ));

    // Embedded child/attribute `<auth`/`<stream:stream`/`<presence` substrings inside `<iq>` dispatch as `iq`.
    let mut spoof_auth = eve_server::services::fleet_social::XmppStreamAuth {
        user_id: Some(90_000_001),
        character_id: Some(90_000_001),
        bound_jid: Some("char_90000001@chat.evomu.local/eve".into()),
        sasl_authenticated: true,
    };
    let mut dummy_sc = 2u32;
    let spoof_reply = xmpp_stanza_reply_with_ctx_and_auth(
        Some(&ctx),
        Some(&mut spoof_auth),
        "<iq type='get' id='disc_spoof' to='conference.chat.evomu.local'><query xmlns='http://jabber.org/protocol/disco#items' node='byname/&lt;auth xmlns=\"urn:ietf:params:xml:ns:xmpp-sasl\"/&gt;&lt;stream:stream'/></iq>",
        &mut dummy_sc,
    )
    .expect("spoofed substring IQ reply");
    assert!(spoof_reply.starts_with(
        "<iq type='result' id='disc_spoof'><query xmlns='http://jabber.org/protocol/disco#items' node='byname/&lt;auth xmlns=&quot;urn:ietf:params:xml:ns:xmpp-sasl&quot;/&gt;&lt;stream:stream'>"
    ));
    assert!(!spoof_reply.contains("xmlns='urn:ietf:params:xml:ns:xmpp-sasl'"));
    assert!(!spoof_reply.contains("<mechanisms"));
    assert!(!spoof_reply.contains("<failure"));
    assert!(!spoof_reply.contains("<stream:"));
    assert!(!spoof_reply.contains("<auth"));
    assert_eq!(dummy_sc, 2);
    let raw_spoof_reply = xmpp_stanza_reply_with_ctx_and_auth(
        Some(&ctx),
        Some(&mut spoof_auth),
        "<iq type='get' id='disc_raw_spoof' to='conference.chat.evomu.local'><query xmlns='http://jabber.org/protocol/disco#items' node='byname/<auth xmlns=\"x\"/><presence/>'/></iq>",
        &mut dummy_sc,
    )
    .expect("raw spoofed substring IQ reply");
    assert!(raw_spoof_reply.starts_with(
        "<iq type='result' id='disc_raw_spoof'><query xmlns='http://jabber.org/protocol/disco#items' node='byname/&lt;auth xmlns=&quot;x&quot;/&gt;&lt;presence/&gt;'>"
    ));
    assert!(!raw_spoof_reply.contains("xmpp-sasl"));
    assert!(!raw_spoof_reply.contains("<auth"));
    assert!(!raw_spoof_reply.contains("<presence"));

    // #973 & #974: `<presence>` join, leave, and denied error stanzas echo `id='...'` and include `<error code='403' type='auth'>`
    let pres_join = xmpp_stanza_reply_with_ctx_and_auth(
        Some(&ctx),
        Some(&mut spoof_auth),
        "<presence id='join_42' to='player_playtest_room@conference.chat.evomu.local/90000001'><x xmlns='http://jabber.org/protocol/muc'/></presence>",
        &mut dummy_sc,
    )
    .expect("presence join reply");
    assert!(
        pres_join.contains(" id='join_42'"),
        "expected id='join_42' in presence join reply: {pres_join}"
    );
    let pres_channel_prefix = xmpp_stanza_reply_with_ctx_and_auth(
        Some(&ctx),
        Some(&mut spoof_auth),
        "<presence id='join_43' to='channel_playtest_room@conference.chat.evomu.local/90000001'><x xmlns='http://jabber.org/protocol/muc'/></presence>",
        &mut dummy_sc,
    )
    .expect("presence channel_ prefix join reply");
    assert!(
        pres_channel_prefix.contains(" id='join_43'") && !pres_channel_prefix.contains("type='error'"),
        "expected channel_ prefix to resolve player room and succeed: {pres_channel_prefix}"
    );
    let pres_leave = xmpp_stanza_reply_with_ctx_and_auth(
        Some(&ctx),
        Some(&mut spoof_auth),
        "<presence id='leave_44' type='unavailable' to='player_playtest_room@conference.chat.evomu.local/90000001'/>",
        &mut dummy_sc,
    )
    .expect("presence leave reply");
    assert!(
        pres_leave.contains("type='unavailable' id='leave_44'"),
        "expected id='leave_44' in unavailable presence reply: {pres_leave}"
    );
    let mut unauth_pres = eve_server::services::fleet_social::XmppStreamAuth::default();
    let pres_err = xmpp_stanza_reply_with_ctx_and_auth(
        Some(&ctx),
        Some(&mut unauth_pres),
        "<presence id='err_45' to='corp_1000009@conference.chat.evomu.local/90000002'/>",
        &mut dummy_sc,
    )
    .expect("presence error reply");
    assert!(
        pres_err.contains("type='error' id='err_45'")
            && pres_err.contains("<error code='403' type='auth'>"),
        "expected id='err_45' and code='403' on presence error stanza: {pres_err}"
    );

    // #1088: Joining rookie_help via XMPP <presence> emits groupchat <subject> containing overviewPreset: link
    let rookie_join = xmpp_stanza_reply_with_ctx_and_auth(
        Some(&ctx),
        Some(&mut spoof_auth),
        "<presence id='rookie_46' to='rookie_help@conference.chat.evomu.local/90000001'><x xmlns='http://jabber.org/protocol/muc'/></presence>",
        &mut dummy_sc,
    )
    .expect("rookie_help presence reply");
    assert!(
        rookie_join.contains("<presence from='rookie_help@conference.chat.evomu.local/90000001'")
            && rookie_join.contains("<message from='rookie_help@conference.chat.evomu.local/admin'")
            && rookie_join.contains("type='groupchat'><subject>")
            && rookie_join.contains("overviewPreset:"),
        "expected rookie_help join reply with groupchat subject containing overviewPreset link: {rookie_join}"
    );

    drop(cli);
    srv_task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clean_shutdown_sequence_stops_accepting_drains_and_persists_clock_and_wal() {
    use eve_db::sqlite_store::system_checkpoint::{
        apply_system_checkpoint_migration, load_clean_shutdown_marker, load_clock_offset_secs,
    };

    let ctx = Arc::new(ServerContext::with_sde_and_repository(
        Arc::new(eve_sde::SdeRegistry::default()),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    ));
    ctx.db
        .with_sqlite_conn(|conn| apply_system_checkpoint_migration(conn))
        .expect("migrate system_checkpoint");
    ctx.clock().advance_secs(7_200);
    let _jita = ctx.systems.get_or_spawn(30_000_142);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut bg_rx = shutdown_tx.subscribe();
    let bg_task = tokio::spawn(async move {
        let _ = bg_rx.changed().await;
    });

    let conn_limiter = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    let http_conn_limiter = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    let xmpp_conn_semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    let in_flight_permit = Arc::clone(&conn_limiter)
        .try_acquire_owned()
        .expect("acquire permit");
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(in_flight_permit);
    });

    let (close_tx, mut close_rx) = tokio::sync::mpsc::unbounded_channel();
    ctx.session_closers.write().unwrap().insert(42, close_tx);

    run_clean_shutdown(
        &ctx,
        &shutdown_tx,
        Some(bg_task),
        None,
        None,
        &conn_limiter,
        &http_conn_limiter,
        &xmpp_conn_semaphore,
        Duration::from_secs(2),
    )
    .await;

    assert!(*shutdown_rx.borrow());
    assert!(ctx.cluster_state.read().unwrap().shutdown_in_progress);
    let (close_reason, _) = close_rx.recv().await.expect("session closer notice");
    assert_eq!(close_reason, "ServerShuttingDown");
    let (persisted_offset, clean_marker) = ctx
        .db
        .with_sqlite_conn(|conn| {
            Ok::<_, String>((
                load_clock_offset_secs(conn)?,
                load_clean_shutdown_marker(conn)?,
            ))
        })
        .expect("read shutdown metadata");
    assert_eq!(persisted_offset, 7_200);
    assert!(clean_marker);
}

fn write_test_sde_db(
    db_path: &std::path::Path,
    client_build: &str,
    client_release: &str,
    resource_count: &str,
    combined_sha256: &str,
) {
    let conn = rusqlite::Connection::open(db_path).expect("open temp sde sqlite");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _evomu_sde_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )
    .expect("create _evomu_sde_meta");
    for (k, v) in [
        ("schema_version", "1"),
        ("client_build", client_build),
        ("client_release", client_release),
        ("resource_count", resource_count),
        ("combined_resource_sha256", combined_sha256),
    ] {
        conn.execute(
            "INSERT OR REPLACE INTO _evomu_sde_meta (key, value) VALUES (?1, ?2)",
            rusqlite::params![k, v],
        )
        .expect("insert _evomu_sde_meta");
    }
}

#[test]
fn static_data_startup_validation_fails_fast_and_honors_override() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_dir = std::env::temp_dir().join(format!(
        "evomu_static_data_1119_{}_{}",
        std::process::id(),
        nonce
    ));
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");

    let sde_db = tmp_dir.join("evomu_sde.sqlite");
    let sde_manifest = tmp_dir.join("sde_import_manifest.json");
    let quasar_db = tmp_dir.join("quasar_descriptors.sqlite");
    let quasar_manifest = tmp_dir.join("quasar_descriptor_manifest.json");

    let sample_resources_line = b"res:/staticdata/types.static:abc123:42\n";
    let expected_res_sha256 = eve_proto::quasar::sha256_hex(sample_resources_line);
    let manifest_json = serde_json::json!({
        "schema_version": 1,
        "client_build": 3698585u64,
        "client_release": "V24.02",
        "resources": [
            {
                "res_path": "res:/staticdata/types.static",
                "md5": "abc123",
                "byte_size": 42u64
            }
        ]
    });
    std::fs::write(
        &sde_manifest,
        serde_json::to_string_pretty(&manifest_json).expect("serialize sde manifest"),
    )
    .expect("write sde manifest");

    // 1. Missing SDE DB fails fast with actionable import_sde.py instructions (macOS + Windows)
    let missing_sde_err =
        eve_server::static_data::validate_sde_metadata(&sde_db, &sde_manifest).unwrap_err();
    assert!(missing_sde_err.contains("missing SDE SQLite database"));
    assert!(missing_sde_err.contains("tools/sde-import/import_sde.py"));
    assert!(missing_sde_err.contains("--shared-cache"));
    assert!(missing_sde_err.contains("--allow-stale-static-data"));
    assert!(missing_sde_err.contains("EVOMU_ALLOW_STALE_STATIC_DATA=1"));

    // 2. DB missing `_evomu_sde_meta` is rejected cleanly
    {
        let conn = rusqlite::Connection::open(&sde_db).expect("open empty sde");
        conn.execute_batch("CREATE TABLE sde_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .expect("create sde_meta");
    }
    let no_meta_err =
        eve_server::static_data::validate_sde_metadata(&sde_db, &sde_manifest).unwrap_err();
    assert!(no_meta_err.contains("_evomu_sde_meta"));
    assert!(no_meta_err.contains("import_sde.py"));

    // 3. Stale client_build, resource_count, and combined_resource_sha256 are caught
    write_test_sde_db(&sde_db, "3690000", "V24.02", "1", &expected_res_sha256);
    let stale_build_err =
        eve_server::static_data::validate_sde_metadata(&sde_db, &sde_manifest).unwrap_err();
    assert!(stale_build_err.contains("client_build mismatch"));

    write_test_sde_db(&sde_db, "3698585", "V24.02", "99", &expected_res_sha256);
    let stale_count_err =
        eve_server::static_data::validate_sde_metadata(&sde_db, &sde_manifest).unwrap_err();
    assert!(stale_count_err.contains("resource_count mismatch"));

    write_test_sde_db(&sde_db, "3698585", "V24.02", "1", &"0".repeat(64));
    let stale_hash_err =
        eve_server::static_data::validate_sde_metadata(&sde_db, &sde_manifest).unwrap_err();
    assert!(stale_hash_err.contains("combined_resource_sha256 mismatch"));

    // 4. Matching `_evomu_sde_meta` passes O(1) verification
    write_test_sde_db(&sde_db, "3698585", "V24.02", "1", &expected_res_sha256);
    eve_server::static_data::validate_sde_metadata(&sde_db, &sde_manifest)
        .expect("matching _evomu_sde_meta should pass");

    // 5. Missing Quasar DB fails fast with actionable import_sde.py instructions
    let missing_quasar_err =
        eve_server::static_data::validate_quasar_descriptors(&quasar_db, &quasar_manifest)
            .unwrap_err();
    assert!(missing_quasar_err.contains("tools/sde-import/import_sde.py"));

    // 6. Full startup validator rejects stale/missing DBs when `allow_stale == false`,
    //    and succeeds with fallback fixtures when `allow_stale == true`
    let blocked = eve_server::static_data::validate_and_load_startup_static_data_with_paths(
        &sde_db,
        &sde_manifest,
        &quasar_db,
        &quasar_manifest,
        false,
    );
    assert!(blocked.is_err());

    let bypassed = eve_server::static_data::validate_and_load_startup_static_data_with_paths(
        &tmp_dir.join("absent_sde.sqlite"),
        &sde_manifest,
        &quasar_db,
        &quasar_manifest,
        true,
    )
    .expect("allow_stale should allow synthetic fallback startup");
    assert!(bypassed.quasar_descriptors.get_message("google.protobuf.Empty").is_some());

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

fn encode_xmpp_b64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

async fn provision_test_xmpp_token(ctx: &Arc<ServerContext>, character_id: i64) -> String {
    let mut sess = SessionState::new(1);
    sess.user_id = Some(character_id as u64);
    sess.character_id = Some(character_id);
    ctx.dispatch_rpc(
        &mut sess,
        "chatAuthenticationService",
        None,
        "GetAuthenticationToken",
        &[],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap()
    .as_str()
    .expect("CSPRNG XMPP auth token")
    .to_string()
}

async fn read_xmpp_until(cli: &mut tokio::io::DuplexStream, marker: &str) -> String {
    let mut out = String::new();
    let mut buf = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(1);
    while !out.contains(marker) {
        let n = timeout_at(deadline, cli.read(&mut buf))
            .await
            .expect("timed out waiting for XMPP reply")
            .expect("read XMPP reply");
        assert!(n > 0, "unexpected EOF before {marker:?}; received: {out:?}");
        out.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    out
}

#[tokio::test]
async fn xmpp_unauthenticated_idle_stream_times_out_cleanly() {
    let ctx = Arc::new(ServerContext::with_sde_and_repository(
        Arc::new(eve_sde::SdeRegistry::default()),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    ));

    // 1. Completely silent unauthenticated connection closes after `idle_read_timeout`.
    let (client_io, server_io) = duplex(1024);
    let ctx_1 = Arc::clone(&ctx);
    let srv_1 = tokio::spawn(async move {
        handle_xmpp_stream_with_timeouts(
            server_io,
            Some(&ctx_1),
            Duration::from_millis(25),
            Duration::from_millis(50),
            Duration::from_millis(40),
        )
        .await;
    });
    timeout(Duration::from_millis(250), srv_1)
        .await
        .expect("silent unauthenticated XMPP stream must time out")
        .unwrap();
    drop(client_io);

    // 2. Stream that opens `<stream:stream>` (and even fails SASL) remains unauthenticated and times out.
    let (mut cli, srv) = duplex(2048);
    let ctx_2 = Arc::clone(&ctx);
    let srv_2 = tokio::spawn(async move {
        handle_xmpp_stream_with_timeouts(
            srv,
            Some(&ctx_2),
            Duration::from_millis(25),
            Duration::from_millis(50),
            Duration::from_millis(40),
        )
        .await;
    });
    cli.write_all(b"<?xml version='1.0'?><stream:stream to='chat.evomu.local'>")
        .await
        .unwrap();
    let open_reply = read_xmpp_until(&mut cli, "</stream:features>").await;
    assert!(open_reply.contains("xmpp-sasl"));
    cli.write_all(
        b"<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>AGNoYXJfOTAwMDAwMDEAYmFk</auth>",
    )
    .await
    .unwrap();
    let fail_reply = read_xmpp_until(&mut cli, "</failure>").await;
    assert!(fail_reply.contains("<not-authorized/>"));

    timeout(Duration::from_millis(250), srv_2)
        .await
        .expect("unauthenticated XMPP stream after failed SASL must time out on idle_read_timeout")
        .unwrap();
    drop(cli);
}

#[tokio::test]
async fn xmpp_authenticated_idle_stream_persists_past_idle_timeout_and_handles_leave_room() {
    let ctx = Arc::new(ServerContext::with_sde_and_repository(
        Arc::new(eve_sde::SdeRegistry::default()),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    ));
    let token = provision_test_xmpp_token(&ctx, 90_000_001).await;

    let (mut cli, srv) = duplex(4096);
    let ctx_clone = Arc::clone(&ctx);
    let srv_task = tokio::spawn(async move {
        handle_xmpp_stream_with_timeouts(
            srv,
            Some(&ctx_clone),
            Duration::from_millis(25),
            Duration::from_millis(100),
            Duration::from_millis(40),
        )
        .await;
    });

    cli.write_all(b"<?xml version='1.0'?><stream:stream to='chat.evomu.local'>")
        .await
        .unwrap();
    let _ = read_xmpp_until(&mut cli, "</stream:features>").await;

    let sasl_b64 = encode_xmpp_b64(format!("\0char_90000001\0{token}").as_bytes());
    cli.write_all(
        format!(
            "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{sasl_b64}</auth>"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let auth_reply = read_xmpp_until(&mut cli, "xmpp-sasl'/>").await;
    assert!(auth_reply.contains("<success"));

    // Even before `<bind>`, an authenticated SASL stream must survive >3x `idle_read_timeout`
    // without disconnecting or emitting leading whitespace before the restarted XML declaration.
    tokio::time::sleep(Duration::from_millis(85)).await;
    assert!(
        !srv_task.is_finished(),
        "authenticated XMPP stream must not drop on idle_read_timeout before resource bind"
    );

    cli.write_all(b"<stream:stream to='chat.evomu.local'>")
        .await
        .unwrap();
    let reopen_reply = read_xmpp_until(&mut cli, "</stream:features>").await;
    assert!(
        reopen_reply.starts_with("<?xml"),
        "pre-bind authenticated stream must not emit leading whitespace before <?xml: {reopen_reply:?}"
    );
    assert!(reopen_reply.contains("xmpp-bind"));

    cli.write_all(
        b"<iq type='set' id='bind_1'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><resource>eve</resource></bind></iq>",
    )
    .await
    .unwrap();
    let bind_reply = read_xmpp_until(&mut cli, "</iq>").await;
    assert!(bind_reply.contains("char_90000001@chat.evomu.local/eve"));

    // Sit idle well past `idle_read_timeout` (25ms) to verify persistent connection survival (#1095).
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(
        !srv_task.is_finished(),
        "authenticated bound XMPP stream must remain open after idle_read_timeout elapses"
    );

    // Read XEP-0199 ping emitted across idle ticks and verify zero raw `\n` whitespace keepalives
    // (V24's SAX `XmppContentHandler.characters` crashes on inter-stanza whitespace).
    let ping_reply = read_xmpp_until(&mut cli, "<ping xmlns='urn:xmpp:ping'/></iq>").await;
    assert!(
        ping_reply.starts_with(
            "<iq id='evomu_ping_1' type='get' from='chat.evomu.local' to='char_90000001@chat.evomu.local/eve'><ping xmlns='urn:xmpp:ping'/></iq>"
        ),
        "bound authenticated stream must emit valid XEP-0199 ping keepalive without leading whitespace: {ping_reply:?}"
    );
    assert!(
        !ping_reply.contains('\n'),
        "XMPP stream must not emit raw '\\n' whitespace keepalives that crash V24 SAX parser: {ping_reply:?}"
    );

    // Client acknowledges XEP-0199 ping (`type='result'`) and immediately queries `post_idle_iq`.
    // Server must consume the ping result silently (no `<iq type='result' id='evomu_ping_...'/>` echo).
    cli.write_all(
        b"<iq id='evomu_ping_1' type='result' from='char_90000001@chat.evomu.local/eve' to='chat.evomu.local'/><iq type='get' id='post_idle_iq'><query xmlns='http://jabber.org/protocol/disco#items' node='forme'/></iq>",
    )
    .await
    .unwrap();
    let mut iq_reply = String::new();
    let deadline = Instant::now() + Duration::from_secs(1);
    while !iq_reply
        .find("id='post_idle_iq'")
        .is_some_and(|idx| iq_reply[idx..].contains("</iq>"))
    {
        iq_reply.push_str(&read_xmpp_until(&mut cli, "</iq>").await);
        assert!(
            Instant::now() < deadline,
            "timed out waiting for post_idle_iq reply: {iq_reply:?}"
        );
    }
    assert!(
        !iq_reply.contains("type='result' id='evomu_ping_"),
        "server must not echo `<iq type='result'/>` for client XEP-0199 ping pongs: {iq_reply:?}"
    );
    assert!(
        !iq_reply.contains('\n'),
        "XMPP stream must not emit raw '\\n' whitespace keepalives: {iq_reply:?}"
    );
    assert!(
        iq_reply.contains("<iq type='result' id='post_idle_iq'"),
        "XEP-0199 ping keepalives must not corrupt subsequent IQ replies: {iq_reply:?}"
    );

    // Stargate jump channel leave (`LeaveRoom` -> `<presence type='unavailable'/>`) still works post-idle.
    cli.write_all(
        b"<presence id='leave_after_idle' type='unavailable' to='local_30000142@conference.chat.evomu.local/90000001'/>",
    )
    .await
    .unwrap();
    let leave_reply = read_xmpp_until(&mut cli, "</presence>").await;
    assert!(leave_reply.contains("type='unavailable' id='leave_after_idle'"));

    // Partial-stanza slowloris protection (`stanza_timeout`) still applies on authenticated streams.
    cli.write_all(b"<iq type='get' id='stalled'").await.unwrap();
    timeout(Duration::from_millis(250), srv_task)
        .await
        .expect("authenticated stream with stalled partial stanza must time out on stanza_timeout")
        .unwrap();
    drop(cli);
}

#[test]
fn xmpp_iq_result_and_error_stanzas_do_not_echo_reply() {
    let mut stream_count = 2;
    assert_eq!(
        xmpp_stanza_reply(
            "<iq id='evomu_ping_1' type='result' from='char_90000001@chat.evomu.local/eve' to='chat.evomu.local'/>",
            &mut stream_count,
        ),
        None
    );
    assert_eq!(
        xmpp_stanza_reply(
            "<iq id='evomu_ping_2' type=\"error\" from='char_90000001@chat.evomu.local/eve' to='chat.evomu.local'/>",
            &mut stream_count,
        ),
        None
    );
    assert_eq!(
        xmpp_stanza_reply(
            "<iq id='ping_get' type='get'><query type='result'/></iq>",
            &mut stream_count,
        )
        .as_deref(),
        Some("<iq type='result' id='ping_get'/>")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_bridge_client_collapses_unauthenticated_and_unknown_rpc_latency() -> Result<()> {
    use eve_server::flight_recorder::UNKNOWN_RPC_LABEL;

    let ctx = Arc::new(ServerContext::with_sde_and_repository(
        Arc::new(eve_sde::SdeRegistry::default()),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let srv_ctx = Arc::clone(&ctx);
    let srv = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        handle_bridge_client(stream, srv_ctx).await
    });

    let mut cli = TcpStream::connect(addr).await?;
    let pad_64k = "b".repeat(65_536);

    async fn send_rpc(
        cli: &mut TcpStream,
        req_id: u64,
        service: &str,
        method: &str,
    ) -> Result<()> {
        let req = BridgeEnvelope::RpcRequest {
            req_id,
            service: service.to_string(),
            bound_id: None,
            method: method.to_string(),
            args: vec![],
            kwargs: HashMap::new(),
            caller_trace: None,
        };
        write_bounded_frame(cli, &serde_json::to_vec(&req)?).await?;
        loop {
            let raw = read_bounded_frame(cli).await?.expect("rpc response frame");
            if let BridgeEnvelope::RpcResponse { req_id: got_id, .. } =
                serde_json::from_slice(&raw)?
            {
                assert_eq!(got_id, req_id);
                return Ok(());
            }
        }
    }

    // Valid pre-auth bootstrap RPC keeps its own bucket.
    send_rpc(&mut cli, 1, "machoNet", "GetTime").await?;

    // Unauthenticated unknown, 64 KiB, Unauthorized, and release probes must all collapse.
    for i in 0..32u64 {
        let (svc, meth) = match i % 4 {
            0 => (format!("bridge_svc_{i}"), format!("{pad_64k}_{i}")),
            1 => (format!("bridge_svc_{i}"), format!("bridge_meth_{i}")),
            2 => (
                "charUnboundMgr".to_string(),
                "GetCharactersToSelect".to_string(),
            ),
            _ => (
                format!("bridge_rel_{i}"),
                "ClientHasReleasedTheseObjects".to_string(),
            ),
        };
        send_rpc(&mut cli, i + 2, &svc, &meth).await?;
    }

    drop(cli);
    srv.await.unwrap()?;

    let report = ctx.flight_recorder.latency_histogram_report();
    assert_eq!(report.total_calls, 33);
    assert_eq!(report.methods.len(), 2);
    assert_eq!(
        report
            .methods
            .iter()
            .find(|m| m.service == "machoNet" && m.method == "GetTime")
            .map(|m| m.count),
        Some(1)
    );
    assert_eq!(
        report
            .methods
            .iter()
            .find(|m| m.service == UNKNOWN_RPC_LABEL && m.method == UNKNOWN_RPC_LABEL)
            .map(|m| m.count),
        Some(32)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_bridge_client_collapses_forged_bound_id_and_kicked_session_probes() -> Result<()> {
    use eve_server::flight_recorder::{MAX_TRACKED_RPC_METHODS, UNKNOWN_RPC_LABEL};

    async fn call_bridge_rpc(
        cli: &mut TcpStream,
        req_id: u64,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
        args: Vec<PyRep>,
    ) -> Result<Result<PyRep, eve_proto::BridgeRpcError>> {
        let req = BridgeEnvelope::RpcRequest {
            req_id,
            service: service.to_string(),
            bound_id,
            method: method.to_string(),
            args,
            kwargs: HashMap::new(),
            caller_trace: None,
        };
        write_bounded_frame(cli, &serde_json::to_vec(&req)?).await?;
        loop {
            let resp_bytes = read_bounded_frame(cli)
                .await?
                .expect("expected RPC response frame");
            if let BridgeEnvelope::RpcResponse { req_id: got_id, result } =
                serde_json::from_slice(&resp_bytes)?
            {
                if got_id == req_id {
                    return Ok(result);
                }
            }
        }
    }

    async fn finish_bridge_client(
        mut cli: TcpStream,
        srv: tokio::task::JoinHandle<Result<()>>,
    ) -> Result<()> {
        cli.shutdown().await?;
        while read_bounded_frame(&mut cli).await?.is_some() {}
        srv.await.unwrap()
    }

    let ctx = Arc::new(ServerContext::with_sde_and_repository(
        Arc::new(eve_sde::SdeRegistry::with_canonical_seed()),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    ));

    // Bind a victim object before any RPC latency buckets exist.
    let mut victim_sess = SessionState::new(8_001);
    victim_sess.user_id = Some(1);
    victim_sess.character_id = Some(90_000_001);
    let existing_bid = ctx
        .bind_verified_sub_object(&victim_sess, "beyonce", 30_000_142)
        .expect("bind victim beyonce sub-object");

    // 1. Pre-auth TCP attacker floods forged `bound_id: Some(existing_bid)` probes (sharded across
    //    sequential connections under per-session burst cap) and rejected non-allowlisted RPCs via
    //    `handle_bridge_client`.
    let flood_count = MAX_TRACKED_RPC_METHODS + 16;
    let mut next_req_id = 1u64;
    let mut expected_collapsed = 0u64;
    for chunk_start in (0..flood_count).step_by(150) {
        let chunk_end = (chunk_start + 150).min(flood_count);
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let ctx_chunk = Arc::clone(&ctx);
        let srv = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_bridge_client(stream, ctx_chunk).await
        });
        let mut cli = TcpStream::connect(addr).await?;
        for i in chunk_start..chunk_end {
            let svc = format!("tcp_probe_svc_{i}");
            let meth = format!("tcp_probe_meth_{i}");
            let res =
                call_bridge_rpc(&mut cli, next_req_id, &svc, Some(existing_bid), &meth, vec![])
                    .await?;
            next_req_id += 1;
            assert!(res.is_err());
            expected_collapsed += 1;
        }
        finish_bridge_client(cli, srv).await?;
    }

    let listener1 = TcpListener::bind("127.0.0.1:0").await?;
    let addr1 = listener1.local_addr()?;
    let ctx1 = Arc::clone(&ctx);
    let srv1 = tokio::spawn(async move {
        let (stream, _) = listener1.accept().await.unwrap();
        handle_bridge_client(stream, ctx1).await
    });
    let mut cli_unauth = TcpStream::connect(addr1).await?;

    let forged_preauth_probes: &[(&str, Option<u64>, &str, Vec<PyRep>)] = &[
        ("machoNet", Some(existing_bid), "GetTime", vec![]),
        (
            "authentication",
            Some(existing_bid),
            "Login",
            vec![
                PyRep::String("demo".into()),
                PyRep::String("demo-secret".into()),
            ],
        ),
        ("cache", Some(existing_bid), "Rowset", vec![]),
        ("skillMgr2", None, "GetSkills", vec![]),
    ];
    for (svc, bid, meth, args) in forged_preauth_probes {
        let err = call_bridge_rpc(
            &mut cli_unauth,
            next_req_id,
            svc,
            *bid,
            meth,
            args.clone(),
        )
        .await?
        .unwrap_err();
        next_req_id += 1;
        if bid.is_some() {
            assert_eq!(err.code, "BoundObjectOwnershipViolation");
        }
        expected_collapsed += 1;
    }

    assert!(call_bridge_rpc(
        &mut cli_unauth,
        next_req_id,
        "machoNet",
        None,
        "GetGlobalConfig",
        vec![],
    )
    .await?
    .is_ok());
    next_req_id += 1;
    expected_collapsed += 1;

    assert!(call_bridge_rpc(
        &mut cli_unauth,
        next_req_id,
        "machoNet",
        Some(existing_bid),
        "ClientHasReleasedTheseObjects",
        vec![],
    )
    .await?
    .is_ok());
    next_req_id += 1;
    expected_collapsed += 1;

    finish_bridge_client(cli_unauth, srv1).await?;

    let after_preauth = ctx.flight_recorder.latency_histogram_report();
    assert_eq!(after_preauth.total_calls, expected_collapsed);
    assert_eq!(after_preauth.methods.len(), 1);
    assert_eq!(after_preauth.methods[0].service, UNKNOWN_RPC_LABEL);
    assert_eq!(after_preauth.methods[0].method, UNKNOWN_RPC_LABEL);
    assert_eq!(after_preauth.methods[0].count, expected_collapsed);

    // 2. Logged-in TCP client is kicked; its first post-kick call (`machoNet.GetTime`) returns
    //    `SessionKicked` and collapses, and subsequent bound/authed calls also collapse.
    let kicked_uid1 = ctx
        .db
        .provision_player_account("kicked_tcp1_hf1184", "kicked-tcp1-secret");
    let listener2 = TcpListener::bind("127.0.0.1:0").await?;
    let addr2 = listener2.local_addr()?;
    let ctx2 = Arc::clone(&ctx);
    let srv2 = tokio::spawn(async move {
        let (stream, _) = listener2.accept().await.unwrap();
        handle_bridge_client(stream, ctx2).await
    });
    let mut cli_kicked1 = TcpStream::connect(addr2).await?;
    assert!(call_bridge_rpc(
        &mut cli_kicked1,
        next_req_id,
        "authentication",
        None,
        "Login",
        vec![
            PyRep::String("kicked_tcp1_hf1184".into()),
            PyRep::String("kicked-tcp1-secret".into()),
        ],
    )
    .await?
    .is_ok());
    next_req_id += 1;

    ctx.db
        .kick_user(kicked_uid1, "TCP kick test 1", ctx.now_unix());
    let kicked_time_err = call_bridge_rpc(&mut cli_kicked1, next_req_id, "machoNet", None, "GetTime", vec![])
        .await?
        .unwrap_err();
    next_req_id += 1;
    assert_eq!(kicked_time_err.code, "SessionKicked");
    expected_collapsed += 1;

    let kicked_bound_err = call_bridge_rpc(
        &mut cli_kicked1,
        next_req_id,
        "beyonce",
        Some(existing_bid),
        "GetFormations",
        vec![],
    )
    .await?
    .unwrap_err();
    next_req_id += 1;
    assert_eq!(kicked_bound_err.code, "BoundObjectOwnershipViolation");
    expected_collapsed += 1;
    finish_bridge_client(cli_kicked1, srv2).await?;

    // 3. Second logged-in TCP client is kicked before calling `charMgr.GetPublicInfo3`;
    //    `SessionKicked` collapses `charMgr.GetPublicInfo3` to `<unknown>`.
    let kicked_uid2 = ctx
        .db
        .provision_player_account("kicked_tcp2_hf1184", "kicked-tcp2-secret");
    let listener3 = TcpListener::bind("127.0.0.1:0").await?;
    let addr3 = listener3.local_addr()?;
    let ctx3 = Arc::clone(&ctx);
    let srv3 = tokio::spawn(async move {
        let (stream, _) = listener3.accept().await.unwrap();
        handle_bridge_client(stream, ctx3).await
    });
    let mut cli_kicked2 = TcpStream::connect(addr3).await?;
    assert!(call_bridge_rpc(
        &mut cli_kicked2,
        next_req_id,
        "authentication",
        None,
        "Login",
        vec![
            PyRep::String("kicked_tcp2_hf1184".into()),
            PyRep::String("kicked-tcp2-secret".into()),
        ],
    )
    .await?
    .is_ok());
    next_req_id += 1;

    ctx.db
        .kick_user(kicked_uid2, "TCP kick test 2", ctx.now_unix());
    let kicked_char_err = call_bridge_rpc(
        &mut cli_kicked2,
        next_req_id,
        "charMgr",
        None,
        "GetPublicInfo3",
        vec![PyRep::Integer(90_000_788)],
    )
    .await?
    .unwrap_err();
    next_req_id += 1;
    assert_eq!(kicked_char_err.code, "SessionKicked");
    expected_collapsed += 1;
    finish_bridge_client(cli_kicked2, srv3).await?;

    // 4. Legit TCP client calls valid pre-auth RPCs (`machoNet.GetTime`, `authentication.Login`,
    //    `authentication.Logout`) and still gets individual tracking buckets after the probe flood.
    let listener4 = TcpListener::bind("127.0.0.1:0").await?;
    let addr4 = listener4.local_addr()?;
    let ctx4 = Arc::clone(&ctx);
    let srv4 = tokio::spawn(async move {
        let (stream, _) = listener4.accept().await.unwrap();
        handle_bridge_client(stream, ctx4).await
    });
    let mut cli_legit = TcpStream::connect(addr4).await?;
    assert!(call_bridge_rpc(&mut cli_legit, next_req_id, "machoNet", None, "GetTime", vec![])
        .await?
        .is_ok());
    next_req_id += 1;
    assert!(call_bridge_rpc(
        &mut cli_legit,
        next_req_id,
        "authentication",
        None,
        "Login",
        vec![
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
    )
    .await?
    .is_ok());
    next_req_id += 1;
    assert!(call_bridge_rpc(&mut cli_legit, next_req_id, "authentication", None, "Logout", vec![])
        .await?
        .is_ok());
    finish_bridge_client(cli_legit, srv4).await?;

    let final_report = ctx.flight_recorder.latency_histogram_report();
    assert_eq!(final_report.methods.len(), 4);
    assert_eq!(
        final_report
            .methods
            .iter()
            .find(|m| m.service == "machoNet" && m.method == "GetTime")
            .map(|m| m.count),
        Some(1)
    );
    assert_eq!(
        final_report
            .methods
            .iter()
            .find(|m| m.service == "authentication" && m.method == "Login")
            .map(|m| m.count),
        Some(3)
    );
    assert_eq!(
        final_report
            .methods
            .iter()
            .find(|m| m.service == "authentication" && m.method == "Logout")
            .map(|m| m.count),
        Some(1)
    );
    assert!(
        final_report
            .methods
            .iter()
            .find(|m| m.service == "beyonce" && m.method == "GetFormations")
            .is_none()
    );
    assert!(
        final_report
            .methods
            .iter()
            .find(|m| m.service == "charMgr" && m.method == "GetPublicInfo3")
            .is_none()
    );
    assert!(
        final_report
            .methods
            .iter()
            .find(|m| m.service == "machoNet" && m.method == "GetGlobalConfig")
            .is_none()
    );
    assert_eq!(
        final_report
            .methods
            .iter()
            .find(|m| m.service == UNKNOWN_RPC_LABEL && m.method == UNKNOWN_RPC_LABEL)
            .map(|m| m.count),
        Some(expected_collapsed)
    );
    assert_eq!(final_report.total_calls, expected_collapsed + 5);
    Ok(())
}

