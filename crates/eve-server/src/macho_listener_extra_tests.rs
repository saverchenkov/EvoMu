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
            [("ownerName", eve_proto::UserErrorArg::Int(expected_owner_id))],
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

    // Zero-arg InsureShipFailedSingleContract must not inject a hardcoded character ID.
    let zero_arg_ins = eve_proto::BridgeRpcError::from_user_key(
        eve_proto::UserErrorKey::InsureShipFailedSingleContract,
    );
    let (key, dict) = decode_wrapped_user_error(map_call_req_error_body(
        "insuranceSvc",
        "InsureShip",
        &zero_arg_ins,
    ));
    assert_eq!(key, "InsureShipFailedSingleContract");
    assert!(
        dict.is_empty(),
        "zero-arg InsureShipFailedSingleContract must leave ownerName unset for session fallback, got {dict:?}"
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

    let notify_err = eve_proto::BridgeRpcError::custom_notify("Clone installed");
    assert_eq!(
        notify_err.client_presentation("jumpCloneSvc", "InstallCloneInStation"),
        eve_proto::UserErrorPresentation::Notify
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
        client_error: None,
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
        client_error: None,
    };
    assert_eq!(
        decode_wrapped_runtime_error(map_call_req_error_body("inv", "m", &fake_item)),
        "FakeItemNotFound"
    );

    let bound_err = eve_proto::BridgeRpcError {
        code: "InvalidBoundObject".into(),
        message: "bad oid".into(),
        client_error: None,
    };
    let (key, dict) = decode_wrapped_user_error(map_call_req_error_body("svc", "m", &bound_err));
    assert_eq!(key, "UnMachoDestination");
    assert_eq!(dict[0].1.as_str(), Some("InvalidBoundObject: bad oid"));

    for code in ["BoundObjectOwnershipViolation", "BindAuthorizationDenied"] {
        let stale_err = eve_proto::BridgeRpcError {
            code: code.into(),
            message: "stale bound handle".into(),
            client_error: None,
        };
        let (key, dict) = decode_wrapped_user_error(map_call_req_error_body(
            "ship",
            "MachoBindObject",
            &stale_err,
        ));
        assert_eq!(key, "UnMachoDestination");
        assert_eq!(
            dict[0].1.as_str(),
            Some(format!("{code}: stale bound handle").as_str())
        );
    }

    let slash_err = eve_proto::BridgeRpcError {
        code: "SlashError".into(),
        message: "bad syntax".into(),
        client_error: None,
    };
    let (key, dict) = decode_wrapped_user_error(map_call_req_error_body("slash", "SlashCmd", &slash_err));
    assert_eq!(key, "SlashError");
    assert_eq!(dict[0].0, "reason");
    assert_eq!(dict[0].1.as_str(), Some("bad syntax"));

    let slash_cmd_err = eve_proto::BridgeRpcError {
        code: "UnknownSlashCommand".into(),
        message: "unknown /foo".into(),
        client_error: None,
    };
    let (key, dict) = decode_wrapped_user_error(map_call_req_error_body(
        "publicQaToolsServer",
        "SlashCmd",
        &slash_cmd_err,
    ));
    assert_eq!(key, "SlashError");
    assert_eq!(dict[0].0, "reason");
    assert_eq!(dict[0].1.as_str(), Some("UnknownSlashCommand: unknown /foo"));

    let cargo_err = eve_proto::BridgeRpcError::hold_capacity_error(
        eve_proto::UserErrorKey::NotEnoughCargoSpace,
        40.0,
        12.5,
    );
    assert_eq!(
        cargo_err.client_presentation("inv", "Add"),
        eve_proto::UserErrorPresentation::Modal
    );
    let (key, dict) = decode_wrapped_user_error(map_call_req_error_body("inv", "Add", &cargo_err));
    assert_eq!(key, "NotEnoughCargoSpace");
    assert_eq!(dict[0].0, "volume");
    assert_eq!(dict[0].1.as_f64(), Some(40.0));
    assert_eq!(dict[1].0, "available");
    assert_eq!(dict[1].1.as_f64(), Some(12.5));

    let max_orders = eve_proto::BridgeRpcError::max_open_orders_reached(17, 17);
    let (key, dict) = decode_wrapped_user_error(map_call_req_error_body("marketProxy", "PlaceBuyOrder", &max_orders));
    assert_eq!(key, "MarketExceededOrderCount");
    assert_eq!(dict[0], ("curCnt".to_string(), PyRep::Integer(17)));
    assert_eq!(dict[1], ("maxCnt".to_string(), PyRep::Integer(17)));

    for code in ["OutsideDockingPerimeter", "DockingApproach"] {
        let dock_err = eve_proto::BridgeRpcError {
            code: code.into(),
            message: "Ship is 12000.0 m from docking target".into(),
            client_error: None,
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

#[tokio::test]
async fn section4_outbound_call_rsp_marshal_failure_returns_wrapped_internal_error() {
    let proxy_node_id = MACHO_PROXY_NODE_ID;
    let client_id = 9001u64;
    let user_id = Some(555u64);
    let req_dest = MachoAddress::Node {
        node_id: proxy_node_id,
        service: Some("beyonce".into()),
        call_id: 0,
    };

    // 1. Reply serialization failure (recursion depth exceeded) still answers the call_id.
    {
        let (mut writer, mut reader) = tokio::io::duplex(64 * 1024);
        let req_source = MachoAddress::Client {
            client_id,
            call_id: 77,
            service: None,
        };
        let mut deeply_nested = PyRep::None;
        for _ in 0..(eve_proto::MAX_RECURSION_DEPTH + 10) {
            deeply_nested = PyRep::Tuple(vec![deeply_nested]);
        }
        let frames = [crate::outbound_sequencer::OutFrame::CallRsp {
            call_id: 77,
            frame: deeply_nested,
        }];
        write_out_frames_for_call_req(
            &mut writer,
            proxy_node_id,
            client_id,
            user_id,
            &req_source,
            &req_dest,
            &frames,
        )
        .await
        .unwrap();
        drop(writer);

        let wire = read_framed_pyrep(&mut reader).await.unwrap().unwrap();
        let pkt = MachoPacket::from_pyrep(&wire).unwrap();
        assert_eq!(pkt.packet_type, MachoPacketType::ErrorResponse);
        assert_eq!(pkt.destination.call_id(), 77);
        assert_eq!(pkt.user_id, user_id);
        let msg = decode_wrapped_runtime_error(pkt.payload);
        assert!(
            msg.starts_with("Reply serialization failed:"),
            "expected serialization failure message, got {msg:?}"
        );
        assert!(matches!(read_framed_pyrep(&mut reader).await, Ok(None)));
    }

    // 2. Oversized CallRsp exceeding MAX_MACHO_FRAME_LEN falls back to wrapped Internal error.
    {
        let (mut writer, mut reader) = tokio::io::duplex(64 * 1024);
        let req_source = MachoAddress::Client {
            client_id,
            call_id: 78,
            service: None,
        };
        let frames = [crate::outbound_sequencer::OutFrame::CallRsp {
            call_id: 78,
            frame: PyRep::Bytes(vec![0u8; MAX_MACHO_FRAME_LEN + 64]),
        }];
        write_out_frames_for_call_req(
            &mut writer,
            proxy_node_id,
            client_id,
            user_id,
            &req_source,
            &req_dest,
            &frames,
        )
        .await
        .unwrap();
        drop(writer);

        let wire = read_framed_pyrep(&mut reader).await.unwrap().unwrap();
        let pkt = MachoPacket::from_pyrep(&wire).unwrap();
        assert_eq!(pkt.packet_type, MachoPacketType::ErrorResponse);
        assert_eq!(pkt.destination.call_id(), 78);
        let msg = decode_wrapped_runtime_error(pkt.payload);
        assert!(
            msg.starts_with("Reply serialization failed:"),
            "expected oversized reply fallback message, got {msg:?}"
        );
        assert!(matches!(read_framed_pyrep(&mut reader).await, Ok(None)));
    }

    // 3. Empty outbound frame slice guarantees a CallReq still receives a wrapped Internal reply.
    {
        let (mut writer, mut reader) = tokio::io::duplex(64 * 1024);
        let req_source = MachoAddress::Client {
            client_id,
            call_id: 79,
            service: None,
        };
        write_out_frames_for_call_req(
            &mut writer,
            proxy_node_id,
            client_id,
            user_id,
            &req_source,
            &req_dest,
            &[],
        )
        .await
        .unwrap();
        drop(writer);

        let wire = read_framed_pyrep(&mut reader).await.unwrap().unwrap();
        let pkt = MachoPacket::from_pyrep(&wire).unwrap();
        assert_eq!(pkt.packet_type, MachoPacketType::ErrorResponse);
        assert_eq!(pkt.destination.call_id(), 79);
        let msg = decode_wrapped_runtime_error(pkt.payload);
        assert_eq!(
            msg,
            "Internal error: CallReq yielded no serializable reply frame"
        );
        assert!(matches!(read_framed_pyrep(&mut reader).await, Ok(None)));
    }
}

#[tokio::test]
async fn section4_inbound_undecodable_and_malformed_calls_never_hang_or_drop() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let addr = spawn_test_listener(Arc::clone(&ctx)).await;

    let (mut s, client_id, user_id) = login_client(addr, "demo", "demo-secret", "DE").await;

    // 1. Undecodable CallReq payload whose call_id prefix IS recoverable answers on call_id=42
    //    without tearing down the connection.
    let call42_pkt = MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client {
            client_id,
            call_id: 42,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: Some("beyonce".into()),
            call_id: 0,
        },
        user_id: Some(user_id),
        payload: PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::String("payload_marker_1234567890".into()),
        ]),
        named_payload: HashMap::new(),
    };
    let raw42 = marshal::encode(&call42_pkt.to_pyrep());
    let truncated42 = &raw42[..raw42.len() - 12];
    assert!(
        marshal::decode_with_limits(
            truncated42,
            MAX_MACHO_FRAME_LEN,
            eve_proto::MAX_DECODED_NODES
        )
        .is_err()
    );
    s.write_u32_le(truncated42.len() as u32).await.unwrap();
    s.write_all(truncated42).await.unwrap();
    s.flush().await.unwrap();

    let err42 =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(err42.packet_type, MachoPacketType::ErrorResponse);
    assert_eq!(err42.destination.call_id(), 42);
    assert_eq!(err42.user_id, Some(user_id));
    let msg42 = decode_wrapped_runtime_error(err42.payload);
    assert!(
        msg42.starts_with("Malformed CallReq payload:"),
        "expected wrapped payload error, got {msg42:?}"
    );

    // 2. Valid-marshal CallReq that fails MachoPacket::from_pyrep recovers call_id=44 and answers.
    let malformed_call44 = PyRep::Object {
        type_name: MachoPacket::class_name(MachoPacketType::CallReq).into(),
        args: Box::new(PyRep::Tuple(vec![
            PyRep::Integer(MachoPacketType::CallReq as i64),
            MachoAddress::Client {
                client_id,
                call_id: 44,
                service: None,
            }
            .to_pyrep(),
            PyRep::Integer(999),
            PyRep::Integer(user_id as i64),
            PyRep::None,
            PyRep::None,
        ])),
    };
    assert!(MachoPacket::from_pyrep(&malformed_call44).is_none());
    write_framed_pyrep(&mut s, &malformed_call44).await.unwrap();

    let err44 =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(err44.packet_type, MachoPacketType::ErrorResponse);
    assert_eq!(err44.destination.call_id(), 44);
    let msg44 = decode_wrapped_runtime_error(err44.payload);
    assert_eq!(msg44, "Malformed CallReq packet");

    // 3. Session remains usable for subsequent requests.
    let ping45 = MachoPacket {
        packet_type: MachoPacketType::PingReq,
        source: MachoAddress::Client {
            client_id,
            call_id: 45,
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
    write_framed_pyrep(&mut s, &ping45.to_pyrep()).await.unwrap();
    let ping45_rsp =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap()).unwrap();
    assert_eq!(ping45_rsp.packet_type, MachoPacketType::PingRsp);
    assert_eq!(ping45_rsp.destination.call_id(), 45);

    // 4. Unrecoverable corrupt frame (no recoverable CallReq header) closes with GPSTransportClosed.
    let garbage: [u8; 5] = [0xff, 0xee, 0xdd, 0xcc, 0xbb];
    s.write_u32_le(garbage.len() as u32).await.unwrap();
    s.write_all(&garbage).await.unwrap();
    s.flush().await.unwrap();

    let closed_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
    let closed = GPSTransportClosed::from_pyrep(&closed_rep)
        .expect("expected GPSTransportClosed on unrecoverable frame");
    assert_eq!(
        closed.reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEPROTOCOL")
    );
    assert!(matches!(read_framed_pyrep(&mut s).await, Ok(None)));
}

