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
                std::time::Duration::from_secs(60),
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

fn decode_wrapped_user_error(body: PyRep) -> (String, Vec<(String, PyRep)>) {
    let PyRep::Tuple(parts) = body else {
        panic!("expected ErrorResponse tuple body, got {body:?}");
    };
    assert_eq!(parts[0].as_i64(), Some(MachoPacketType::CallReq as i64));
    assert_eq!(
        parts[1].as_i64(),
        Some(eve_machonet::MACHONETERR_WRAPPEDEXCEPTION)
    );
    let PyRep::Tuple(inner) = &parts[2] else {
        panic!("expected inner ErrorResponse payload tuple");
    };
    let PyRep::Bytes(raw) = &inner[0] else {
        panic!("expected marshaled exception bytes");
    };
    let decoded = eve_proto::marshal::decode(raw).expect("valid marshal bytes");
    let PyRep::ObjectEx { header, .. } = decoded else {
        panic!("expected ObjectEx UserError, got {decoded:?}");
    };
    let PyRep::Tuple(h_parts) = *header else {
        panic!("expected ObjectEx header tuple");
    };
    assert_eq!(h_parts[0], PyRep::Token("eveexceptions.UserError".into()));
    let PyRep::Tuple(u_args) = &h_parts[1] else {
        panic!("expected UserError args tuple");
    };
    let name = u_args[0]
        .as_str()
        .expect("UserError message key must be string/bytes")
        .to_string();
    let PyRep::Dict(dict_entries) = &u_args[1] else {
        panic!("expected UserError dict args");
    };
    let dict = dict_entries
        .iter()
        .map(|(k, v)| (k.as_str().expect("dict key str").to_string(), v.clone()))
        .collect();
    (name, dict)
}

#[test]
fn map_call_req_error_body_preserves_exact_insurance_user_error_keys() {
    for expected_owner_id in [90_000_001i64, 1_000_009i64] {
        let err = eve_proto::BridgeRpcError::user_error_with_args(
            eve_proto::UserErrorKey::InsureShipFailedSingleContract,
            [("ownerName", PyRep::Integer(expected_owner_id))],
        );
        let (key, dict) = decode_wrapped_user_error(map_call_req_error_body(
            "insuranceSvc",
            "InsureShip",
            &err,
        ));
        assert_eq!(key, "InsureShipFailedSingleContract");
        assert_eq!(
            dict,
            vec![("ownerName".to_string(), PyRep::Integer(expected_owner_id))],
            "InsureShipFailedSingleContract must include integer ownerName for V24 base_insurance.pyj"
        );
    }

    // Zero-arg fallback uses default ownerName without scraping message strings.
    let zero_arg_ins = eve_proto::BridgeRpcError::from_user_key(
        eve_proto::UserErrorKey::InsureShipFailedSingleContract,
    );
    let (key, dict) = decode_wrapped_user_error(map_call_req_error_body(
        "insuranceSvc",
        "InsureShip",
        &zero_arg_ins,
    ));
    assert_eq!(key, "InsureShipFailedSingleContract");
    assert_eq!(
        dict,
        vec![("ownerName".to_string(), PyRep::Integer(90_000_001))]
    );

    for key_enum in [
        eve_proto::UserErrorKey::InsureShipInvalidCost,
        eve_proto::UserErrorKey::InsureShipFailed,
        eve_proto::UserErrorKey::SkillMissing,
        eve_proto::UserErrorKey::CanOnlyDoInStation,
    ] {
        let err = eve_proto::BridgeRpcError::from_user_key(key_enum);
        let (key, dict) = decode_wrapped_user_error(map_call_req_error_body(
            "insuranceSvc",
            "InsureShip",
            &err,
        ));
        assert_eq!(
            key,
            key_enum.as_str(),
            "named UserError '{key_enum}' must not collapse to CustomInfo"
        );
        assert_ne!(key, "CustomInfo");
        assert!(
            dict.is_empty(),
            "expected empty dict for '{key_enum}', got {dict:?}"
        );
    }

    let notify_err = eve_proto::BridgeRpcError::user_error_with_args(
        eve_proto::UserErrorKey::CustomNotify,
        [("notify", PyRep::String("Clone installed".into()))],
    );
    let (n_key, n_dict) = decode_wrapped_user_error(map_call_req_error_body(
        "jumpCloneSvc",
        "InstallCloneInStation",
        &notify_err,
    ));
    assert_eq!(n_key, "CustomNotify");
    assert_eq!(n_dict.len(), 1);
    assert_eq!(n_dict[0].0, "notify");
    assert_eq!(n_dict[0].1.as_str(), Some("Clone installed"));

    // Fallback still maps unrelated errors to CustomInfo with `code: message` info.
    let fallback_err = eve_proto::BridgeRpcError {
        code: "SomeUnmappedError".into(),
        message: "details".into(),
    };
    let (key, dict) = decode_wrapped_user_error(map_call_req_error_body(
        "insuranceSvc",
        "InsureShip",
        &fallback_err,
    ));
    assert_eq!(key, "CustomInfo");
    assert_eq!(dict.len(), 1);
    assert_eq!(dict[0].0, "info");
    assert_eq!(dict[0].1.as_str(), Some("SomeUnmappedError: details"));
}

