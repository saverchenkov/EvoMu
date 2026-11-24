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
        None,
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
            None,
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
        None,
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
    crate::router::session_transition::clear_all_session_change_timers(&ctx);

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

#[tokio::test]
async fn build_destiny_batch_and_sequencer_deliver_fx_floor_damage_state_without_regression() {
    use crate::outbound_sequencer::{
        normalize_server_destiny_batch, sequence_tick_with_events, OutFrame, SessionWireState,
    };
    use eve_destiny::{Ball, CombatState, DestinyEvent, MovementMode};
    use glam::DVec3;

    let ctx = Arc::new(ServerContext::new());
    let sys_id = 30_000_142_u32;
    let ship_id = 99_200_001_i64;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 11567,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 800.0,
                max_shield_hp: 1_000.0,
                armor_hp: 800.0,
                max_armor_hp: 1_000.0,
                structure_hp: 800.0,
                max_structure_hp: 1_000.0,
                capacitor: 0.0,
                max_capacitor: 1_000.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    sys_handle.push_set_state(ship_id).await.unwrap();
    let (init_tick, init_evs, init_pyrep) =
        sys_handle.drain_observer_updates(ship_id).await.unwrap();
    let mut rx = sys_handle.subscribe();
    let mut wire = SessionWireState::new(1268, 1268);
    let ep = wire.enter_space(Some(sys_id));

    let set_state_batch = build_destiny_batch(
        &ctx,
        ep,
        init_tick,
        &init_evs,
        init_pyrep,
        &[],
        SelfDestinyWireContext::NonDock,
        None,
    )
    .expect("SetState batch");
    for sub in normalize_server_destiny_batch(&mut wire, set_state_batch) {
        let (frame, evs) = sequence_tick_with_events(&mut wire, sub);
        assert!(frame.is_some(), "SetState must emit DoDestinyUpdate");
        assert!(evs.is_empty(), "SetState produced unexpected sequencer drops: {evs:?}");
    }

    // Advance wire last_stamp ahead of current_tick via an FX-floored update (simulating Doomsday
    // OnSpecialFX clamped above ballpark.current_tick under TiDi).
    let fx_floor = init_tick + 5;
    let fx_evs = vec![DestinyEvent::OnSpecialFX {
        ball_id: ship_id,
        module_id: Some(9001),
        guid: "effects.DoomsdayBeam".to_string(),
        is_active: true,
    }];
    let fx_pyrep = eve_destiny::wire::encode_destiny_updates(fx_floor as u32, &fx_evs, None);
    let fx_batch = build_destiny_batch(
        &ctx,
        ep,
        init_tick,
        &fx_evs,
        fx_pyrep,
        &[],
        SelfDestinyWireContext::NonDock,
        None,
    )
    .expect("FX batch");
    assert_eq!(fx_batch.stamp, fx_floor as u32);
    for sub in normalize_server_destiny_batch(&mut wire, fx_batch) {
        let (frame, evs) = sequence_tick_with_events(&mut wire, sub);
        assert!(frame.is_some());
        assert!(evs.is_empty());
    }

    // Mirror actor `last_fx_stamp = fx_floor`, heal ship, flush peer/self updates while tick == init_tick.
    sys_handle
        .last_fx_stamp
        .fetch_max(fx_floor, std::sync::atomic::Ordering::AcqRel);
    let _ = sys_handle.heal_combat_state(ship_id).await.unwrap();
    let flushed = sys_handle.flush_peer_observer_updates(None).await.unwrap();
    assert!(flushed >= 1);

    let bcast = rx.try_recv().expect("flushed TickBroadcast");
    let dmg_evs = bcast
        .observer_events
        .get(&ship_id)
        .expect("ego OnDamageStateChange event");
    let dmg_pyrep = bcast
        .observer_updates_pyrep
        .get(&ship_id)
        .expect("ego OnDamageStateChange updates_pyrep")
        .clone();

    let dmg_batch = build_destiny_batch(
        &ctx,
        ep,
        bcast.tick,
        dmg_evs,
        dmg_pyrep,
        &[],
        SelfDestinyWireContext::NonDock,
        None,
    )
    .expect("OnDamageStateChange batch");
    assert_eq!(
        dmg_batch.stamp, fx_floor as u32,
        "build_destiny_batch must lift batch.stamp to uniform entry stamp fx_floor={fx_floor}"
    );

    let mut delivered_damage = false;
    for sub in normalize_server_destiny_batch(&mut wire, dmg_batch) {
        let (frame_opt, evs) = sequence_tick_with_events(&mut wire, sub);
        assert!(
            evs.is_empty(),
            "heal OnDamageStateChange must not trigger sequencer drop: {evs:?}"
        );
        if let Some(OutFrame::DoDestinyUpdate { stamp, updates, .. }) = frame_opt {
            assert_eq!(stamp, fx_floor as u32);
            if let PyRep::List(entries) = updates {
                for entry in &entries {
                    if let Some((Some(st), "OnDamageStateChange", Some(_))) =
                        crate::sequencer_invariants::parse_destiny_entry(entry)
                    {
                        assert_eq!(st, fx_floor as u32);
                        delivered_damage = true;
                    }
                }
            }
        }
    }
    assert!(delivered_damage, "OnDamageStateChange must be delivered, not dropped as stamp_regression");
}