fn collect_destiny_actions_from_list(updates: &PyRep) -> Vec<(String, Vec<PyRep>)> {
    let PyRep::List(entries) = updates else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let (name, args) = destiny_entry_action(entry)?;
            Some((name.to_string(), args.to_vec()))
        })
        .collect()
}

fn collect_destiny_actions_from_frames(
    frames: &[crate::outbound_sequencer::OutFrame],
) -> Vec<(String, Vec<PyRep>)> {
    let mut actions = Vec::new();
    for frame in frames {
        if let crate::outbound_sequencer::OutFrame::DoDestinyUpdate { updates, .. } = frame {
            actions.extend(collect_destiny_actions_from_list(updates));
        }
    }
    actions
}

fn contains_warping_fx(actions: &[(String, Vec<PyRep>)], ship_id: i64, start: i64) -> bool {
    actions.iter().any(|(name, args)| {
        name == "OnSpecialFX"
            && args.first().and_then(PyRep::as_i64) == Some(ship_id)
            && args.get(5).and_then(PyRep::as_str) == Some("effects.Warping")
            && match start {
                0 => matches!(args.get(7), Some(PyRep::Integer(0) | PyRep::Bool(false))),
                other => args.get(7).and_then(PyRep::as_i64) == Some(other),
            }
    })
}