fn decode_wrapped_runtime_error(body: PyRep) -> String {
    let PyRep::Tuple(parts) = body else {
        panic!("expected ErrorResponse tuple body");
    };
    assert_eq!(parts[0].as_i64(), Some(MachoPacketType::CallReq as i64));
    assert_eq!(
        parts[1].as_i64(),
        Some(eve_machonet::MACHONETERR_WRAPPEDEXCEPTION)
    );
    let PyRep::Tuple(inner) = &parts[2] else {
        panic!("expected inner ErrorResponse payload tuple");
    };
    let PyRep::Bytes(raw) = &inner[0] else {
        panic!("expected marshaled exception bytes");
    };
    let PyRep::ObjectEx { header, .. } = eve_proto::marshal::decode(raw).unwrap() else {
        panic!("expected ObjectEx RuntimeError");
    };
    let PyRep::Tuple(h_parts) = *header else {
        panic!("expected header tuple");
    };
    assert_eq!(h_parts[0], PyRep::Token("exceptions.RuntimeError".into()));
    let PyRep::Tuple(args) = &h_parts[1] else {
        panic!("expected RuntimeError args tuple");
    };
    args[0].as_str().unwrap().to_string()
}

#[test]
fn map_call_req_error_body_preserves_existing_rpc_error_mappings() {
    let unimpl = eve_proto::BridgeRpcError::from_client_error(
        eve_proto::ClientError::Unimplemented {
            service: "insuranceSvc".into(),
            method: "InsureShip".into(),
        },
    );
    assert_eq!(
        decode_wrapped_runtime_error(map_call_req_error_body(
            "insuranceSvc",
            "InsureShip",
            &unimpl
        )),
        "UnimplementedRPC: insuranceSvc.InsureShip is not yet implemented"
    );

    let fake_item = eve_proto::BridgeRpcError {
        code: "FakeItemNotFound".into(),
        message: "item 1".into(),
    };
    assert_eq!(
        decode_wrapped_runtime_error(map_call_req_error_body("inv", "m", &fake_item)),
        "FakeItemNotFound"
    );

    let bound_err = eve_proto::BridgeRpcError {
        code: "InvalidBoundObject".into(),
        message: "bad oid".into(),
    };
    let (key, dict) = decode_wrapped_user_error(map_call_req_error_body("svc", "m", &bound_err));
    assert_eq!(key, "UnMachoDestination");
    assert_eq!(dict[0].1.as_str(), Some("InvalidBoundObject: bad oid"));

    let slash_err = eve_proto::BridgeRpcError {
        code: "SlashError".into(),
        message: "bad syntax".into(),
    };
    let (key, dict) = decode_wrapped_user_error(map_call_req_error_body("slash", "SlashCmd", &slash_err));
    assert_eq!(key, "SlashError");
    assert_eq!(dict[0].0, "reason");
    assert_eq!(dict[0].1.as_str(), Some("bad syntax"));

    let slash_cmd_err = eve_proto::BridgeRpcError {
        code: "UnknownSlashCommand".into(),
        message: "unknown /foo".into(),
    };
    let (key, dict) = decode_wrapped_user_error(map_call_req_error_body(
        "publicQaToolsServer",
        "SlashCmd",
        &slash_cmd_err,
    ));
    assert_eq!(key, "SlashError");
    assert_eq!(dict[0].0, "reason");
    assert_eq!(dict[0].1.as_str(), Some("UnknownSlashCommand: unknown /foo"));

    let cargo_err = eve_proto::BridgeRpcError::user_error_with_args(
        eve_proto::UserErrorKey::NotEnoughCargoSpace,
        [
            ("free", PyRep::Real(12.5)),
            ("required", PyRep::Real(40.0)),
        ],
    );
    let (key, dict) = decode_wrapped_user_error(map_call_req_error_body("inv", "Add", &cargo_err));
    assert_eq!(key, "NotEnoughCargoSpace");
    assert_eq!(dict[0].0, "free");
    assert_eq!(dict[0].1.as_f64(), Some(12.5));
    assert_eq!(dict[1].0, "required");
    assert_eq!(dict[1].1.as_f64(), Some(40.0));

    let max_orders = eve_proto::BridgeRpcError::max_open_orders_reached(17, 17);
    let (key, dict) = decode_wrapped_user_error(map_call_req_error_body("marketProxy", "PlaceBuyOrder", &max_orders));
    assert_eq!(key, "MarketExceededOrderCount");
    assert_eq!(dict[0], ("curCnt".to_string(), PyRep::Integer(17)));
    assert_eq!(dict[1], ("maxCnt".to_string(), PyRep::Integer(17)));

    for code in ["OutsideDockingPerimeter", "DockingApproach"] {
        let dock_err = eve_proto::BridgeRpcError {
            code: code.into(),
            message: "Ship is 12000.0 m from docking target".into(),
        };
        let (key, dict) = decode_wrapped_user_error(map_call_req_error_body(
            "beyonce",
            "CmdDock",
            &dock_err,
        ));
        assert_eq!(key, "DockingApproach");
        assert!(dict.is_empty(), "expected empty dict for '{code}', got {dict:?}");
    }
}