#[tokio::test]
async fn test_1172_step_n_tick_broadcast_delivers_before_peer_set_state_n_plus_1_drain() {
    use crate::outbound_sequencer::OutFrame;
    use crate::router::client_wire::ClientWireSession;
    use crate::sequencer_invariants::{
        extract_add_balls_entities, extract_remove_balls_entities, parse_destiny_entry,
    };
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, CombatState, DestinyEvent, MovementMode};
    use glam::DVec3;

    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    crate::router::session_transition::clear_all_session_change_timers(&ctx);

    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let st_pos = ctx.sde.stations[&st_id].position;
    let ego_char_id = 90_000_001_i64;
    let ego_ship = 895_147_i64;
    let peer_char_id = 90_000_002_i64;
    let peer_ship = 895_148_i64;
    let entity_x = 895_149_i64;
    let entity_y = 895_150_i64;
    let tayra_type_id = 649_u32;

    let ego_pos = st_pos + DVec3::new(500_000_000.0, 0.0, 0.0);
    let peer_pos = ego_pos + DVec3::new(1_000.0, 0.0, 0.0);

    let ego_corp_id = ctx.db.with_mut_snapshot(|snap| {
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
        let ego_corp = setup_char(ego_char_id, ego_ship);
        let _peer_corp = setup_char(peer_char_id, peer_ship);
        ego_corp
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

    sys_handle.push_set_state(ego_ship).await.unwrap();
    let _ = sys_handle.drain_observer_updates(ego_ship).await;
    sys_handle.push_set_state(peer_ship).await.unwrap();
    let _ = sys_handle.drain_observer_updates(peer_ship).await;
    sys_handle.step_manual(1.0).await.unwrap();
    let _ = sys_handle.drain_observer_updates(ego_ship).await;
    let _ = sys_handle.drain_observer_updates(peer_ship).await;

    let mut ego_sess = SessionState::new(11_721);
    ego_sess.user_id = Some(1);
    ego_sess.character_id = Some(ego_char_id);
    ego_sess.corporation_id = Some(ego_corp_id);
    ego_sess.solar_system_id = Some(sys_id);
    ego_sess.ship_id = Some(ego_ship);
    ego_sess.live_notify_listener = true;
    ctx.db
        .acquire_online_lock(
            1,
            ego_sess.session_id,
            Some(ego_char_id),
            true,
            ctx.now_unix_secs(),
        )
        .unwrap();
    ctx.sync_session(&mut ego_sess);

    let mut ego_wire = ClientWireSession::from_session(&ctx, ego_sess);
    let ego_beyonce_bid = ctx
        .bind_object(&ego_wire.session, "beyonce", sys_id as i64)
        .expect("bind beyonce");
    let _ = sys_handle.drain_observer_updates(ego_ship).await;
    let _ = ego_wire.drain_pending_frames();
    let empty_kw = HashMap::new();

    // Part 1 (#1172):
    // 1. Spawn X and step tick N -> TickBroadcast(N) with AddBalls2(X) waits unread in ego's sys_rx.
    sys_handle
        .spawn_ball_with_combat(
            make_ship_ball(entity_x, ego_pos + DVec3::new(500.0, 0.0, 0.0)),
            full_combat.clone(),
        )
        .await
        .unwrap();
    let step_n = sys_handle.step_manual(1.0).await.unwrap();
    assert!(
        step_n
            .observer_events
            .get(&ego_ship)
            .is_some_and(|evs| evs.iter().any(|ev| matches!(
                ev,
                DestinyEvent::AddBalls2 { balls } if balls.iter().any(|b| b.id == entity_x)
            ))),
        "Step N TickBroadcast must carry AddBalls2({entity_x}) for ego observer"
    );

    // 2. Peer B drains SetState while ego's sys_rx still holds TickBroadcast(N); SetState must
    //    preserve `ballpark.current_tick == N` (#1169), then advancing Step N + 1 precedes ego's RPC drain.
    sys_handle.push_set_state(peer_ship).await.unwrap();
    let (tick_peer_drain, _, _) = sys_handle.drain_observer_updates(peer_ship).await.unwrap();
    assert_eq!(
        tick_peer_drain,
        step_n.tick,
        "peer SetState drain must not inflate ballpark current_tick (#1169)"
    );
    let step_np1 = sys_handle.step_manual(1.0).await.unwrap();
    let tick_np1 = step_np1.tick;
    assert_eq!(
        tick_np1,
        step_n.tick + 1,
        "1.0s physics step must advance ballpark tick from N to N + 1"
    );

    // 3. Ego executes an RPC (CmdSetSpeedFraction) which calls drain_observer_updates_with_seq at
    //    stamp N + 1; causal draining must deliver TickBroadcast(N) before stamp N + 1 updates
    //    without dropping Step N as a stamp regression.
    let rpc1 = ego_wire
        .call_wire(
            "beyonce",
            Some(ego_beyonce_bid),
            "CmdSetSpeedFraction",
            &[PyRep::Real(0.5)],
            &empty_kw,
        )
        .await;
    assert!(rpc1.rpc_result.is_ok(), "CmdSetSpeedFraction(0.5): {:?}", rpc1.rpc_result);
    assert!(rpc1.marshal_errors.is_empty(), "marshal errors: {:?}", rpc1.marshal_errors);

    let mut add_x_pos: Option<(usize, usize)> = None;
    let mut np1_speed_pos: Option<(usize, usize)> = None;
    let mut emitted_stamps_part1 = Vec::new();
    for (f_idx, frame) in rpc1.out_frames.iter().enumerate() {
        if let OutFrame::DoDestinyUpdate { stamp, updates, .. } = frame {
            emitted_stamps_part1.push(*stamp);
            if let PyRep::List(entries) = updates {
                for (e_idx, entry) in entries.iter().enumerate() {
                    if let Some((entry_stamp, action, args_opt)) = parse_destiny_entry(entry) {
                        if let Some(args) = args_opt {
                            if action == "AddBalls2" || action == "AddBalls" {
                                let mut ids = Vec::new();
                                extract_add_balls_entities(action, args, &mut ids);
                                if ids.contains(&entity_x) && entry_stamp == Some(step_n.tick as u32)
                                {
                                    add_x_pos = Some((f_idx, e_idx));
                                }
                            }
                        }
                        if action == "SetSpeedFraction" && entry_stamp == Some(tick_np1 as u32) {
                            np1_speed_pos = Some((f_idx, e_idx));
                        }
                    }
                }
            }
        }
    }
    assert!(
        add_x_pos.is_some(),
        "Step N AddBalls2({entity_x}) must be delivered before N+1 drain; stamps={emitted_stamps_part1:?}"
    );
    assert!(
        np1_speed_pos.is_some(),
        "Stamp N+1 SetSpeedFraction must be delivered; stamps={emitted_stamps_part1:?}"
    );
    assert!(
        add_x_pos.unwrap() < np1_speed_pos.unwrap(),
        "Step N AddBalls2({entity_x}) must precede stamp N+1 update ({add_x_pos:?} vs {np1_speed_pos:?})"
    );
    assert!(
        ego_wire.wire_state.materialized.contains(&entity_x),
        "entity_x must remain materialized after Step N delivery"
    );
    assert_eq!(
        ego_wire.wire_state.sequencer_counters().prevented_jolts,
        0,
        "TickBroadcast(N) must not be dropped as stamp regression"
    );
    assert_eq!(
        ego_wire.wire_state.sequencer_counters().invariant_violations,
        0
    );

    // Part 2 (#1172): Same-stamp Step N2 TickBroadcast AddBalls2(Y) followed by direct remove_ball(Y)
    // before session A's RPC drain must emit AddBalls2(Y) before RemoveBalls(Y), leaving neither
    // ghost entity nor invariant violation.
    sys_handle
        .spawn_ball_with_combat(
            make_ship_ball(entity_y, ego_pos + DVec3::new(750.0, 0.0, 0.0)),
            full_combat,
        )
        .await
        .unwrap();
    let step_n2 = sys_handle.step_manual(1.0).await.unwrap();
    assert!(
        step_n2
            .observer_events
            .get(&ego_ship)
            .is_some_and(|evs| evs.iter().any(|ev| matches!(
                ev,
                DestinyEvent::AddBalls2 { balls } if balls.iter().any(|b| b.id == entity_y)
            ))),
        "Step N2 TickBroadcast must carry AddBalls2({entity_y})"
    );
    let removed_y = sys_handle.remove_ball(entity_y).await;
    assert!(removed_y.is_some(), "entity_y must be present before remove_ball");

    let rpc2 = ego_wire
        .call_wire(
            "beyonce",
            Some(ego_beyonce_bid),
            "CmdSetSpeedFraction",
            &[PyRep::Real(0.25)],
            &empty_kw,
        )
        .await;
    assert!(rpc2.rpc_result.is_ok(), "CmdSetSpeedFraction(0.25): {:?}", rpc2.rpc_result);
    assert!(rpc2.marshal_errors.is_empty(), "marshal errors: {:?}", rpc2.marshal_errors);

    let mut add_y_pos: Option<(usize, usize)> = None;
    let mut rem_y_pos: Option<(usize, usize)> = None;
    for (f_idx, frame) in rpc2.out_frames.iter().enumerate() {
        if let OutFrame::DoDestinyUpdate { updates, .. } = frame {
            if let PyRep::List(entries) = updates {
                for (e_idx, entry) in entries.iter().enumerate() {
                    if let Some((entry_stamp, action, Some(args))) = parse_destiny_entry(entry) {
                        if action == "AddBalls2" || action == "AddBalls" {
                            let mut ids = Vec::new();
                            extract_add_balls_entities(action, args, &mut ids);
                            if ids.contains(&entity_y) && entry_stamp == Some(step_n2.tick as u32) {
                                add_y_pos = Some((f_idx, e_idx));
                            }
                        } else if action == "RemoveBalls" || action == "RemoveBall" {
                            let mut ids = Vec::new();
                            extract_remove_balls_entities(args, &mut ids);
                            if ids.contains(&entity_y) {
                                rem_y_pos = Some((f_idx, e_idx));
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        add_y_pos.is_some() && rem_y_pos.is_some(),
        "both AddBalls2({entity_y}) and RemoveBall*({entity_y}) must be emitted in causal order: add={add_y_pos:?}, rem={rem_y_pos:?}"
    );
    assert!(
        add_y_pos.unwrap() < rem_y_pos.unwrap(),
        "AddBalls2({entity_y}) must strictly precede RemoveBall*({entity_y})"
    );
    assert!(
        !ego_wire.wire_state.materialized.contains(&entity_y),
        "entity_y must not remain materialized after AddBalls2 -> RemoveBall*"
    );
    assert_eq!(ego_wire.wire_state.sequencer_counters().prevented_jolts, 0);
    assert_eq!(ego_wire.wire_state.sequencer_counters().invariant_violations, 0);
    assert_eq!(
        ego_wire
            .wire_state
            .sequencer_counters()
            .dropped_dependent_notifications,
        0
    );
}

#[test]
fn test_1172_drain_sys_rx_discarding_up_to_seq_continuity_and_lag() {
    use crate::system_actor::TickBroadcast;

    let ctx = Arc::new(ServerContext::new());
    let sys_id = 30_000_142_u32;
    let ship_id = 895_201_i64;

    let mk_tick = |tick: u64, broadcast_seq: u64| {
        let ev = eve_destiny::DestinyEvent::SetSpeedFraction {
            ball_id: ship_id,
            fraction: 0.5,
        };
        let mut observer_updates_pyrep = std::collections::BTreeMap::new();
        observer_updates_pyrep.insert(
            ship_id,
            PyRep::List(vec![eve_destiny::wire::encode_destiny_event(
                tick as u32,
                &ev,
                None,
            )]),
        );
        let mut observer_events = std::collections::BTreeMap::new();
        observer_events.insert(ship_id, vec![ev]);
        TickBroadcast {
            solar_system_id: sys_id,
            tick,
            broadcast_seq,
            events: Vec::new(),
            observer_events,
            observer_updates_pyrep,
        }
    };

    // Case 1 (HF1172-02a): Lag evicted only pre-watermark <= watermark_seq messages, leaving
    // earliest retained seq <= watermark_seq (buffer holds [3, 4, 5, 6] for watermark_seq = 4).
    // Continuity at watermark_seq + 1 is intact; post-watermark ticks [5, 6] must be preserved.
    let (tx1, mut rx1) = tokio::sync::broadcast::channel(4);
    for seq in 1..=6_u64 {
        let _ = tx1.send(mk_tick(100 + seq, seq));
    }
    let mut post1 = Vec::new();
    let loss1 = drain_sys_rx_discarding_up_to_seq(&ctx, &mut rx1, sys_id, &[ship_id], 1, 4, &mut post1);
    assert!(
        !loss1,
        "pre-watermark lag leaving earliest seq <= watermark_seq must preserve post-watermark ticks"
    );
    assert_eq!(
        post1.iter().map(|b| b.stamp).collect::<Vec<_>>(),
        vec![105, 106]
    );

    // Case 2 (HF1172-02a boundary): Lag evicted 1..=4 exactly, leaving earliest retained seq ==
    // watermark_seq + 1 (buffer holds [5, 6, 7, 8] for watermark_seq = 4). Zero post-watermark loss.
    let (tx2, mut rx2) = tokio::sync::broadcast::channel(4);
    for seq in 1..=8_u64 {
        let _ = tx2.send(mk_tick(200 + seq, seq));
    }
    let mut post2 = Vec::new();
    let loss2 = drain_sys_rx_discarding_up_to_seq(&ctx, &mut rx2, sys_id, &[ship_id], 1, 4, &mut post2);
    assert!(
        !loss2,
        "lag leaving earliest seq == watermark_seq + 1 has intact continuity"
    );
    assert_eq!(
        post2.iter().map(|b| b.stamp).collect::<Vec<_>>(),
        vec![205, 206, 207, 208]
    );

    // Case 3 (HF1172-02b): True post-watermark lag (seq 5 evicted; buffer holds [6, 7, 8, 9] for
    // watermark_seq = 4) breaks continuity at watermark_seq + 1 -> must report true and clear.
    let (tx3, mut rx3) = tokio::sync::broadcast::channel(4);
    for seq in 1..=9_u64 {
        let _ = tx3.send(mk_tick(300 + seq, seq));
    }
    let mut post3 = Vec::new();
    let loss3 = drain_sys_rx_discarding_up_to_seq(&ctx, &mut rx3, sys_id, &[ship_id], 1, 4, &mut post3);
    assert!(loss3, "evicting watermark_seq + 1 must report post-watermark loss");
    assert!(post3.is_empty(), "post_batches must be cleared on post-watermark loss");

    // Case 4 (HF1172-02b boundary with watermark_seq == 0): seq 1 evicted -> earliest 2 > 0 + 1.
    let (tx4, mut rx4) = tokio::sync::broadcast::channel(4);
    for seq in 1..=5_u64 {
        let _ = tx4.send(mk_tick(400 + seq, seq));
    }
    let mut post4 = Vec::new();
    let loss4 = drain_sys_rx_discarding_up_to_seq(&ctx, &mut rx4, sys_id, &[ship_id], 1, 0, &mut post4);
    assert!(loss4, "evicting seq 1 past watermark_seq == 0 must report post-watermark loss");
    assert!(post4.is_empty());
}

#[tokio::test]
async fn test_1172_caller_resyncs_when_drain_sys_rx_discarding_up_to_seq_reports_post_loss() {
    use crate::outbound_sequencer::OutFrame;
    use crate::router::client_wire::ClientWireSession;
    use crate::sequencer_invariants::parse_destiny_entry;
    use crate::system_actor::TickBroadcast;
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, CombatState, MovementMode};
    use glam::DVec3;

    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    crate::router::session_transition::clear_all_session_change_timers(&ctx);

    let sys_id = 30_000_142_u32;
    let st_pos = ctx.sde.stations[&60_003_760_i64].position;
    let ego_char_id = 90_000_001_i64;
    let ego_ship = 895_301_i64;
    let tayra_type_id = 649_u32;
    let ego_pos = st_pos + DVec3::new(500_000_000.0, 0.0, 0.0);

    let corp_id = ctx.db.with_mut_snapshot(|snap| {
        let corp_id = snap
            .characters
            .get_mut(&ego_char_id)
            .map(|ch| {
                ch.active_ship_id = ego_ship;
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.corporation_id
            })
            .expect("seeded character");
        snap.items.insert(
            ego_ship,
            InventoryItemRecord {
                item_id: ego_ship,
                type_id: tayra_type_id,
                owner_id: ego_char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        corp_id
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ego_ship,
                type_id: tayra_type_id,
                position: ego_pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_100_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    let mut sess = SessionState::new(11_722);
    sess.user_id = Some(1);
    sess.character_id = Some(ego_char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ego_ship);
    sess.live_notify_listener = true;
    ctx.db
        .acquire_online_lock(1, sess.session_id, Some(ego_char_id), true, ctx.now_unix_secs())
        .unwrap();
    ctx.sync_session(&mut sess);

    let mut ego_wire = ClientWireSession::from_session(&ctx, sess);
    // Simulate AwaitingSetState before first SetState arrives, with a stale SetState already
    // queued on observer `ego_ship`, followed by >4096 post-watermark broadcasts overflowing sys_rx.
    let ep = ego_wire.wire_state.current_epoch();
    ego_wire.wire_state.destiny = DestinyStream::AwaitingSetState { epoch: ep };
    let beyonce_bid = ctx
        .bind_object(&ego_wire.session, "beyonce", sys_id as i64)
        .expect("bind beyonce");
    sys_handle.push_set_state(ego_ship).await.unwrap();

    for seq in 2..=4_105_u64 {
        let _ = sys_handle.event_tx.send(TickBroadcast {
            solar_system_id: sys_id,
            tick: seq,
            broadcast_seq: seq,
            events: Vec::new(),
            observer_events: std::collections::BTreeMap::new(),
            observer_updates_pyrep: std::collections::BTreeMap::new(),
        });
    }

    let empty_kw = HashMap::new();
    let rpc = ego_wire
        .call_wire(
            "beyonce",
            Some(beyonce_bid),
            "CmdSetSpeedFraction",
            &[PyRep::Real(0.5)],
            &empty_kw,
        )
        .await;
    assert!(rpc.rpc_result.is_ok(), "RPC failed: {:?}", rpc.rpc_result);
    assert!(rpc.marshal_errors.is_empty(), "marshal errors: {:?}", rpc.marshal_errors);
    assert!(
        matches!(ego_wire.wire_state.destiny, DestinyStream::Live { .. }),
        "stream must resync SetState and attach Live after post-watermark lag, got {:?}",
        ego_wire.wire_state.destiny
    );

    let mut saw_resynced_set_state = false;
    let mut actions = Vec::new();
    for frame in &rpc.out_frames {
        if let OutFrame::DoDestinyUpdate { updates, .. } = frame {
            if let PyRep::List(entries) = updates {
                for entry in entries {
                    if let Some((_, action, _)) = parse_destiny_entry(entry) {
                        if action == "SetState" {
                            saw_resynced_set_state = true;
                        }
                        actions.push(action.to_string());
                    }
                }
            }
        }
    }
    assert!(saw_resynced_set_state, "resynced DoDestinyUpdate must contain SetState");
    assert_eq!(
        actions,
        vec!["SetState".to_string()],
        "resync after post_lagged must discard stale SetState+SetSpeedFraction snapshot and emit fresh SetState only"
    );
}

#[tokio::test]
async fn test_1172_notify_rx_ship_swap_broadcast_lagged_triggers_set_state_resync() {
    use crate::outbound_sequencer::{
        normalize_server_destiny_batch, sequence_tick_with_events, OutFrame, SessionWireState,
    };
    use crate::sequencer_invariants::parse_destiny_entry;
    use crate::system_actor::TickBroadcast;
    use eve_destiny::{Ball, CombatState, MovementMode};
    use glam::DVec3;

    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_142_u32;
    let st_pos = ctx.sde.stations[&60_003_760_i64].position;
    let old_ship = 895_401_i64;
    let new_pod = 895_402_i64;
    let tayra_type_id = 649_u32;
    let pod_type_id = 670_u32;
    let base_pos = st_pos + DVec3::new(500_000_000.0, 0.0, 0.0);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mk_ball = |id: i64, type_id: u32, offset_x: f64| Ball {
        id,
        type_id,
        position: base_pos + DVec3::new(offset_x, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_100_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    sys_handle
        .spawn_ball_with_combat(mk_ball(old_ship, tayra_type_id, 0.0), CombatState::default())
        .await
        .unwrap();
    sys_handle
        .spawn_ball_with_combat(mk_ball(new_pod, pod_type_id, 150.0), CombatState::default())
        .await
        .unwrap();
    sys_handle.push_set_state(old_ship).await.unwrap();
    let _ = sys_handle.drain_observer_updates(old_ship).await;

    // Live session in the same solar system before ship swap (`is_live_same_system` initially true).
    let mut sys_rx = sys_handle.subscribe();
    let mut wire_state = SessionWireState::new(11_723, 911_723);
    let ep = wire_state.enter_space(Some(sys_id));
    wire_state.destiny = DestinyStream::Live {
        epoch: ep,
        set_state_stamp: 100,
        last_stamp: 100,
    };
    wire_state.materialized.insert(old_ship);

    // Queue an incremental handoff (RemoveBall + AddBalls2, i.e., `has_set_state == false`) onto
    // `new_pod`, matching in-space pod ejection / ship swap prior to `notify_rx` firing.
    assert!(
        sys_handle.try_handoff_in_space_observer(old_ship, new_pod, &[new_pod]),
        "handoff must queue incremental updates on new_pod"
    );

    // Overflow `sys_rx` past tokio broadcast capacity (4096) so the pre-watermark drain hits
    // `TryRecvError::Lagged` and reverts `wire_state.destiny` to `AwaitingSetState`.
    for seq in 1..=4_105_u64 {
        let _ = sys_handle.event_tx.send(TickBroadcast {
            solar_system_id: sys_id,
            tick: seq,
            broadcast_seq: seq,
            events: Vec::new(),
            observer_events: std::collections::BTreeMap::new(),
            observer_updates_pyrep: std::collections::BTreeMap::new(),
        });
    }

    let sync = sync_in_space_ship_swap_destiny(
        &ctx,
        &mut sys_rx,
        &mut wire_state,
        11_723,
        sys_id,
        new_pod,
        Some(old_ship),
        true,
    )
    .await
    .expect("ship swap sync outcome");

    assert!(
        sync.pre_batches.is_empty(),
        "lagged pre-watermark batches must be discarded"
    );
    let direct_batch = sync
        .direct_batch
        .expect("HF1172-01: lagged ship swap must synthesize a resync SetState batch");
    assert!(
        direct_batch.has_set_state,
        "HF1172-01: resync direct batch must contain SetState when sys_rx lagged"
    );

    let mut emitted_set_state = false;
    for sub in normalize_server_destiny_batch(&mut wire_state, direct_batch) {
        let (frame_opt, _evs) = sequence_tick_with_events(&mut wire_state, sub);
        if let Some(OutFrame::DoDestinyUpdate { updates, .. }) = frame_opt {
            if let PyRep::List(entries) = updates {
                for entry in &entries {
                    if let Some((_, "SetState", _)) = parse_destiny_entry(entry) {
                        emitted_set_state = true;
                    }
                }
            }
        }
    }
    for post in sync.post_batches {
        for sub in normalize_server_destiny_batch(&mut wire_state, post) {
            let _ = sequence_tick_with_events(&mut wire_state, sub);
        }
    }

    assert!(emitted_set_state, "must emit DoDestinyUpdate(SetState) before SessionChange");
    assert!(
        matches!(wire_state.destiny, DestinyStream::Live { .. }),
        "session must recover to Live rather than stranding in AwaitingSetState, got {:?}",
        wire_state.destiny
    );
}

#[tokio::test]
async fn displaced_macho_session_does_not_steal_or_clear_replacement_notifications() {
    use crate::services::space_warp_jump::{set_pre_take_ball_hook_for_test, PreTakeBallHook};
    use tokio::sync::oneshot;

    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.owner_id = char_id;
            sh.location_id = i64::from(sys_id);
            sh.flag = eve_db::InventoryFlag::None;
        }
    });
    let gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id)
        .cloned()
        .expect("gate from 30000142 in SDE");
    let addr = spawn_test_listener(Arc::clone(&ctx)).await;

    let make_call = |cid: u64, uid: u64, call_id: u64, service: &str, method: &str, args: Vec<PyRep>| {
        MachoPacket {
            packet_type: MachoPacketType::CallReq,
            source: MachoAddress::Client {
                client_id: cid,
                call_id,
                service: None,
            },
            destination: MachoAddress::Any {
                service: Some(service.to_string()),
                call_id: 0,
            },
            user_id: Some(uid),
            payload: PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Integer(0),
                PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                    PyRep::ascii_bytes(method),
                    PyRep::Tuple(args),
                    PyRep::Dict(Vec::new()),
                ]))),
            ])]),
            named_payload: HashMap::new(),
        }
    };
    let (mut s1, client_id_1, user_id) = login_client(addr, "demo", "demo-secret", "DE").await;
    write_framed_pyrep(
        &mut s1,
        &make_call(
            client_id_1,
            user_id,
            1,
            "charUnboundMgr",
            "SelectCharacterID",
            vec![PyRep::Integer(char_id)],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let mut got_rsp_1 = false;
    let mut got_sc_1 = false;
    while !got_rsp_1 || !got_sc_1 {
        let pkt = MachoPacket::from_pyrep(
            &tokio::time::timeout(
                std::time::Duration::from_secs(10),
                read_framed_pyrep(&mut s1),
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        )
        .unwrap();
        if pkt.packet_type == MachoPacketType::CallRsp {
            got_rsp_1 = true;
        } else if pkt.packet_type == MachoPacketType::SessionChangeNotification {
            got_sc_1 = true;
        }
    }

    ctx.advance_secs(12);
    write_framed_pyrep(
        &mut s1,
        &make_call(
            client_id_1,
            user_id,
            2,
            "beyonce",
            "MachoBindObject",
            vec![
                PyRep::Integer(i64::from(sys_id)),
                PyRep::Tuple(vec![
                    PyRep::ascii_bytes("GetFormations"),
                    PyRep::Tuple(Vec::new()),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    loop {
        let pkt = MachoPacket::from_pyrep(
            &tokio::time::timeout(
                std::time::Duration::from_secs(10),
                read_framed_pyrep(&mut s1),
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        )
        .unwrap();
        if pkt.packet_type == MachoPacketType::CallRsp {
            break;
        }
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert!(
        sys_handle
            .set_ball_position(
                ship_id,
                gate.position + eve_destiny::DVec3::new(1_000.0, 0.0, 0.0),
            )
            .await
    );
    ctx.advance_secs(12);

    // Pause S1 mid-RPC inside CmdStargateJump so S1 stays live across S2 login + notify.
    let (paused_tx, paused_rx) = oneshot::channel::<()>();
    let (resume_tx, resume_rx) = oneshot::channel::<()>();
    let hook: PreTakeBallHook = Box::new(move || {
        Box::pin(async move {
            let _ = paused_tx.send(());
            let _ = resume_rx.await;
        })
    });
    set_pre_take_ball_hook_for_test(&ctx, char_id, Some(hook));
    write_framed_pyrep(
        &mut s1,
        &make_call(
            client_id_1,
            user_id,
            3,
            "beyonce",
            "CmdStargateJump",
            vec![PyRep::Integer(gate.stargate_id)],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), paused_rx)
        .await
        .expect("S1 paused inside CmdStargateJump")
        .expect("paused_tx");

    // S2 logs in on the same account (displacing S1) and selects char_id while S1 is paused in-flight.
    let (mut s2, client_id_2, user_id_2) = login_client(addr, "demo", "demo-secret", "DE").await;
    assert_eq!(user_id_2, user_id);
    write_framed_pyrep(
        &mut s2,
        &make_call(
            client_id_2,
            user_id,
            1,
            "charUnboundMgr",
            "SelectCharacterID",
            vec![PyRep::Integer(char_id)],
        )
        .to_pyrep(),
    )
    .await
    .unwrap();
    let mut got_rsp_2 = false;
    let mut got_sc_2 = false;
    while !got_rsp_2 || !got_sc_2 {
        let pkt = MachoPacket::from_pyrep(
            &tokio::time::timeout(
                std::time::Duration::from_secs(10),
                read_framed_pyrep(&mut s2),
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        )
        .unwrap();
        if pkt.packet_type == MachoPacketType::CallRsp {
            got_rsp_2 = true;
        } else if pkt.packet_type == MachoPacketType::SessionChangeNotification {
            got_sc_2 = true;
        }
    }

    // Push a character notification WHILE S1 is still live inside execute_call_req_wire, then resume S1.
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnAccountChange",
        "charid",
        vec![
            PyRep::ascii_bytes("cash"),
            PyRep::Integer(1000),
            PyRep::Real(9_999_000.0),
        ],
    );
    let _ = resume_tx.send(());

    let mut s1_stole_account_change = false;
    let mut s1_got_error_rsp = false;
    while let Ok(Ok(Some(rep))) =
        tokio::time::timeout(std::time::Duration::from_secs(5), read_framed_pyrep(&mut s1)).await
    {
        if let Some(pkt) = MachoPacket::from_pyrep(&rep) {
            if pkt.packet_type == MachoPacketType::ErrorResponse {
                s1_got_error_rsp = true;
            }
            if pkt.packet_type == MachoPacketType::Notification {
                if let MachoAddress::Broadcast { broadcast_id, .. } = &pkt.destination {
                    if broadcast_id == "OnAccountChange" {
                        s1_stole_account_change = true;
                    }
                }
            }
        }
    }
    assert!(s1_got_error_rsp, "paused S1 RPC must unwind cleanly with ErrorResponse");
    assert!(
        !s1_stole_account_change,
        "live displaced S1 resuming from RPC must never steal replacement S2's OnAccountChange notification"
    );

    loop {
        let pkt = MachoPacket::from_pyrep(
            &tokio::time::timeout(
                std::time::Duration::from_secs(10),
                read_framed_pyrep(&mut s2),
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        )
        .unwrap();
        if pkt.packet_type == MachoPacketType::Notification {
            if let MachoAddress::Broadcast { broadcast_id, .. } = &pkt.destination {
                if broadcast_id == "OnAccountChange" {
                    break;
                }
            }
        }
    }
}

#[tokio::test]
async fn execute_call_req_wire_displaced_in_space_preserves_observer_destiny_updates_for_replacement() {
    use crate::outbound_sequencer::{DestinyStream, OutFrame, SessionWireState};

    let ctx = ServerContext::new();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;
    let s1_id = 91_301_u64;
    let s2_id = 91_302_u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = user_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.owner_id = char_id;
            sh.location_id = i64::from(sys_id);
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: eve_destiny::DVec3::new(100_000.0, 0.0, 0.0),
            velocity: eve_destiny::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn ship ball");

    // S1 was previously live in space, then S2 displaced S1 and reclaimed char_id.
    ctx.db
        .acquire_online_lock(user_id, s1_id, Some(char_id), true, ctx.now_unix())
        .expect("s1 acquires lock");
    ctx.db
        .acquire_online_lock(user_id, s2_id, Some(char_id), true, ctx.now_unix())
        .expect("s2 displaces s1 and owns char_id");

    // Queue a pending SetState observer event (intended for S2's space bootstrap/resync).
    sys_handle.push_set_state(ship_id).await.unwrap();

    let mut sess_s1 = SessionState::new(s1_id);
    sess_s1.user_id = Some(user_id);
    sess_s1.character_id = Some(char_id);
    sess_s1.ship_id = Some(ship_id);
    sess_s1.solar_system_id = Some(sys_id);

    let mut sys_rx = sys_handle.subscribe();
    let mut subscribed_sys_id = Some(sys_id);
    let mut wire_state = SessionWireState::new(s1_id, 91_301);
    let ep = wire_state.enter_space(Some(sys_id));
    wire_state.destiny = DestinyStream::Live {
        epoch: ep,
        set_state_stamp: 100,
        last_stamp: 100,
    };
    let mut deferred_ship_id = None;
    let mut wire_refs = CallReqWireRefs {
        client_id: 91_301,
        sys_rx: &mut sys_rx,
        subscribed_sys_id: &mut subscribed_sys_id,
        wire_state: &mut wire_state,
        deferred_ship_id: &mut deferred_ship_id,
    };

    let req_source = MachoAddress::Client {
        client_id: 91_301,
        call_id: 17,
        service: None,
    };
    let req_dest = MachoAddress::Node {
        node_id: eve_machonet::MACHO_PROXY_NODE_ID,
        service: Some("beyonce".to_string()),
        call_id: 0,
    };
    let kwargs = HashMap::new();
    let outcome = execute_call_req_wire(
        &ctx,
        &mut sess_s1,
        &mut wire_refs,
        &req_source,
        &req_dest,
        "beyonce",
        None,
        "GetFormations",
        &[],
        &kwargs,
    )
    .await;

    assert!(
        !outcome
            .out_frames
            .iter()
            .any(|f| matches!(f, OutFrame::DoDestinyUpdate { .. })),
        "BF-04: displaced S1 must not emit DoDestinyUpdate observer batches: {:?}",
        outcome.out_frames
    );

    // S2 must still find the queued SetState observer update untouched.
    let (_tick, obs_evs, _pyrep, _seq) = sys_handle
        .drain_observer_updates_with_seq(ship_id)
        .await
        .expect("drain observer updates for S2");
    assert!(
        obs_evs
            .iter()
            .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. })),
        "BF-04: displaced S1 RPC completion must leave replacement S2's observer queue intact"
    );
}

#[tokio::test]
async fn notify_rx_displaced_session_preserves_notifications_and_observer_updates_for_replacement() {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.owner_id = char_id;
            sh.location_id = i64::from(sys_id);
            sh.flag = eve_db::InventoryFlag::None;
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: eve_destiny::DVec3::new(100_000.0, 0.0, 0.0),
            velocity: eve_destiny::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn ball");

    let addr = spawn_test_listener(Arc::clone(&ctx)).await;
    let (mut s1, client_id_1, user_id) = login_client(addr, "demo", "demo-secret", "DE").await;
    let select_call = MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client {
            client_id: client_id_1,
            call_id: 1,
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
                PyRep::Tuple(vec![PyRep::Integer(char_id)]),
                PyRep::Dict(Vec::new()),
            ]))),
        ])]),
        named_payload: HashMap::new(),
    };
    write_framed_pyrep(&mut s1, &select_call.to_pyrep())
        .await
        .unwrap();
    let mut got_rsp = false;
    let mut got_sc = false;
    while !got_rsp || !got_sc {
        let pkt = MachoPacket::from_pyrep(
            &tokio::time::timeout(
                std::time::Duration::from_secs(10),
                read_framed_pyrep(&mut s1),
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
            got_sc = true;
        }
    }

    // Drain post-SelectCharacterID client notifications (e.g. OnChannelJoined) before displacing S1.
    while let Ok(Ok(Some(_))) = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        read_framed_pyrep(&mut s1),
    )
    .await
    {}

    // Displace S1's online lock with S2 directly (without closing S1 yet, isolating the notify_rx race)
    // and transition char_id in DB + push SetState & OnAccountChange intended for S2.
    ctx.db
        .acquire_online_lock(user_id, 99_991, Some(char_id), true, ctx.now_unix())
        .expect("S2 claims lock");
    sys_handle.push_set_state(ship_id).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
        }
    });
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnSessionChanged",
        "charid",
        vec![],
    );
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnAccountChange",
        "charid",
        vec![
            PyRep::ascii_bytes("cash"),
            PyRep::Integer(1000),
            PyRep::Real(8_888_000.0),
        ],
    );

    // Give S1's listener task a moment to wake on notify_rx and process any guarded branches.
    let leaked_frame = tokio::time::timeout(
        std::time::Duration::from_millis(150),
        read_framed_pyrep(&mut s1),
    )
    .await;
    assert!(
        leaked_frame.is_err(),
        "P1-1: displaced S1 must not emit SessionChangeNotification or OnAccountChange: {leaked_frame:?}"
    );
    assert_eq!(
        ctx.drain_notifications_for_char(char_id).len(),
        2,
        "P1-1: displaced S1 notify_rx must leave survivor's queued notifications intact"
    );
    let (_tick, obs_evs, _pyrep, _seq) = sys_handle
        .drain_observer_updates_with_seq(ship_id)
        .await
        .expect("drain observer updates for survivor");
    assert!(
        obs_evs
            .iter()
            .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. })),
        "P1-1: displaced S1 notify_rx must leave survivor's observer updates intact"
    );
}