fn contains_ship_stop(actions: &[(String, Vec<PyRep>)], ship_id: i64) -> bool {
    actions.iter().any(|(name, args)| {
        name == "Stop" && args.first().and_then(PyRep::as_i64) == Some(ship_id)
    })
}

#[test]
fn test_1147_strip_self_non_dock_warp_stop_fx_and_build_destiny_batch_contexts() {
    use eve_destiny::wire::encode_destiny_updates;
    use eve_destiny::DestinyEvent;

    let ctx = ServerContext::new();
    let ego_ship = 1_897_147_421_i64;
    let peer_ship = 1_897_147_422_i64;

    // 1. NonDock strips only ego's OnSpecialFX("effects.Warping", start=0) while preserving
    //    Stop, warp start (start=1), peer OnSpecialFX("effects.Warping", 0), and unrelated FX.
    let mixed_events = vec![
        DestinyEvent::OnSpecialFX {
            ball_id: ego_ship,
            module_id: None,
            guid: "effects.Warping".into(),
            is_active: false,
        },
        DestinyEvent::Stop { ball_id: ego_ship },
        DestinyEvent::OnSpecialFX {
            ball_id: ego_ship,
            module_id: None,
            guid: "effects.Warping".into(),
            is_active: true,
        },
        DestinyEvent::OnSpecialFX {
            ball_id: peer_ship,
            module_id: None,
            guid: "effects.Warping".into(),
            is_active: false,
        },
        DestinyEvent::OnSpecialFX {
            ball_id: ego_ship,
            module_id: None,
            guid: "effects.JumpIn".into(),
            is_active: false,
        },
    ];
    let mixed_updates = encode_destiny_updates(100, &mixed_events, None);
    let non_dock_batch = build_destiny_batch(
        &ctx,
        1,
        100,
        &mixed_events,
        mixed_updates,
        &[ego_ship],
        SelfDestinyWireContext::NonDock,
    )
    .expect("mixed NonDock batch must remain non-empty");
    let non_dock_actions = collect_destiny_actions_from_list(&non_dock_batch.updates);
    assert!(!contains_warping_fx(&non_dock_actions, ego_ship, 0));
    assert!(contains_ship_stop(&non_dock_actions, ego_ship));
    assert!(contains_warping_fx(&non_dock_actions, ego_ship, 1));
    assert!(contains_warping_fx(&non_dock_actions, peer_ship, 0));
    assert!(non_dock_actions.iter().any(|(name, args)| {
        name == "OnSpecialFX"
            && args.first().and_then(PyRep::as_i64) == Some(ego_ship)
            && args.get(5).and_then(PyRep::as_str) == Some("effects.JumpIn")
    }));

    // 2. NonDock batch containing solely ego's warp stop FX drops cleanly to None.
    let stop_only_events = vec![DestinyEvent::OnSpecialFX {
        ball_id: ego_ship,
        module_id: None,
        guid: "effects.Warping".into(),
        is_active: false,
    }];
    let stop_only_updates = encode_destiny_updates(101, &stop_only_events, None);
    assert!(
        build_destiny_batch(
            &ctx,
            1,
            101,
            &stop_only_events,
            stop_only_updates,
            &[ego_ship],
            SelfDestinyWireContext::NonDock,
        )
        .is_none(),
        "NonDock batch consisting solely of ego warp-stop FX should collapse to None"
    );

    // 3. Direct helper also strips Bool(false) start flags for suppressed ships and respects empty slice.
    let bool_stop_entry = |ship_id: i64| {
        PyRep::Tuple(vec![
            PyRep::Integer(102),
            PyRep::Tuple(vec![
                PyRep::String("OnSpecialFX".into()),
                PyRep::Tuple(vec![
                    PyRep::Integer(ship_id),
                    PyRep::None,
                    PyRep::None,
                    PyRep::None,
                    PyRep::None,
                    PyRep::String("effects.Warping".into()),
                    PyRep::Bool(false),
                    PyRep::Bool(false),
                    PyRep::Bool(false),
                    PyRep::Integer(-1),
                    PyRep::None,
                    PyRep::None,
                    PyRep::Integer(0),
                    PyRep::None,
                ]),
            ]),
        ])
    };
    let bool_updates = PyRep::List(vec![bool_stop_entry(ego_ship), bool_stop_entry(peer_ship)]);
    assert_eq!(
        collect_destiny_actions_from_list(&strip_self_non_dock_warp_stop_fx(
            bool_updates.clone(),
            &[]
        ))
        .len(),
        2
    );
    let filtered_bool_actions =
        collect_destiny_actions_from_list(&strip_self_non_dock_warp_stop_fx(
            bool_updates,
            &[ego_ship],
        ));
    assert!(!contains_warping_fx(&filtered_bool_actions, ego_ship, 0));
    assert!(contains_warping_fx(&filtered_bool_actions, peer_ship, 0));

    // 4. DockSettle preserves ego's OnSpecialFX("effects.Warping", 0) and Stop in atomic order.
    let dock_events = vec![
        DestinyEvent::SetBallMassive {
            ball_id: ego_ship,
            is_massive: true,
        },
        DestinyEvent::OnSpecialFX {
            ball_id: ego_ship,
            module_id: None,
            guid: "effects.Warping".into(),
            is_active: false,
        },
        DestinyEvent::Stop { ball_id: ego_ship },
    ];
    let dock_updates = encode_destiny_updates(103, &dock_events, None);
    let dock_batch = build_destiny_batch(
        &ctx,
        1,
        103,
        &dock_events,
        dock_updates,
        &[ego_ship],
        SelfDestinyWireContext::DockSettle,
    )
    .expect("DockSettle batch must preserve dock-settle events");
    let dock_names: Vec<String> = collect_destiny_actions_from_list(&dock_batch.updates)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(dock_names, vec!["SetBallMassive", "OnSpecialFX", "Stop"]);
}

