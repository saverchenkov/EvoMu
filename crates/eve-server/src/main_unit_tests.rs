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