#[test]
fn test_1531_canonical_ballpark_and_sequencer_emit_remove_balls() {
    use crate::outbound_sequencer::{normalize_server_destiny_batch, sequence_tick, DestinyBatch, OutFrame, SessionWireState};
    use eve_destiny::wire::encode_destiny_updates;
    use eve_destiny::{Ball, Ballpark, DestinyEvent, MovementMode};
    use glam::DVec3;

    let ctx = ServerContext::new();
    let observer_ship_id = 1_897_000_001_i64;
    let new_ship_id = 1_897_000_002_i64;
    let npc_id = 900_000_002_i64;
    let drone_id = 1_898_125_074_i64;
    let scout_id = 1_898_125_075_i64;

    let make_ball = |id: i64, x: f64| Ball {
        id,
        type_id: 587,
        position: DVec3::new(x, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };

    // 1. Canonical `Ballpark::remove_ball` queues `DestinyEvent::RemoveBalls([id])` to
    //    observers so both `:26000` (`build_destiny_batch`) and bridge (`observer_updates_pyrep`)
    //    emit `RemoveBalls([id])` instead of 1-arg `RemoveBall`.
    let mut bp = Ballpark::new(30_000_142);
    bp.current_tick = 100;
    bp.add_ball(make_ball(observer_ship_id, 0.0));
    bp.add_ball(make_ball(npc_id, 1_000.0));
    bp.add_ball(make_ball(drone_id, 500.0));
    bp.add_ball(make_ball(scout_id, 600.0));
    bp.queue_set_state(observer_ship_id);
    bp.drain_all_observer_events();

    bp.emit_event(
        Some(npc_id),
        DestinyEvent::OnSpecialFXTimed {
            ball_id: npc_id,
            module_id: None,
            target_id: Some(drone_id),
            guid: "effects.HybridWeapon".into(),
            start: true,
            is_active: true,
            duration_ms: Some(2_000.0),
            repeat: Some(1),
            graphic_info: None,
        },
    );
    bp.emit_event(
        Some(drone_id),
        DestinyEvent::TerminalExplosion {
            ball_id: drone_id,
            destruction_effect_id: None,
        },
    );
    assert!(bp.remove_ball(drone_id).is_some());
    assert!(bp.remove_ball(scout_id).is_some());

    let obs_evs = bp.drain_observer_events(observer_ship_id);
    let bridge_pyrep = encode_destiny_updates(100, &obs_evs, Some(&bp));
    let bridge_actions = collect_destiny_actions_from_list(&bridge_pyrep);
    assert_eq!(
        bridge_actions.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        vec![
            "OnSpecialFX",
            "TerminalPlayDestructionEffect",
            "RemoveBalls",
            "RemoveBalls",
        ]
    );
    assert_eq!(bridge_actions[2].1, vec![PyRep::List(vec![PyRep::Integer(drone_id)])]);
    assert_eq!(bridge_actions[3].1, vec![PyRep::List(vec![PyRep::Integer(scout_id)])]);

    let batch = build_destiny_batch(
        &ctx,
        1,
        100,
        &obs_evs,
        bridge_pyrep,
        &[observer_ship_id],
        SelfDestinyWireContext::NonDock,
        None,
    )
    .expect("destruction batch must not collapse");
    assert_eq!(
        collect_destiny_actions_from_list(&batch.updates),
        bridge_actions,
        "MachoNet :26000 and bridge wire updates must match identically (#1531)"
    );

    // 2. Canonical `Ballpark::queue_in_space_ship_handoff` schedules `RemoveBalls([old_ship_id])`
    //    for tick +2 without `SetState`.
    bp.add_ball(make_ball(new_ship_id, 50.0));
    assert!(bp.queue_in_space_ship_handoff(observer_ship_id, new_ship_id, &[new_ship_id]));
    let immediate_handoff = bp.drain_observer_events(new_ship_id);
    assert!(!immediate_handoff
        .iter()
        .any(|e| matches!(e, DestinyEvent::RemoveBall { .. } | DestinyEvent::RemoveBalls { .. })));
    bp.step(1.0);
    assert!(!bp
        .drain_observer_events(new_ship_id)
        .iter()
        .any(|e| matches!(e, DestinyEvent::RemoveBall { .. } | DestinyEvent::RemoveBalls { .. })));
    bp.step(1.0);
    let handoff_tick2 = bp.drain_observer_events(new_ship_id);
    assert!(handoff_tick2.iter().any(
        |e| matches!(e, DestinyEvent::RemoveBalls { ball_ids } if ball_ids == &[observer_ship_id])
    ));
    let handoff_pyrep = encode_destiny_updates(bp.current_tick as u32, &handoff_tick2, Some(&bp));
    assert_eq!(
        collect_destiny_actions_from_list(&handoff_pyrep),
        vec![(
            "RemoveBalls".into(),
            vec![PyRep::List(vec![PyRep::Integer(observer_ship_id)])],
        )]
    );

    // 3. `OutboundSequencer` deferred removal always emits `RemoveBalls([id])`, never `RemoveBall`.
    let mut wire = SessionWireState::new(98_153_101, 9101);
    let ep = wire.enter_space(Some(30_000_142));
    assert!(sequence_tick(
        &mut wire,
        DestinyBatch::new(
            ep,
            10,
            true,
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(10),
                PyRep::Tuple(vec![
                    PyRep::String("SetState".into()),
                    PyRep::Tuple(vec![PyRep::keyval([("ego", PyRep::Integer(observer_ship_id))])]),
                ]),
            ])]),
        ),
    )
    .is_some());
    let deferred = normalize_server_destiny_batch(
        &mut wire,
        DestinyBatch::new(
            ep,
            11,
            false,
            PyRep::List(vec![
                PyRep::Tuple(vec![
                    PyRep::Integer(11),
                    PyRep::Tuple(vec![
                        PyRep::String("AddBalls2".into()),
                        PyRep::Tuple(vec![PyRep::Integer(scout_id)]),
                    ]),
                ]),
                PyRep::Tuple(vec![
                    PyRep::Integer(11),
                    PyRep::Tuple(vec![
                        PyRep::String("RemoveBalls".into()),
                        PyRep::Tuple(vec![PyRep::List(vec![PyRep::Integer(scout_id)])]),
                    ]),
                ]),
            ]),
        ),
    );
    assert_eq!(deferred.len(), 2);
    assert_eq!(deferred[1].stamp, 12);
    assert_eq!(
        collect_destiny_actions_from_list(&deferred[1].updates),
        vec![(
            "RemoveBalls".into(),
            vec![PyRep::List(vec![PyRep::Integer(scout_id)])],
        )]
    );
    let mut deferred_iter = deferred.into_iter();
    assert!(matches!(
        sequence_tick(&mut wire, deferred_iter.next().unwrap()),
        Some(OutFrame::DoDestinyUpdate { stamp: 11, .. })
    ));
    let out = sequence_tick(&mut wire, deferred_iter.next().unwrap());
    match out {
        Some(OutFrame::DoDestinyUpdate { stamp: 12, updates, .. }) => {
            assert_eq!(
                collect_destiny_actions_from_list(&updates),
                vec![(
                    "RemoveBalls".into(),
                    vec![PyRep::List(vec![PyRep::Integer(scout_id)])],
                )]
            );
        }
        other => panic!("expected sequenced DoDestinyUpdate at stamp 12, got {other:?}"),
    }
}