#[tokio::test]
async fn test_1147_wire_warp_cancel_and_natural_exit_suppress_ego_fx_and_arm_settle() {
    use crate::router::client_wire::ClientWireSession;
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, CombatState, MovementMode};
    use glam::DVec3;

    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx.session_change_timers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();

    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let st_pos = ctx.sde.stations[&st_id].position;
    let ego_char_id = 90_000_001_i64;
    let ego_ship = 895_147_i64;
    let peer_char_id = 90_000_002_i64;
    let peer_ship = 895_148_i64;
    let tayra_type_id = 649_u32;

    let ego_pos = st_pos + DVec3::new(500_000_000.0, 0.0, 0.0);
    let peer_pos = ego_pos + DVec3::new(1_000.0, 0.0, 0.0);

    let (ego_corp_id, peer_corp_id) = ctx.db.with_mut_snapshot(|snap| {
        let mut setup_char = |cid: i64, sid: i64| {
            let corp_id = snap
                .characters
                .get_mut(&cid)
                .map(|ch| {
                    ch.active_ship_id = sid;
                    ch.solar_system_id = sys_id;
                    ch.station_id = None;
                    ch.structure_id = None;
                    ch.corporation_id
                })
                .expect("seeded character");
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: tayra_type_id,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            corp_id
        };
        (
            setup_char(ego_char_id, ego_ship),
            setup_char(peer_char_id, peer_ship),
        )
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let full_combat = CombatState {
        shield_hp: 1_000.0,
        max_shield_hp: 1_000.0,
        armor_hp: 1_000.0,
        max_armor_hp: 1_000.0,
        structure_hp: 1_000.0,
        max_structure_hp: 1_000.0,
        capacitor: 1_000.0,
        max_capacitor: 1_000.0,
        ..Default::default()
    };
    let make_ship_ball = |id: i64, pos: DVec3| Ball {
        id,
        type_id: tayra_type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_100_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    sys_handle
        .spawn_ball_with_combat(make_ship_ball(ego_ship, ego_pos), full_combat.clone())
        .await
        .unwrap();
    sys_handle
        .spawn_ball_with_combat(make_ship_ball(peer_ship, peer_pos), full_combat.clone())
        .await
        .unwrap();

    let mut ego_sess = SessionState::new(11_471);
    ego_sess.user_id = Some(1);
    ego_sess.character_id = Some(ego_char_id);
    ego_sess.corporation_id = Some(ego_corp_id);
    ego_sess.solar_system_id = Some(sys_id);
    ego_sess.ship_id = Some(ego_ship);
    ego_sess.live_notify_listener = true;

    let mut peer_sess = SessionState::new(11_472);
    peer_sess.user_id = Some(2);
    peer_sess.character_id = Some(peer_char_id);
    peer_sess.corporation_id = Some(peer_corp_id);
    peer_sess.solar_system_id = Some(sys_id);
    peer_sess.ship_id = Some(peer_ship);
    peer_sess.live_notify_listener = true;
    ctx.db
        .acquire_online_lock(
            1,
            ego_sess.session_id,
            Some(ego_char_id),
            true,
            ctx.now_unix_secs(),
        )
        .unwrap();
    ctx.db
        .acquire_online_lock(
            2,
            peer_sess.session_id,
            Some(peer_char_id),
            true,
            ctx.now_unix_secs(),
        )
        .unwrap();
    ctx.sync_session(&mut ego_sess);
    ctx.sync_session(&mut peer_sess);

    sys_handle.step_manual(1.0).await.unwrap();
    let _ = sys_handle.drain_observer_updates(ego_ship).await;
    let _ = sys_handle.drain_observer_updates(peer_ship).await;

    let mut ego_wire = ClientWireSession::from_session(&ctx, ego_sess);
    let mut peer_wire = ClientWireSession::from_session(&ctx, peer_sess);
    let ego_beyonce_bid = ctx
        .bind_object(&ego_wire.session, "beyonce", sys_id as i64)
        .expect("bind beyonce");
    let empty_kw = HashMap::new();

    // 1. Start warp: ego receives WarpTo + OnSpecialFX("effects.Warping", start=1).
    let warp1 = ego_wire
        .call_wire(
            "beyonce",
            Some(ego_beyonce_bid),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(st_id)],
            &empty_kw,
        )
        .await;
    assert!(warp1.rpc_result.is_ok(), "1st CmdWarpToStuff: {:?}", warp1.rpc_result);
    let warp1_actions = collect_destiny_actions_from_frames(&warp1.out_frames);
    assert!(contains_warping_fx(&warp1_actions, ego_ship, 1));
    assert!(warp1_actions.iter().any(|(name, _)| name == "WarpTo"));

    // Advance 1s while still in Aligning (~3.05s align time), clearing pending peer start FX.
    sys_handle.step_manual(1.0).await.unwrap();
    let _ = ego_wire.drain_pending_frames();
    let _ = peer_wire.drain_pending_frames();

    // 2. Abort warp with CmdStop (#1147 repro sequence step 2):
    //    - ego receives Stop without OnSpecialFX("effects.Warping", 0)
    //    - warp_stop_settle_deadline is armed immediately on observer drain (before 1Hz tick)
    //    - peer observer STILL receives OnSpecialFX(ego_ship, "effects.Warping", 0)
    let stop1 = ego_wire
        .call_wire("beyonce", Some(ego_beyonce_bid), "CmdStop", &[], &empty_kw)
        .await;
    assert!(stop1.rpc_result.is_ok(), "CmdStop: {:?}", stop1.rpc_result);
    let stop1_actions = collect_destiny_actions_from_frames(&stop1.out_frames);
    assert!(contains_ship_stop(&stop1_actions, ego_ship));
    assert!(
        !contains_warping_fx(&stop1_actions, ego_ship, 0),
        "ego non-dock CmdStop batch must suppress OnSpecialFX(effects.Warping, 0): {stop1_actions:?}"
    );
    assert!(
        sys_handle
            .warp_stop_settle_deadline(ego_ship, std::time::Duration::from_millis(3_500))
            .is_some(),
        "post-CmdStop observer drain must immediately arm warp_stop_settle_deadline"
    );

    sys_handle.step_manual(1.0).await.unwrap();
    let _ = ego_wire.drain_pending_frames();
    let (peer_abort_frames, _) = peer_wire.drain_pending_frames();
    let peer_abort_actions = collect_destiny_actions_from_frames(&peer_abort_frames);
    assert!(contains_ship_stop(&peer_abort_actions, ego_ship));
    assert!(
        contains_warping_fx(&peer_abort_actions, ego_ship, 0),
        "peer observer must still receive ego OnSpecialFX(effects.Warping, 0): {peer_abort_actions:?}"
    );

    // 3. Re-warp immediately after CmdStop (#1147 step 3) and complete natural warp exit:
    //    - re-entering warp re-emits start=1 and clears warp_stopped_at
    //    - natural exit on 1Hz TickBroadcast delivers Stop to ego without OnSpecialFX(0),
    //      while delivering OnSpecialFX(0) to peer.
    let warp2 = ego_wire
        .call_wire(
            "beyonce",
            Some(ego_beyonce_bid),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(st_id)],
            &empty_kw,
        )
        .await;
    assert!(warp2.rpc_result.is_ok(), "2nd CmdWarpToStuff: {:?}", warp2.rpc_result);
    let warp2_actions = collect_destiny_actions_from_frames(&warp2.out_frames);
    assert!(contains_warping_fx(&warp2_actions, ego_ship, 1));
    assert!(
        sys_handle
            .warp_stop_settle_deadline(ego_ship, std::time::Duration::from_millis(3_500))
            .is_none(),
        "new CmdWarpToStuff must clear prior warp_stop_settle_deadline"
    );

    // Move peer ship into Jita 4-4's destination grid so it observes ego's warp arrival/exit.
    let _ = sys_handle.remove_ball(peer_ship).await;
    sys_handle
        .spawn_ball_with_combat(
            make_ship_ball(peer_ship, st_pos + DVec3::new(100_000.0, 0.0, 0.0)),
            full_combat,
        )
        .await
        .unwrap();
    let _ = sys_handle.drain_observer_updates(peer_ship).await;

    let mut ego_tick_actions = Vec::new();
    let mut peer_tick_actions = Vec::new();
    let mut exited_naturally = false;
    for _ in 0..60 {
        sys_handle.step_manual(1.0).await.unwrap();
        let (ef, _) = ego_wire.drain_pending_frames();
        let (pf, _) = peer_wire.drain_pending_frames();
        ego_tick_actions.extend(collect_destiny_actions_from_frames(&ef));
        peer_tick_actions.extend(collect_destiny_actions_from_frames(&pf));
        if let Some((_, snap)) = sys_handle.get_ball_snapshot(ego_ship).await.unwrap() {
            if !matches!(snap.mode, MovementMode::Warp { .. }) {
                exited_naturally = true;
                break;
            }
        }
    }
    assert!(exited_naturally, "2nd warp should finish naturally within 60 ticks");
    assert!(contains_ship_stop(&ego_tick_actions, ego_ship));
    assert!(
        !contains_warping_fx(&ego_tick_actions, ego_ship, 0),
        "ego natural warp exit ticks must suppress OnSpecialFX(effects.Warping, 0): {ego_tick_actions:?}"
    );
    assert!(
        contains_warping_fx(&peer_tick_actions, ego_ship, 0),
        "peer observer must still receive ego OnSpecialFX(effects.Warping, 0) on natural warp exit"
    );
}

