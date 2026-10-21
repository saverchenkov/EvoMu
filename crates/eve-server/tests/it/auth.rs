//! Authentication, service roles bitmasks, native `machoNet` handshake (`P-6`),
//! SSO OAuth2/PKCE server (`ESI-1`), and SDE persistence integration tests.

#![forbid(unsafe_code)]

use eve_machonet::{
    placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeAck, CryptoHandshakeResult,
    CryptoRequestPacket, CryptoServerHandshake, GPSTransportClosed, HandshakeCommand, MachoAddress,
    MachoPacket, MachoPacketType, SessionState, VersionExchange,
};
use eve_proto::PyRep;
use eve_server::macho_listener::{handle_macho_client, read_framed_pyrep, write_framed_pyrep};
use eve_server::router::ServerContext;
use eve_server::sso::handle_http_connection;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::test]
async fn test_phase_0_s9_service_roles_bitmask_and_production_account_seed() {
    // parity: feat:account.account-roles, rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:slash.SlashCmd
    let prod_ctx = ServerContext::new_production();
    let kw = HashMap::new();
    let mut prod_sess = SessionState::new(9090);
    let prod_login = prod_ctx
        .dispatch_rpc(
            &mut prod_sess,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("demo".into()),
                PyRep::String("demo-secret".into()),
            ],
            &kw,
            None,
        )
        .await;
    assert!(
        prod_login.is_err(),
        "S-9: ServerContext::new_production() must not seed demo accounts"
    );

    // Persist player, content, and GM/Admin accounts in a SQLite-backed WorldRepository,
    // reopen from disk to prove durability across server restart, and verify roles are
    // both sent in the session handshake/change wire payloads and enforced by the central role gate.
    let tmp_roles_db =
        std::env::temp_dir().join(format!("evomu_roles_test_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&tmp_roles_db);
    {
        let db = eve_db::WorldRepository::new_empty_with_snapshot_path(Some(tmp_roles_db.clone()))
            .expect("create SQLite world db for role persistence");
        let pilot_uid = db.provision_player_account("pilot_persisted", "pilot-secret-123");
        db.create_character(
            pilot_uid,
            "Pilot RoleTester",
            1,
            1,
            11,
            17,
            &eve_sde::SdeRegistry::with_canonical_seed(),
        )
        .expect("create character for pilot_persisted");
        db.provision_gm_account("gm_persisted", "gm-secret-456", 90_000_701, "ISD Polaris");
        // Also provision a content-editor account with persisted ROLE_PLAYER_DEFAULT | ROLE_CONTENT
        let content_uid = db.provision_player_account("content_persisted", "content-secret-789");
        db.with_mut_snapshot(|snap| {
            if let Some(acc) = snap.accounts.get_mut("content_persisted") {
                acc.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT
                    | eve_machonet::service_roles::ROLE_CONTENT;
            }
            let _ = content_uid;
        });
    }

    let reopened_db = Arc::new(
        eve_db::WorldRepository::new_empty_with_snapshot_path(Some(tmp_roles_db.clone()))
            .expect("reopen SQLite world db for role persistence"),
    );
    let ctx = Arc::new(ServerContext::with_repository(reopened_db));

    // 1. Verify persisted player role over native machoNet handshake (CryptoHandshakeAck + SessionInitialStateNotification + SessionChangeNotification)
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind macho");
    let addr = listener.local_addr().unwrap();
    let ctx_srv = Arc::clone(&ctx);
    let srv_task = tokio::spawn(async move {
        let mut handles = Vec::new();
        for _ in 0..2 {
            if let Ok((stream, _)) = listener.accept().await {
                let c = Arc::clone(&ctx_srv);
                handles.push(tokio::spawn(async move {
                    let _ = handle_macho_client(stream, c).await;
                }));
            }
        }
        for h in handles {
            let _ = h.await;
        }
    });

    let mut player_stream = TcpStream::connect(addr).await.expect("connect player");
    let _srv_ver = read_framed_pyrep(&mut player_stream)
        .await
        .unwrap()
        .unwrap();
    write_framed_pyrep(&mut player_stream, &VersionExchange::default().to_pyrep())
        .await
        .unwrap();
    write_framed_pyrep(
        &mut player_stream,
        &CryptoRequestPacket::placebo().to_pyrep(),
    )
    .await
    .unwrap();
    let _ok_cc = read_framed_pyrep(&mut player_stream)
        .await
        .unwrap()
        .unwrap();
    write_framed_pyrep(
        &mut player_stream,
        &CryptoChallengePacket::new_password_login("pilot_persisted", "pilot-secret-123")
            .to_pyrep(),
    )
    .await
    .unwrap();
    let _pw_ver = read_framed_pyrep(&mut player_stream)
        .await
        .unwrap()
        .unwrap();
    let srv_hs_rep = read_framed_pyrep(&mut player_stream)
        .await
        .unwrap()
        .unwrap();
    let srv_hs = CryptoServerHandshake::from_pyrep(&srv_hs_rep).unwrap();
    write_framed_pyrep(
        &mut player_stream,
        &(CryptoHandshakeResult {
            challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
            func_output: Vec::new(),
            func_result: PyRep::Integer(0),
        })
        .to_pyrep(),
    )
    .await
    .unwrap();
    let player_ack = CryptoHandshakeAck::from_pyrep(
        &read_framed_pyrep(&mut player_stream)
            .await
            .unwrap()
            .unwrap(),
    )
    .expect("player CryptoHandshakeAck");
    assert_eq!(
        player_ack.role,
        eve_machonet::service_roles::ROLE_PLAYER_DEFAULT,
        "S-9: Persisted player account must receive ROLE_PLAYER | ROLE_LOGIN in CryptoHandshakeAck"
    );
    let player_init_pkt = MachoPacket::from_pyrep(
        &read_framed_pyrep(&mut player_stream)
            .await
            .unwrap()
            .unwrap(),
    )
    .expect("player SessionInitialStateNotification");
    assert_eq!(
        player_init_pkt.packet_type,
        MachoPacketType::SessionInitialStateNotification
    );
    let player_scn_pkt = MachoPacket::from_pyrep(
        &read_framed_pyrep(&mut player_stream)
            .await
            .unwrap()
            .unwrap(),
    )
    .expect("player SessionChangeNotification");
    assert_eq!(
        player_scn_pkt.packet_type,
        MachoPacketType::SessionChangeNotification
    );
    drop(player_stream);

    // 2. Verify persisted GM/Admin role over native machoNet handshake and inspect decoded SessionInitialStateNotification + SessionChangeNotification payloads
    let mut gm_stream = TcpStream::connect(addr).await.expect("connect gm");
    let _srv_ver = read_framed_pyrep(&mut gm_stream).await.unwrap().unwrap();
    write_framed_pyrep(&mut gm_stream, &VersionExchange::default().to_pyrep())
        .await
        .unwrap();
    write_framed_pyrep(&mut gm_stream, &CryptoRequestPacket::placebo().to_pyrep())
        .await
        .unwrap();
    let _ok_cc = read_framed_pyrep(&mut gm_stream).await.unwrap().unwrap();
    write_framed_pyrep(
        &mut gm_stream,
        &CryptoChallengePacket::new_password_login("gm_persisted", "gm-secret-456").to_pyrep(),
    )
    .await
    .unwrap();
    let _pw_ver = read_framed_pyrep(&mut gm_stream).await.unwrap().unwrap();
    let gm_srv_hs =
        CryptoServerHandshake::from_pyrep(&read_framed_pyrep(&mut gm_stream).await.unwrap().unwrap())
            .unwrap();
    write_framed_pyrep(
        &mut gm_stream,
        &(CryptoHandshakeResult {
            challenge_response_hash: placebo_crypto_hash(&gm_srv_hs.server_challenge),
            func_output: Vec::new(),
            func_result: PyRep::Integer(0),
        })
        .to_pyrep(),
    )
    .await
    .unwrap();
    let gm_ack =
        CryptoHandshakeAck::from_pyrep(&read_framed_pyrep(&mut gm_stream).await.unwrap().unwrap())
            .expect("gm CryptoHandshakeAck");
    assert_eq!(
        gm_ack.role,
        eve_machonet::service_roles::ROLE_GM_DEFAULT,
        "S-9: Persisted GM account must receive ROLE_GM_DEFAULT bitmask in CryptoHandshakeAck"
    );
    let gm_init_pkt =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut gm_stream).await.unwrap().unwrap())
            .expect("gm SessionInitialStateNotification");
    if let PyRep::Tuple(items) = &gm_init_pkt.payload {
        // V24 body: (sid, sessionType, initialstate)
        assert_eq!(items.get(1), Some(&PyRep::Integer(5)), "SESSION_TYPE_GAME");
        if let Some(PyRep::Dict(entries)) = items.get(2) {
            let role_val = entries
                .iter()
                .find(|(k, _)| k.as_str() == Some("role"))
                .and_then(|(_, v)| v.as_i64())
                .expect("role in SessionInitialStateNotification");
            assert_eq!(
                role_val as u64,
                eve_machonet::service_roles::ROLE_GM_DEFAULT
            );
        } else {
            panic!("Expected Dict in SessionInitialStateNotification payload");
        }
    }
    let gm_scn_pkt =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut gm_stream).await.unwrap().unwrap())
            .expect("gm SessionChangeNotification");
    if let PyRep::Tuple(outer) = &gm_scn_pkt.payload {
        // V24 body: (sid, (0, changes), nodesOfInterest)
        if let Some(PyRep::Tuple(inner)) = outer.get(1) {
            if let Some(PyRep::Dict(changes)) = inner.get(1) {
                let role_change = changes
                    .iter()
                    .find(|(k, _)| k.as_str() == Some("role"))
                    .and_then(|(_, v)| match v {
                        PyRep::Tuple(pair) => pair.get(1).and_then(|n| n.as_i64()),
                        _ => None,
                    })
                    .expect("role in SessionChangeNotification");
                assert_eq!(
                    role_change as u64,
                    eve_machonet::service_roles::ROLE_GM_DEFAULT
                );
            }
        }
    }
    drop(gm_stream);
    let _ = srv_task.await;

    // 3. Verify privileged RPC gating using the persisted accounts (without mutating sess.role in memory)
    let player_chars = ctx.db.get_characters_for_user(player_ack.user_id);
    let player_cid = player_chars[0].character_id;
    let mut sess_player = SessionState::new(909);
    ctx.dispatch_rpc(
        &mut sess_player,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("pilot_persisted".into()),
            PyRep::String("pilot-secret-123".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login pilot_persisted");
    assert_eq!(
        sess_player.role,
        eve_machonet::service_roles::ROLE_PLAYER_DEFAULT
    );
    assert!(!sess_player.has_gm_role());
    ctx.dispatch_rpc(
        &mut sess_player,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(player_cid)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID pilot_persisted");

    for (svc, mth) in [
        ("slash", "SlashCmd"),
        ("publicQaToolsServer", "Command"),
        // rpc-lint: allow negative test (pre-existing): a player must be denied this non-client GM service name
        ("devToolsProvider", "GetDevTools"),
        ("keeper", "GetLevelEditor"),
    ] {
        let denied = ctx
            .dispatch_rpc(
                &mut sess_player,
                svc,
                None,
                mth,
                &[PyRep::String("/giveisk 100".into())],
                &kw,
                None,
            )
            .await;
        assert_eq!(
            denied.unwrap_err().code,
            "InsufficientRole",
            "Non-GM/QA session must be rejected by central role gate on {svc}.{mth}"
        );
    }

    // Verify persisted ROLE_CONTENT account does NOT have GM access
    let mut sess_content = SessionState::new(910);
    ctx.dispatch_rpc(
        &mut sess_content,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("content_persisted".into()),
            PyRep::String("content-secret-789".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login content_persisted");
    assert!(
        !sess_content.has_gm_role(),
        "S-9: Persisted ROLE_CONTENT (1 << 4) account must not be treated as a GM role"
    );

    // Log in as the SQLite-persisted GM account and verify slash.SlashCmd succeeds without in-memory role mutation
    let mut sess_gm = SessionState::new(911);
    ctx.dispatch_rpc(
        &mut sess_gm,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("gm_persisted".into()),
            PyRep::String("gm-secret-456".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login gm_persisted");
    assert_eq!(sess_gm.role, eve_machonet::service_roles::ROLE_GM_DEFAULT);
    assert!(sess_gm.has_gm_role());
    ctx.dispatch_rpc(
        &mut sess_gm,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_701)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID gm_persisted");
    ctx.dispatch_rpc(
        &mut sess_gm,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/giveisk 100".into())],
        &kw,
        None,
    )
    .await
    .expect("S-9: Persisted GM account session must pass GM check on slash.SlashCmd");

    // 4. Grant GM role to pilot_persisted via `/role` slash command and verify SQLite persistence + sessionChange payload
    let role_cmd_rsp = ctx
        .dispatch_rpc(
            &mut sess_gm,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/role pilot_persisted gm".into())],
            &kw,
            None,
        )
        .await
        .expect("/role pilot_persisted gm");
    assert_eq!(
        role_cmd_rsp.dict_get("role").and_then(|v| v.as_i64()),
        Some(eve_machonet::service_roles::ROLE_GM_DEFAULT as i64)
    );
    assert!(role_cmd_rsp.dict_get("sessionChange").is_some());

    let reopened = eve_db::WorldRepository::new_empty_with_snapshot_path(Some(tmp_roles_db.clone()))
        .expect("reopen SQLite");
    let updated_acc = reopened
        .with_snapshot(|snap| snap.accounts.get("pilot_persisted").cloned())
        .expect("pilot_persisted in reopened SQLite");
    assert_eq!(
        updated_acc.role,
        eve_machonet::service_roles::ROLE_GM_DEFAULT,
        "Account role updated via /role must persist across SQLite reopen"
    );

    let _ = std::fs::remove_file(&tmp_roles_db);
}

async fn connect_loopback_with_retry(addr: std::net::SocketAddr) -> TcpStream {
    for attempt in 0..8 {
        match tokio::time::timeout(std::time::Duration::from_secs(3), TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => return stream,
            Ok(Err(_)) | Err(_) if attempt < 7 => {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            Ok(Err(e)) => panic!("TcpStream::connect({addr}) failed after retries: {e:?}"),
            Err(e) => panic!("TcpStream::connect({addr}) timed out after retries: {e:?}"),
        }
    }
    unreachable!()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_phase_2_p6_native_machonet_handshake_state_machine() {
    // parity: plan:P-6, feat:account.native-handshake
    let ctx = Arc::new(ServerContext::new());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind macho listener");
    let addr = listener.local_addr().unwrap();

    let ctx_srv = Arc::clone(&ctx);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let c = Arc::clone(&ctx_srv);
            tokio::spawn(async move {
                let _ = handle_macho_client(stream, c).await;
            });
        }
    });
    tokio::task::yield_now().await;

    // 1. A client build older than the server build must be rejected with GPSTransportClosed (HANDSHAKE_INCOMPATIBLEBUILD)
    {
        let mut s = connect_loopback_with_retry(addr).await;
        let srv_hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        assert_eq!(srv_hello, VersionExchange::default().to_server_hello_pyrep());

        let mut bad_ver = VersionExchange::default();
        bad_ver.build_version = 3_619_879;
        write_framed_pyrep(&mut s, &bad_ver.to_pyrep())
            .await
            .unwrap();
        let err_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let closed = GPSTransportClosed::from_pyrep(&err_rep)
            .expect("Expected GPSTransportClosed on build mismatch");
        assert_eq!(
            closed.reason_code.as_deref(),
            Some("HANDSHAKE_INCOMPATIBLEBUILD")
        );
    }

    // 2. Premature CallReq or legacy AuthenticationReq("ok") before completing handshake must be rejected with GPSTransportClosed
    {
        let mut s = connect_loopback_with_retry(addr).await;
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let fake_auth_req = MachoPacket {
            packet_type: MachoPacketType::AuthenticationReq,
            source: MachoAddress::Client {
                client_id: 1,
                call_id: 1,
                service: None,
            },
            destination: MachoAddress::Node {
                node_id: 888_444,
                service: None,
                call_id: 1,
            },
            user_id: None,
            payload: PyRep::Tuple(vec![PyRep::ascii_bytes("ok")]),
            named_payload: HashMap::new(),
        };
        write_framed_pyrep(&mut s, &fake_auth_req.to_pyrep())
            .await
            .unwrap();
        let err_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let closed = GPSTransportClosed::from_pyrep(&err_rep)
            .expect("Expected GPSTransportClosed on unauthenticated AuthenticationReq('ok')");
        assert_eq!(closed.reason_code.as_deref(), Some("HANDSHAKE_REQUIRED"));
    }

    // 3. Wrong password in Step 3 CryptoChallengePacket must be rejected with GPSTransportClosed (LoginAuthFailed)
    {
        let mut s = connect_loopback_with_retry(addr).await;
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
            .await
            .unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
            .await
            .unwrap();
        let ok_cc = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        assert_eq!(ok_cc.as_str(), Some("OK CC"));

        let bad_challenge =
            CryptoChallengePacket::new_password_login("demo", "wrong-password").to_pyrep();
        write_framed_pyrep(&mut s, &bad_challenge).await.unwrap();
        let err_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let closed = GPSTransportClosed::from_pyrep(&err_rep)
            .expect("Expected GPSTransportClosed on wrong password");
        assert_eq!(closed.reason_code.as_deref(), Some("LoginAuthFailed"));
    }

    // 4. Banned and suspended accounts must be rejected during Step 3 CryptoChallengePacket with GPSTransportClosed carrying reason and expiry
    {
        let now_unix = ctx.now_unix();
        ctx.db.provision_player_account("banned_p6", "pass-p6");
        ctx.db
            .ban_account("banned_p6", now_unix + 7200, "RMT violation")
            .unwrap();

        let mut s = connect_loopback_with_retry(addr).await;
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
            .await
            .unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
            .await
            .unwrap();
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(
            &mut s,
            &CryptoChallengePacket::new_password_login("banned_p6", "pass-p6").to_pyrep(),
        )
        .await
        .unwrap();
        let err_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let closed = GPSTransportClosed::from_pyrep(&err_rep).expect("GPSTransportClosed on ban");
        assert_eq!(closed.reason_code.as_deref(), Some("UserBanned"));
        assert!(closed.reason.contains("RMT violation"));
        assert_eq!(
            closed.reason_args.get("expiry"),
            Some(&PyRep::Integer((now_unix + 7200) as i64))
        );

        ctx.db.provision_player_account("suspended_p6", "pass-p6");
        ctx.db
            .suspend_account("suspended_p6", now_unix + 1800, "Billing hold")
            .unwrap();
        let mut s2 = connect_loopback_with_retry(addr).await;
        let _ = read_framed_pyrep(&mut s2).await.unwrap().unwrap();
        write_framed_pyrep(&mut s2, &VersionExchange::default().to_pyrep())
            .await
            .unwrap();
        write_framed_pyrep(&mut s2, &CryptoRequestPacket::placebo().to_pyrep())
            .await
            .unwrap();
        let _ = read_framed_pyrep(&mut s2).await.unwrap().unwrap();
        write_framed_pyrep(
            &mut s2,
            &CryptoChallengePacket::new_password_login("suspended_p6", "pass-p6").to_pyrep(),
        )
        .await
        .unwrap();
        let err_rep2 = read_framed_pyrep(&mut s2).await.unwrap().unwrap();
        let closed2 =
            GPSTransportClosed::from_pyrep(&err_rep2).expect("GPSTransportClosed on suspension");
        assert_eq!(closed2.reason_code.as_deref(), Some("AccountSuspended"));
        assert!(closed2.reason.contains("Billing hold"));
        assert_eq!(
            closed2.reason_args.get("expiry"),
            Some(&PyRep::Integer((now_unix + 1800) as i64))
        );
    }

    // 5. Full valid 4-step V24.01 handshake (including QC, VK, 2-step passwordVersion=1 -> Attempt 2, CryptoServerHandshake, CryptoHandshakeResult, CryptoHandshakeAck, SessionInitialStateNotification, SessionChangeNotification, and authenticated CallReq)
    {
        let mut s = connect_loopback_with_retry(addr).await;
        let srv_hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let parsed_ver = VersionExchange::from_pyrep(&srv_hello).expect("server VersionExchange");
        assert_eq!(parsed_ver.birthday, 170_472);
        assert_eq!(parsed_ver.macho_version, 496);
        assert!((parsed_ver.version_number - 24.01).abs() < 1e-6);
        assert_eq!(parsed_ver.build_version, 3_624_425);
        assert_eq!(parsed_ver.project_version, "V24.01@ccp");

        // Step 1: Send VersionExchangeClient
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
            .await
            .unwrap();

        // Step 2a: QueueCheck (None, "QC") -> server returns queue position 1
        write_framed_pyrep(&mut s, &HandshakeCommand::QueueCheck.to_pyrep())
            .await
            .unwrap();
        let qc_pos = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        assert_eq!(qc_pos, PyRep::Integer(1));
        // After a queue check the server restarts the handshake with a fresh version tuple.
        let hello_again = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        assert_eq!(hello_again, srv_hello);
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
            .await
            .unwrap();

        // Step 2b: VipKey (None, "VK", "vip-key") -> accepted
        write_framed_pyrep(
            &mut s,
            &HandshakeCommand::VipKey("evomu-vip".into()).to_pyrep(),
        )
        .await
        .unwrap();

        // Step 2c: CryptoRequestPacket ("placebo", {}) -> "OK CC"
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
            .await
            .unwrap();
        let ok_cc = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        assert_eq!(ok_cc.as_str(), Some("OK CC"));

        // Step 3 (2-step V24 password handshake): Attempt 1 has user_password=None -> server replies passwordVersion = 1
        let mut attempt1 = CryptoChallengePacket::new_password_login("demo", "demo-secret");
        attempt1.user_password = None;
        attempt1.user_language_id = "DE".into();
        write_framed_pyrep(&mut s, &attempt1.to_pyrep())
            .await
            .unwrap();
        let pw_ver = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        assert_eq!(pw_ver, PyRep::Integer(1));

        // Attempt 2 includes user_password -> server replies CryptoServerHandshake directly
        let mut attempt2 = CryptoChallengePacket::new_password_login("demo", "demo-secret");
        attempt2.user_language_id = "DE".into();
        write_framed_pyrep(&mut s, &attempt2.to_pyrep())
            .await
            .unwrap();
        let srv_hs_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let srv_hs =
            CryptoServerHandshake::from_pyrep(&srv_hs_rep).expect("decode CryptoServerHandshake");
        assert_eq!(
            srv_hs.challenge_response_hash,
            placebo_crypto_hash(&attempt2.client_challenge)
        );
        assert_eq!(srv_hs.macho_version, 496);
        assert_eq!(srv_hs.boot_build, 3_624_425);
        assert_eq!(srv_hs.boot_codename, "V24.01");
        assert_eq!(srv_hs.boot_region, "ccp");

        // Step 4: Send CryptoHandshakeResult -> receive CryptoHandshakeAck + SessionInitialStateNotification (18) + SessionChangeNotification (16)
        let hs_res = CryptoHandshakeResult {
            challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
            func_output: Vec::new(),
            func_result: PyRep::Integer(0),
        };
        write_framed_pyrep(&mut s, &hs_res.to_pyrep())
            .await
            .unwrap();

        let ack_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let ack = CryptoHandshakeAck::from_pyrep(&ack_rep).expect("decode CryptoHandshakeAck");
        assert_eq!(ack.user_id, 1);
        assert_eq!(ack.role, eve_machonet::service_roles::ROLE_PLAYER_DEFAULT);
        assert_eq!(ack.language_id, "DE");

        let init_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let init_pkt =
            MachoPacket::from_pyrep(&init_rep).expect("decode SessionInitialStateNotification");
        assert_eq!(
            init_pkt.packet_type,
            MachoPacketType::SessionInitialStateNotification
        );

        let scn_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let scn_pkt = MachoPacket::from_pyrep(&scn_rep).expect("decode SessionChangeNotification");
        assert_eq!(
            scn_pkt.packet_type,
            MachoPacketType::SessionChangeNotification
        );

        // Step 5: Execute authenticated CallReq (machoNet.GetTime)
        let call_pkt = MachoPacket {
            packet_type: MachoPacketType::CallReq,
            source: MachoAddress::Client {
                client_id: ack.client_id,
                call_id: 7,
                service: None,
            },
            destination: MachoAddress::Node {
                node_id: 888_444,
                service: Some("machoNet".into()),
                call_id: 7,
            },
            user_id: Some(ack.user_id),
            payload: PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::String("GetTime".into()),
                PyRep::Tuple(vec![]),
                PyRep::Dict(vec![]),
            ])))]),
            named_payload: HashMap::new(),
        };
        write_framed_pyrep(&mut s, &call_pkt.to_pyrep())
            .await
            .unwrap();
        let rsp_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let rsp_pkt = MachoPacket::from_pyrep(&rsp_rep).expect("CallRsp");
        assert_eq!(rsp_pkt.packet_type, MachoPacketType::CallRsp);
    }
}