#[tokio::test]
async fn test_1531_wire_live_ball_removal_and_destruction_with_active_fx_emit_remove_balls() {
    use crate::router::client_wire::ClientWireSession;
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, CombatState, DestinyEvent, MovementMode};
    use glam::DVec3;

    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    crate::router::session_transition::clear_all_session_change_timers(&ctx);

    let sys_id = 30_000_142_u32;
    let st_pos = ctx.sde.stations[&60_003_760_i64].position;
    let ego_char_id = 90_000_001_i64;
    let ego_ship = 895_251_i64;
    let npc_attacker = 900_125_001_i64;
    let drone_target = 1_898_125_074_i64;
    let scooped_drone = 1_898_125_075_i64;
    let tayra_type_id = 649_u32;
    let ego_pos = st_pos + DVec3::new(500_000_000.0, 0.0, 0.0);

    let ego_corp_id = ctx.db.with_mut_snapshot(|snap| {
        let corp_id = snap
            .characters
            .get_mut(&ego_char_id)
            .map(|ch| {
                ch.active_ship_id = ego_ship;
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.corporation_id
            })
            .expect("seeded character");
        snap.items.insert(
            ego_ship,
            InventoryItemRecord {
                item_id: ego_ship,
                type_id: tayra_type_id,
                owner_id: ego_char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        corp_id
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
    let make_ball = |id: i64, offset: DVec3| Ball {
        id,
        type_id: tayra_type_id,
        position: ego_pos + offset,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_100_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };

    sys_handle
        .spawn_ball_with_combat(make_ball(ego_ship, DVec3::ZERO), full_combat.clone())
        .await
        .unwrap();
    sys_handle.push_set_state(ego_ship).await.unwrap();
    let _ = sys_handle.drain_observer_updates(ego_ship).await;
    sys_handle.step_manual(1.0).await.unwrap();
    let _ = sys_handle.drain_observer_updates(ego_ship).await;

    let mut ego_sess = SessionState::new(15_311);
    ego_sess.user_id = Some(1);
    ego_sess.character_id = Some(ego_char_id);
    ego_sess.corporation_id = Some(ego_corp_id);
    ego_sess.solar_system_id = Some(sys_id);
    ego_sess.ship_id = Some(ego_ship);
    ego_sess.live_notify_listener = true;
    ctx.db
        .acquire_online_lock(
            1,
            ego_sess.session_id,
            Some(ego_char_id),
            true,
            ctx.now_unix_secs(),
        )
        .unwrap();
    ctx.sync_session(&mut ego_sess);

    let mut ego_wire = ClientWireSession::from_session(&ctx, ego_sess);
    let _ = ego_wire.drain_pending_frames();

    // 1. Spawn and materialize attacker + two drones on ego's live wire session.
    sys_handle
        .spawn_ball_with_combat(
            make_ball(npc_attacker, DVec3::new(1_000.0, 0.0, 0.0)),
            full_combat.clone(),
        )
        .await
        .unwrap();
    sys_handle
        .spawn_ball_with_combat(
            make_ball(drone_target, DVec3::new(500.0, 0.0, 0.0)),
            full_combat.clone(),
        )
        .await
        .unwrap();
    sys_handle
        .spawn_ball_with_combat(
            make_ball(scooped_drone, DVec3::new(600.0, 0.0, 0.0)),
            full_combat,
        )
        .await
        .unwrap();
    let _step_spawn = sys_handle.step_manual(1.0).await.unwrap();
    let _ = ego_wire.drain_pending_frames();
    assert!(ego_wire.wire_state.materialized.contains(&drone_target));
    assert!(ego_wire.wire_state.materialized.contains(&scooped_drone));

    // 2. Emit targeted NPC weapon FX alongside TerminalExplosion + remove_ball(drone_target) and
    //    verify the MachoNet wire frame delivers RemoveBalls([drone_target]) instead of RemoveBall.
    assert!(
        sys_handle
            .emit_destiny_event(
                npc_attacker,
                DestinyEvent::OnSpecialFXTimed {
                    ball_id: npc_attacker,
                    module_id: None,
                    target_id: Some(drone_target),
                    guid: "effects.HybridWeapon".into(),
                    start: true,
                    is_active: true,
                    duration_ms: Some(2_000.0),
                    repeat: Some(1),
                    graphic_info: None,
                },
            )
            .await
    );
    assert!(
        sys_handle
            .emit_destiny_event(
                drone_target,
                DestinyEvent::TerminalExplosion {
                    ball_id: drone_target,
                    destruction_effect_id: None,
                },
            )
            .await
    );
    assert!(sys_handle.remove_ball(drone_target).await.is_some());
    let step_destroy = sys_handle.step_manual(1.0).await.unwrap();
    let (destroy_frames, _) = ego_wire.drain_pending_frames();
    let destroy_actions = collect_destiny_actions_from_frames(&destroy_frames);

    assert!(
        !destroy_actions.iter().any(|(name, _)| name == "RemoveBall"),
        "MachoNet Destiny wire must never emit 1-arg RemoveBall (#1531): {destroy_actions:?}"
    );
    assert!(
        destroy_actions.iter().any(|(name, args)| {
            name == "OnSpecialFX" && args.get(3).and_then(PyRep::as_i64) == Some(drone_target)
        }),
        "targeted NPC FX must remain in tick batch: {destroy_actions:?}"
    );
    assert!(
        destroy_actions.iter().any(|(name, args)| {
            name == "TerminalPlayDestructionEffect"
                && args.first().and_then(PyRep::as_i64) == Some(drone_target)
        }),
        "TerminalPlayDestructionEffect must remain paired with RemoveBalls: {destroy_actions:?}"
    );
    assert!(
        destroy_actions.iter().any(|(name, args)| {
            name == "RemoveBalls"
                && matches!(
                    args.first(),
                    Some(PyRep::List(ids)) if ids.iter().filter_map(PyRep::as_i64).collect::<Vec<_>>() == vec![drone_target]
                )
        }),
        "dying ball must be emitted via RemoveBalls([drone_target]): {destroy_actions:?}"
    );
    assert_eq!(
        ego_wire.wire_state.sequencer_counters().invariant_violations,
        0
    );
    assert!(!ego_wire.wire_state.materialized.contains(&drone_target));

    // 3. Also verify canonical empty-jetcan despawn (`inventory_stacking::despawn_empty_jetcan_if_needed`)
    //    emits `DoDestinyUpdate` with `RemoveBalls([scooped_drone])` directly on the wire.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            scooped_drone,
            InventoryItemRecord {
                item_id: scooped_drone,
                type_id: 23,
                owner_id: ego_char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert!(
        crate::services::inventory_stacking::despawn_empty_jetcan_if_needed(
            &ctx,
            Some(sys_id),
            scooped_drone,
        )
        .await
    );
    let _ = step_destroy;
    let (notif_frames, _) = ego_wire.drain_pending_frames();
    let notif_remove_balls: Vec<_> = notif_frames
        .iter()
        .filter_map(|f| match f {
            crate::outbound_sequencer::OutFrame::Notification { event, args }
                if event == "DoDestinyUpdate" =>
            {
                Some(collect_destiny_actions_from_list(&args[0]))
            }
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        notif_remove_balls,
        vec![(
            "RemoveBalls".into(),
            vec![PyRep::List(vec![PyRep::Integer(scooped_drone)])],
        )]
    );
}

#[tokio::test]
async fn bf03_safe_logoff_to_charsel_clears_held_frames_and_scene_wire_state() {
    use crate::outbound_sequencer::{
        GateNotificationOutcome, OutboundSceneScope, SessionWireState, TypedNotification,
    };

    let ctx = ServerContext::new();
    let now_unix = 1_800_000_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    let sid = 91_401_u64;
    let uid = 1_u64;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;

    ctx.db
        .acquire_online_lock(uid, sid, Some(char_id), true, now_unix)
        .expect("acquire online lock");
    let ep = ctx.record_atomic_session_transition(
        crate::router::binding_transition_policy::AtomicSceneTransitionRecord {
            session_id: sid,
            user_id: Some(uid),
            char_id,
            kind_label: "undock",
            reason: "bf03_test",
            is_logout: false,
            is_login: false,
            prev_sys: None,
            prev_dock: None,
            next_sys: Some(sys_id),
            next_dock: None,
            can_record_departed: false,
            removed_bindings: Vec::new(),
        },
    );

    let mut session = SessionState::new(sid);
    session.user_id = Some(uid);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    let mut sys_rx = Some(ctx.systems.get_or_spawn(sys_id).subscribe());
    let mut notify_rx = ctx.notify_tx.subscribe();
    let mut subscribed_sys_id = Some(sys_id);
    let mut wire_state = SessionWireState::new(sid, 1_000_000_914_011);
    assert!(wire_state
        .sync_scene_with_ballpark(ep, Some(sys_id), true, true)
        .is_empty());

    let held_outcome = wire_state.gate_typed_notification(
        TypedNotification::new(
            "OnDamageStateChange",
            vec![PyRep::Integer(ship_id), PyRep::None],
        )
        .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_id), Some(ep)))),
    );
    assert_eq!(held_outcome, GateNotificationOutcome::Held(None));
    let held_future_outcome = wire_state.gate_typed_notification(
        TypedNotification::new("OnCombatMessage", vec![PyRep::String("future".into())])
            .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_id), Some(ep + 1)))),
    );
    assert_eq!(held_future_outcome, GateNotificationOutcome::Held(None));
    assert_eq!(wire_state.held_scene_frame_count(), 2);
    assert!(wire_state.in_space);

    // Safe logoff drops active character ownership on the account's online lock (`character_id = None`)
    // before dispatching `SafeLogoffCharsel` to the listener loop.
    ctx.db
        .acquire_online_lock(uid, sid, None, false, now_unix + 1)
        .expect("transition online lock to charsel");

    let mut deferred_ship_id = None;
    let mut pending_dock = None;
    let mut safe_logoff_teardown = None;
    let (_client_rd, mut server_wr) = tokio::io::duplex(4096);
    {
        let mut wire = SafeLogoffWireContext {
            client_id: 1_000_000_914_011,
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
            &char_id.to_string(),
        )
        .await
        .expect("handle_safe_logoff_to_charsel");
    }

    assert!(session.character_id.is_none());
    assert!(session.solar_system_id.is_none());
    assert!(sys_rx.is_none());
    assert!(subscribed_sys_id.is_none());
    assert!(!wire_state.in_space);
    assert!(wire_state.solar_system_id.is_none());
    assert_eq!(wire_state.scene_epoch, ep);
    assert_eq!(wire_state.held_scene_frame_count(), 0);
    assert_eq!(wire_state.scene_drop_count(), 2);
    let ledger = ctx.flight_recorder.get_outbound_ledger(sid, 16);
    assert!(
        ledger.iter().any(|entry| {
            entry.disposition == crate::flight_recorder::OutboundDisposition::Dropped
                && entry.reason == "safe_logoff_charsel"
                && entry.packet_kind == "OnCombatMessage"
        }),
        "P2-03: expected safe_logoff_charsel drop in outbound ledger, got {ledger:?}"
    );

    let late_outcome = wire_state.gate_typed_notification(
        TypedNotification::new(
            "OnDamageStateChange",
            vec![PyRep::Integer(ship_id), PyRep::None],
        )
        .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_id), Some(ep)))),
    );
    assert!(
        matches!(late_outcome, GateNotificationOutcome::Dropped(_)),
        "BF-03: late space notification after charsel logoff must drop, got {late_outcome:?}"
    );
}

#[tokio::test]
async fn bf03_detach_structure_view_outside_and_rejected_resync_preserve_gunner_scene_and_drop_non_gunner() {
    use crate::outbound_sequencer::{
        DestinyStream, GateNotificationOutcome, OutFrame, OutboundSceneScope, SessionWireState,
        TypedNotification,
    };

    let ctx = ServerContext::new();
    let now_unix = 1_800_000_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    let uid = 1_u64;
    let char_id = 90_000_001_i64;
    let pod_ship_id = 100_000_001_i64;
    let struct_id = 1_000_000_000_777_i64;
    let sys_id = 30_000_142_u32;

    // 1. Direct helper checks for docked non-gunner vs active structure gunner on View Outside detach.
    let mut non_gunner = SessionState::new(91_402);
    non_gunner.user_id = Some(uid);
    non_gunner.character_id = Some(char_id);
    non_gunner.solar_system_id = Some(sys_id);
    non_gunner.structure_id = Some(struct_id);
    non_gunner.ship_id = Some(pod_ship_id);

    let mut gunner = non_gunner.clone();
    gunner.ship_id = Some(struct_id);

    let mut non_gunner_wire = SessionWireState::new(91_402, 1_000_000_914_021);
    assert!(non_gunner_wire
        .sync_scene_with_ballpark(1, Some(sys_id), true, true)
        .is_empty());
    assert!(matches!(
        non_gunner_wire.gate_typed_notification(
            TypedNotification::new("OnTarget", vec![PyRep::Integer(pod_ship_id)])
                .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_id), Some(1)))),
        ),
        GateNotificationOutcome::Held(None)
    ));
    let mut sys_rx_ng = Some(ctx.systems.get_or_spawn(sys_id).subscribe());
    let mut sub_ng = Some(sys_id);
    detach_structure_view_outside_on_beyonce_release(
        &ctx,
        &non_gunner,
        &mut non_gunner_wire,
        &mut TickRxSlot::Optional(&mut sys_rx_ng),
        &mut sub_ng,
    );
    assert!(sys_rx_ng.is_none());
    assert!(sub_ng.is_none());
    assert!(!non_gunner_wire.in_space);
    assert_eq!(non_gunner_wire.held_scene_frame_count(), 0);
    assert_eq!(non_gunner_wire.scene_drop_count(), 1);

    let mut gunner_wire = SessionWireState::new(91_403, 1_000_000_914_031);
    gunner_wire.enter_space(Some(sys_id));
    assert!(gunner_wire
        .sync_scene_with_ballpark(1, Some(sys_id), true, true)
        .is_empty());
    let mut sys_rx_g = Some(ctx.systems.get_or_spawn(sys_id).subscribe());
    let mut sub_g = Some(sys_id);
    detach_structure_view_outside_wire_state(
        &ctx,
        &gunner,
        &mut gunner_wire,
        &mut TickRxSlot::Optional(&mut sys_rx_g),
        &mut sub_g,
    );
    assert!(sys_rx_g.is_none());
    assert!(sub_g.is_none());
    assert!(matches!(gunner_wire.destiny, DestinyStream::Detached));
    assert!(
        gunner_wire.in_space && gunner_wire.scene_entered,
        "BF-03: active gunner must keep scene_in_space without ballpark after View Outside detach"
    );
    assert!(matches!(
        gunner_wire.gate_typed_notification(
            TypedNotification::new(
                "OnCombatMessage",
                vec![PyRep::String("StructureAttack".into()), PyRep::Integer(struct_id)],
            )
            .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_id), None))),
        ),
        GateNotificationOutcome::Emit(_)
    ));

    // 2. Real execute_call_req_wire path on rejected structure resync (`!allow_structure_resync`):
    // structure ballpark is NOT bootstrapped on `ctx`, yet wire starts in AwaitingSetState.
    for (sid, is_gunner) in [(91_404_u64, false), (91_405_u64, true)] {
        let active_ship = if is_gunner { struct_id } else { pod_ship_id };
        ctx.clear_notifications_for_char(char_id);
        ctx.clear_space_ballpark_bootstrapped_for_session(sid, char_id);
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).expect("seeded character");
            ch.user_id = uid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
            ch.active_ship_id = active_ship;
        });
        ctx.db
            .acquire_online_lock(uid, sid, Some(char_id), true, now_unix)
            .expect("acquire lock for rejected structure resync test");

        let mut sess = SessionState::new(sid);
        sess.user_id = Some(uid);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(sys_id);
        sess.station_id = None;
        sess.structure_id = Some(struct_id);
        sess.ship_id = Some(active_ship);

        let mut sys_rx = ctx.systems.get_or_spawn(sys_id).subscribe();
        let mut sub_sys = Some(sys_id);
        let mut wire_state = SessionWireState::new(sid, sid * 10);
        let ep = wire_state.enter_space(Some(sys_id));
        wire_state.destiny = DestinyStream::AwaitingSetState { epoch: ep };
        let mut deferred_ship_id = None;
        let mut wire_refs = CallReqWireRefs {
            client_id: sid * 10,
            sys_rx: &mut sys_rx,
            subscribed_sys_id: &mut sub_sys,
            wire_state: &mut wire_state,
            deferred_ship_id: &mut deferred_ship_id,
        };

        ctx.notify_in_scene(
            crate::router::NotificationTarget::Character(char_id),
            "OnCombatMessage",
            "charid",
            vec![PyRep::String("StructureAttack".into()), PyRep::Integer(struct_id)],
            Some(OutboundSceneScope::space(Some(sys_id), None)),
        );

        let req_source = MachoAddress::Client {
            client_id: sid * 10,
            call_id: 21,
            service: None,
        };
        let req_dest = MachoAddress::Node {
            node_id: eve_machonet::MACHO_PROXY_NODE_ID,
            service: Some("account".to_string()),
            call_id: 0,
        };
        let kwargs = HashMap::new();
        let outcome = execute_call_req_wire(
            &ctx,
            &mut sess,
            &mut wire_refs,
            &req_source,
            &req_dest,
            "account",
            None,
            "GetCashBalance",
            &[],
            &kwargs,
        )
        .await;

        assert!(sub_sys.is_none());
        assert!(matches!(wire_state.destiny, DestinyStream::Detached));
        assert_eq!(wire_state.in_space, is_gunner);
        let emitted_combat = outcome.out_frames.iter().any(|f| {
            matches!(
                f,
                OutFrame::Notification { event, .. } if event == "OnCombatMessage"
            )
        });
        assert_eq!(
            emitted_combat, is_gunner,
            "BF-03: rejected structure resync emitted_combat={emitted_combat} vs expected is_gunner={is_gunner}"
        );
    }
}