async fn send_raw_http_request(
    addr: std::net::SocketAddr,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, HashMap<String, String>, serde_json::Value, String) {
    let mut stream = None;
    for _ in 0..5 {
        if let Ok(Ok(s)) =
            tokio::time::timeout(std::time::Duration::from_secs(2), TcpStream::connect(addr)).await
        {
            stream = Some(s);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let mut stream = stream.expect("connect HTTP SSO");
    let mut req = format!(
        "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: {}\r\n",
        addr.port(),
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    stream.write_all(req.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body_str) = text.split_once("\r\n\r\n").expect("HTTP response split");
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap();
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let mut resp_headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            resp_headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let json_val = serde_json::from_str(body_str).unwrap_or(serde_json::Value::Null);
    (status_code, resp_headers, json_val, body_str.to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_phase_2_esi1_sso_oauth2_pkce_http_server_and_macho_sso_login() {
    // parity: plan:ESI-1, feat:account.sso-login, feat:esi.core.sso-tokens
    let ctx = Arc::new(ServerContext::new());

    let http_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind SSO HTTP listener");
    let http_addr = http_listener.local_addr().unwrap();
    let ctx_http = Arc::clone(&ctx);
    tokio::spawn(async move {
        while let Ok((stream, _)) = http_listener.accept().await {
            let c = Arc::clone(&ctx_http);
            tokio::spawn(async move {
                let _ = handle_http_connection(stream, c).await;
            });
        }
    });

    let macho_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind macho listener");
    let macho_addr = macho_listener.local_addr().unwrap();
    let ctx_macho = Arc::clone(&ctx);
    tokio::spawn(async move {
        while let Ok((stream, _)) = macho_listener.accept().await {
            let c = Arc::clone(&ctx_macho);
            tokio::spawn(async move {
                let _ = handle_macho_client(stream, c).await;
            });
        }
    });

    // 1. OAuth2 discovery metadata & JWKS endpoint
    let (disc_status, _, disc_json, _) = send_raw_http_request(
        http_addr,
        "GET",
        "/.well-known/oauth-authorization-server",
        &[],
        "",
    )
    .await;
    assert_eq!(disc_status, 200);
    assert_eq!(disc_json["issuer"], "https://login.eveonline.com");
    assert_eq!(disc_json["code_challenge_methods_supported"][0], "S256");

    let (jwks_status, _, jwks_json, _) =
        send_raw_http_request(http_addr, "GET", "/oauth/jwks", &[], "").await;
    assert_eq!(jwks_status, 200);
    assert_eq!(jwks_json["keys"][0]["alg"], "HS256");
    assert_eq!(jwks_json["keys"][0]["kid"], ctx.db.jwt_key_id());

    // 2. GET /v2/oauth/authorize without credentials renders HTML login/consent form;
    //    POST /v2/oauth/authorize with PKCE S256 issues single-use authorization code and 302 redirect
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    let (html_status, _, _, html_body) = send_raw_http_request(
        http_addr,
        "GET",
        &format!(
            "/v2/oauth/authorize?response_type=code&client_id=eveLauncherTQ&redirect_uri=https://localhost/callback&code_challenge={challenge}&code_challenge_method=S256&scope=esi-wallet.read_character_wallet.v1+esi-skills.read_skills.v1&state=xyz123"
        ),
        &[],
        "",
    )
    .await;
    assert_eq!(html_status, 200);
    assert!(html_body.contains("<form method=\"POST\" action=\"/v2/oauth/authorize\">"));

    let (auth_status, auth_hdrs, auth_json, _) = send_raw_http_request(
        http_addr,
        "POST",
        "/v2/oauth/authorize",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        &format!(
            "response_type=code&client_id=eveLauncherTQ&redirect_uri=https%3A%2F%2Flocalhost%2Fcallback&code_challenge={challenge}&code_challenge_method=S256&scope=esi-wallet.read_character_wallet.v1+esi-skills.read_skills.v1&state=xyz123&username=demo&password=demo-secret&character_id=90000001"
        ),
    )
    .await;
    assert_eq!(auth_status, 302);
    let code = auth_json["code"].as_str().expect("auth code").to_string();
    let location = auth_hdrs.get("location").expect("Location redirect header");
    assert!(location.contains(&format!("code={code}")) && location.contains("state=xyz123"));

    // 3. POST /v2/oauth/token with wrong code_verifier fails with 400 invalid_grant;
    //    valid PKCE code_verifier on a fresh code returns signed JWT access_token + refresh_token
    let (bad_tok_status, _, bad_tok_json, _) = send_raw_http_request(
        http_addr,
        "POST",
        "/v2/oauth/token",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        &format!(
            "grant_type=authorization_code&client_id=eveLauncherTQ&code={code}&code_verifier=wrong_verifier"
        ),
    )
    .await;
    assert_eq!(bad_tok_status, 400);
    assert_eq!(bad_tok_json["error"], "invalid_grant");

    // Issue fresh code and exchange with valid PKCE code_verifier
    let (_, _, auth_json2, _) = send_raw_http_request(
        http_addr,
        "POST",
        "/v2/oauth/authorize",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        &format!(
            "response_type=code&client_id=eveLauncherTQ&redirect_uri=https%3A%2F%2Flocalhost%2Fcallback&code_challenge={challenge}&code_challenge_method=S256&scope=esi-wallet.read_character_wallet.v1+esi-skills.read_skills.v1&state=xyz123&username=demo&password=demo-secret&character_id=90000001"
        ),
    )
    .await;
    let code2 = auth_json2["code"].as_str().unwrap().to_string();
    let (tok_status, _, tok_json, _) = send_raw_http_request(
        http_addr,
        "POST",
        "/v2/oauth/token",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        &format!(
            "grant_type=authorization_code&client_id=eveLauncherTQ&code={code2}&code_verifier={verifier}"
        ),
    )
    .await;
    assert_eq!(tok_status, 200);
    let access_token = tok_json["access_token"].as_str().unwrap().to_string();
    let refresh_token = tok_json["refresh_token"].as_str().unwrap().to_string();
    assert_eq!(tok_json["token_type"], "Bearer");
    assert_eq!(tok_json["expires_in"], 1200);

    // 4. GET /v2/oauth/verify verifies JWT signature, expiry, and route scope (200 OK vs 401 vs 403)
    let (ver_status, _, ver_json, _) = send_raw_http_request(
        http_addr,
        "GET",
        "/v2/oauth/verify?required_scope=esi-wallet.read_character_wallet.v1",
        &[("Authorization", &format!("Bearer {access_token}"))],
        "",
    )
    .await;
    assert_eq!(ver_status, 200);
    assert_eq!(ver_json["CharacterID"], 90_000_001);
    assert_eq!(ver_json["CharacterName"], "Aura Capsuleer");
    assert_eq!(
        ver_json["Scopes"],
        "esi-wallet.read_character_wallet.v1 esi-skills.read_skills.v1"
    );

    // Missing scope returns 403 Forbidden (insufficient_scope)
    let (scope_err_status, _, scope_err_json, _) = send_raw_http_request(
        http_addr,
        "GET",
        "/v2/oauth/verify?required_scope=esi-mail.read_mail.v1",
        &[("Authorization", &format!("Bearer {access_token}"))],
        "",
    )
    .await;
    assert_eq!(scope_err_status, 403);
    assert_eq!(scope_err_json["error"], "insufficient_scope");

    // Tampered token signature returns 401 Unauthorized (invalid_token)
    let (tamper_status, _, tamper_json, _) = send_raw_http_request(
        http_addr,
        "GET",
        "/v2/oauth/verify",
        &[("Authorization", &format!("Bearer {access_token}tampered"))],
        "",
    )
    .await;
    assert_eq!(tamper_status, 401);
    assert_eq!(tamper_json["error"], "invalid_token");

    // 5. Authenticate over native machoNet GPSTransport TCP using the SSO JWT (`EVE-SSO-CONNECTION`)
    {
        let mut s = TcpStream::connect(macho_addr).await.unwrap();
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep())
            .await
            .unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep())
            .await
            .unwrap();
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();

        let sso_challenge = CryptoChallengePacket::new_sso_login(&access_token).to_pyrep();
        write_framed_pyrep(&mut s, &sso_challenge).await.unwrap();
        let pw_ver = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        assert_eq!(pw_ver, PyRep::Integer(2));
        let srv_hs_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let srv_hs = CryptoServerHandshake::from_pyrep(&srv_hs_rep).unwrap();

        let hs_res = CryptoHandshakeResult {
            challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
            func_output: Vec::new(),
            func_result: PyRep::Integer(0),
        };
        write_framed_pyrep(&mut s, &hs_res.to_pyrep())
            .await
            .unwrap();
        let ack =
            CryptoHandshakeAck::from_pyrep(&read_framed_pyrep(&mut s).await.unwrap().unwrap())
                .expect("SSO CryptoHandshakeAck");
        assert_eq!(ack.user_id, 1);
        assert_eq!(ack.access_token.as_deref(), Some(access_token.as_str()));
    }

    // 6. Refresh token rotation (`grant_type=refresh_token`) and revocation (`POST /v2/oauth/revoke`)
    let (ref_status, _, ref_json, _) = send_raw_http_request(
        http_addr,
        "POST",
        "/v2/oauth/token",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        &format!(
            "grant_type=refresh_token&client_id=eveLauncherTQ&refresh_token={refresh_token}"
        ),
    )
    .await;
    assert_eq!(ref_status, 200);
    let rotated_access = ref_json["access_token"].as_str().unwrap().to_string();
    let rotated_refresh = ref_json["refresh_token"].as_str().unwrap().to_string();
    assert_ne!(rotated_refresh, refresh_token);

    // Reusing the old rotated refresh token must fail with 400 invalid_grant
    let (reuse_status, _, reuse_json, _) = send_raw_http_request(
        http_addr,
        "POST",
        "/v2/oauth/token",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        &format!(
            "grant_type=refresh_token&client_id=eveLauncherTQ&refresh_token={refresh_token}"
        ),
    )
    .await;
    assert_eq!(reuse_status, 400);
    assert_eq!(reuse_json["error"], "invalid_grant");

    // Revoke rotated_refresh via POST /v2/oauth/revoke -> paired rotated_access JWT is also revoked
    let (rev_status, _, rev_json, _) = send_raw_http_request(
        http_addr,
        "POST",
        "/v2/oauth/revoke",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        &format!("token={rotated_refresh}&client_id=eveLauncherTQ"),
    )
    .await;
    assert_eq!(rev_status, 200);
    assert_eq!(rev_json["revoked"], true);

    let (ver_after_rev_status, _, _, _) = send_raw_http_request(
        http_addr,
        "GET",
        "/v2/oauth/verify",
        &[("Authorization", &format!("Bearer {rotated_access}"))],
        "",
    )
    .await;
    assert_eq!(
        ver_after_rev_status, 401,
        "Revoking refresh token must also revoke its paired access token JTI"
    );
}
