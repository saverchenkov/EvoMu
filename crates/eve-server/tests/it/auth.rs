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
    // parity: feat:account.account-roles
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
        if let Some(PyRep::Dict(entries)) = items.first() {
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
        if let Some(PyRep::Tuple(inner)) = outer.first() {
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

#[tokio::test]
async fn test_phase_1_d1_to_d5_sde_sqlite_persistence_auth_sso_and_config() {
    // parity: plan:S-11, plan:D-2
    // parity: feat:serverops.static-data, feat:account.multi-login-block, feat:account.account-bans, feat:admin.gm-ban
    use eve_server::config::ServerConfig;
    use std::path::Path;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let sde_path = eve_sde::SdeRegistry::default_db_path();
    assert!(
        sde_path.exists(),
        "D-1/D-2: local data/evomu_sde.sqlite must exist on disk (built via tools/sde-import)"
    );
    let real_sde = eve_sde::SdeRegistry::open(&sde_path).expect("open local data/evomu_sde.sqlite");
    assert_eq!(real_sde.resources.len(), 213);
    assert!(real_sde.solar_systems.contains_key(&30_000_142));

    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // 1. D-1 & D-2: 213-resource SDE loaded at runtime from SQLite
    assert_eq!(ctx.sde.resources.len(), 213);
    assert!(!ctx.sde.types.is_empty());
    assert!(!ctx.sde.solar_systems.is_empty());
    assert_eq!(ctx.sde.market_groups.len(), 2114);
    assert!(ctx.sde.market_groups.contains_key(&4));
    assert_eq!(ctx.sde.attributes.len(), 2871);
    assert_eq!(
        ctx.sde.attributes.get(&54).map(|a| a.name.as_str()),
        Some("maxRange")
    );
    assert_eq!(ctx.sde.effects.len(), 3427);
    assert_eq!(
        ctx.sde.effects.get(&12).map(|e| e.name.as_str()),
        Some("hiPower")
    );
    assert_eq!(ctx.sde.blueprints.len(), 5100);
    assert!(
        ctx.sde.blueprints.contains_key(&691),
        "Rifter Blueprint (691) must be present in SdeRegistry::blueprints"
    );
    assert!(
        ctx.sde.factions.len() >= 20 && ctx.sde.factions.contains_key(&500001),
        "SDE factions table must be imported from V24 client resources"
    );
    assert!(
        ctx.sde.npc_corporations.len() >= 200 && ctx.sde.npc_corporations.contains_key(&1000035),
        "SDE npc_corporations table must be imported from V24 client resources"
    );
    assert!(
        ctx.sde.agents.len() >= 1000,
        "SDE agents table must be imported from V24 client resources"
    );
    assert!(
        ctx.sde.types.values().filter(|t| t.category_id == 11).count() > 1000,
        "SDE types table must contain imported category-11 NPC entity types"
    );
    assert_eq!(
        ctx.sde.npc_spawn_groups.len(),
        392,
        "DERIVED sde_npc_spawn_groups built deterministically from SDE category-11 NPC types"
    );
    assert!(
        !ctx.sde.variations_of(587).is_empty(),
        "D-2: SdeRegistry::variations_of(587) must return Rifter hull variations in O(1)"
    );

    // Authenticate session 1 as "demo" and register a live session closer channel
    let mut sess1 = SessionState::new(10_101);
    let (sess1_close_tx, mut sess1_close_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    ctx.register_session_closer(sess1.session_id, sess1_close_tx);

    ctx.dispatch_rpc(
        &mut sess1,
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
    .await
    .expect("Login sess1");

    // V24 `multiLoginBlocker.Login(computerhash)` on sess1 records computerhash and returns True
    let mlb_sess1 = ctx
        .dispatch_rpc(
            &mut sess1,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::String("hw-hash-alpha-01".into())],
            &kw,
            None,
        )
        .await
        .expect("multiLoginBlocker.Login(computerhash) on sess1");
    assert_eq!(mlb_sess1, PyRep::Bool(true));

    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess1");

    // Verify config.GetInvTypesEx returns UnimplementedRPC until Phase 2 P-4
    let unimpl_cfg = ctx
        .dispatch_rpc(
            &mut sess1,
            "config",
            None,
            "GetInvTypesEx",
            &[PyRep::Integer(587)],
            &kw,
            None,
        )
        .await;
    assert!(
        unimpl_cfg.is_err(),
        "R37-P1-3: config.GetInvTypesEx must return UnimplementedRPC until Phase 2 P-4"
    );

    // Verify R37-P1-2 / R38-P1-2 / R38-P1-3: RequestMission enforces co-location, storyline referral (types 6/7),
    // level standing gate, agent division pool matching, and !is_storyline && has_standing_rewards tally gate
    let remote_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(3_012_018)],
            &kw,
            None,
        )
        .await
        .expect_err("DoAction(RequestMission) for remote Maurasi agent from Jita must fail");
    assert_eq!(remote_err.code, "AgentNotCoLocated");

    // Storyline agents (agentType 7 = 3_009_966, agentType 6 = 3_017_107) must be rejected even when co-located
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_004_969;
            ch.station_id = Some(60_011_920);
        }
    });
    sess1.solar_system_id = Some(30_004_969);
    sess1.station_id = Some(60_011_920);
    let storyline_t7_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(3_009_966)],
            &kw,
            None,
        )
        .await
        .expect_err("Storyline agent (type 7) must be rejected without referral");
    assert_eq!(storyline_t7_err.code, "StorylineAgentRequiresReferral");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_003_041;
            ch.station_id = Some(60_002_647);
        }
    });
    sess1.solar_system_id = Some(30_003_041);
    sess1.station_id = Some(60_002_647);
    let storyline_t6_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(3_017_107)],
            &kw,
            None,
        )
        .await
        .expect_err("Generic storyline agent (type 6) must be rejected without referral");
    assert_eq!(storyline_t6_err.code, "StorylineAgentRequiresReferral");

    // Level 2 agent (3_016_842 in Maurasi 30_000_144 / station 60_003_754) requires corp standing >= 1.0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_000_144;
            ch.station_id = Some(60_003_754);
            ch.standings.clear();
        }
    });
    sess1.solar_system_id = Some(30_000_144);
    sess1.station_id = Some(60_003_754);
    let l2_standing_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(3_016_842)],
            &kw,
            None,
        )
        .await
        .expect_err(
            "Level 2 agent with 0.0 corp standing must fail with InsufficientAgentStanding",
        );
    assert_eq!(l2_standing_err.code, "InsufficientAgentStanding");

    // Default (no-arg) DoAction in Maurasi (60_003_754) selects co-located L1 Security agent 3_012_018
    // and draws a BasicKillMission from that agent's division pool
    let maurasi_offer = ctx
        .dispatch_rpc(
            &mut sess1,
            "agentMgr",
            None,
            "DoAction",
            &[],
            &kw,
            None,
        )
        .await
        .expect("Default DoAction in Maurasi must select co-located L1 agent");
    let PyRep::KeyVal(maurasi_kv) = maurasi_offer else {
        panic!("Expected KeyVal from DoAction");
    };
    assert_eq!(maurasi_kv.get("agentID"), Some(&PyRep::Integer(3_012_018)));
    let offered_mid = maurasi_kv.get("missionID").unwrap().as_i64().unwrap() as u32;
    let offered_tmpl = ctx
        .sde
        .missions
        .get(&offered_mid)
        .expect("offered mission in SDE");
    assert!(
        offered_tmpl
            .content_template
            .as_deref()
            .unwrap_or_default()
            .contains("BasicKill"),
        "Security division 24 agent must offer BasicKillMission from its pool"
    );

    // R39-P1-2: Real Epic Arc agent (Sister Alitura 3_019_356, agentType 10 in Arnon 30_005_001 / station 60_012_607)
    // offers an EpicArc mission template, while Aura agent (3_019_486, agentType 11 in 30_001_672 / 60_007_702) returns UnimplementedRPC
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_005_001;
            ch.station_id = Some(60_012_607);
        }
    });
    sess1.solar_system_id = Some(30_005_001);
    sess1.station_id = Some(60_012_607);
    let epic_arc_offer = ctx
        .dispatch_rpc(
            &mut sess1,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(3_019_356)],
            &kw,
            None,
        )
        .await
        .expect("Epic Arc agent 3_019_356 (type 10) must offer an EpicArc mission");
    let PyRep::KeyVal(epic_kv) = epic_arc_offer else {
        panic!("Expected KeyVal from Epic Arc DoAction");
    };
    let epic_mid = epic_kv.get("missionID").unwrap().as_i64().unwrap() as u32;
    let epic_tmpl = ctx
        .sde
        .missions
        .get(&epic_mid)
        .expect("EpicArc mission in SDE");
    assert!(
        epic_tmpl
            .content_template
            .as_deref()
            .unwrap_or_default()
            .contains("EpicArc"),
        "Epic Arc agent (type 10) must draw from EpicArc mission pool"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_001_672;
            ch.station_id = Some(60_007_702);
        }
    });
    sess1.solar_system_id = Some(30_001_672);
    sess1.station_id = Some(60_007_702);
    let aura_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(3_019_486)],
            &kw,
            None,
        )
        .await
        .expect_err("Aura agent (type 11) must return UnimplementedRPC");
    assert_eq!(aura_err.code, "UnimplementedRPC");

    // Restore sess1 (90_000_001) to Jita 4-4 and verify CompleteMission tally gate (!is_storyline && has_standing_rewards)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(60_003_760);
        }
    });
    sess1.solar_system_id = Some(30_000_142);
    sess1.station_id = Some(60_003_760);
    let no_standing_mid = *ctx
        .sde
        .missions
        .values()
        .find(|m| !m.has_standing_rewards)
        .map(|m| &m.mission_id)
        .expect("SDE mission with has_standing_rewards == false");
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (90_000_001, no_standing_mid),
            eve_db::ActiveMissionRecord {
                mission_id: no_standing_mid,
                character_id: 90_000_001,
                agent_id: 3_020_034,
                solar_system_id: 30_000_142,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
            },
        );
    });
    let comp_no_st = ctx
        .dispatch_rpc(
            &mut sess1,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(6), PyRep::Integer(no_standing_mid as i64)],
            &kw,
            None,
        )
        .await
        .expect("DoAction(Complete) for non-standing mission");
    let PyRep::KeyVal(comp_no_st_kv) = comp_no_st else {
        panic!("Expected KeyVal from DoAction(Complete)");
    };
    assert_eq!(
        comp_no_st_kv.get("missionTally"),
        Some(&PyRep::Integer(0)),
        "R37-P1-1 / R38-P1-3: Mission without standing rewards must not increment storyline mission_tally"
    );

    // Bind an invbroker object on sess1 to test R35-P1-3 bound-object revocation enforcement
    let bind_rsp1 = ctx
        .dispatch_rpc(
            &mut sess1,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(60_003_760), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject invbroker on sess1");
    let bound_id_sess1 = match bind_rsp1 {
        PyRep::Tuple(t) => t[0].as_i64().expect("bound_id i64") as u64,
        other => panic!("Expected Tuple from MachoBindObject, got {other:?}"),
    };

    // 2. D-4: V24 `multiLoginBlocker.Login(computerhash)` + Alpha clone hardware concurrency + live session disconnect:
    // Verify Alpha-clone hardware concurrency across different accounts (`demo` and `other` both Alpha `clone_grade = 0`):
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch1) = snap.characters.get_mut(&90_000_001) {
            ch1.clone_grade = 0; // Alpha clone
        }
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.clone_grade = 0; // Alpha clone
        }
    });
    let mut sess_other = SessionState::new(10_199);
    ctx.dispatch_rpc(
        &mut sess_other,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("other".into()),
            PyRep::String("other-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login other");
    let alpha_same_hw = ctx
        .dispatch_rpc(
            &mut sess_other,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::String("hw-hash-alpha-01".into())],
            &kw,
            None,
        )
        .await
        .expect("multiLoginBlocker.Login on second Alpha account with same computerhash");
    assert_eq!(
        alpha_same_hw,
        PyRep::Bool(false),
        "V24 multiLoginBlocker.Login(computerhash) must return False when another Alpha account is active on the same computerhash"
    );
    let alpha_diff_hw = ctx
        .dispatch_rpc(
            &mut sess_other,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::String("hw-hash-alpha-02".into())],
            &kw,
            None,
        )
        .await
        .expect("multiLoginBlocker.Login on second Alpha account with distinct computerhash");
    assert_eq!(
        alpha_diff_hw,
        PyRep::Bool(true),
        "V24 multiLoginBlocker.Login(computerhash) must return True when another Alpha account logs in from a distinct computerhash"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch1) = snap.characters.get_mut(&90_000_001) {
            ch1.clone_grade = 1;
        }
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.clone_grade = 1;
        }
    });
    // When both accounts are Omega (clone_grade == 1), multiLoginBlocker.Login on the same computerhash must succeed
    let omega_same_hw = ctx
        .dispatch_rpc(
            &mut sess_other,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::String("hw-hash-alpha-01".into())],
            &kw,
            None,
        )
        .await
        .expect("multiLoginBlocker.Login on second Omega account with same computerhash");
    assert_eq!(omega_same_hw, PyRep::Bool(true));

    ctx.dispatch_rpc(
        &mut sess_other,
        "authentication",
        None,
        "Logout",
        &[],
        &kw,
        None,
    )
    .await
    .unwrap();

    // Second concurrent session (`sess2`) logging into "demo" via canonical 2-arg `authentication.Login(username, password)`:
    // Immediately displaces/kicks the prior active session (`sess1`) on the same account with `OnSessionTerminate`
    let mut sess2 = SessionState::new(10_102);
    ctx.dispatch_rpc(
        &mut sess2,
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
    .await
    .expect("Login sess2 immediately displaces sess1");

    let (displaced_code, displaced_reason) = sess1_close_rx
        .try_recv()
        .expect("Displaced live session must immediately receive disconnect notification on second login");
    assert_eq!(displaced_code, "ACL_DISPLACED");
    assert!(
        displaced_reason.contains("Displaced"),
        "Displacement reason must reference concurrent login displacement, got {displaced_reason}"
    );
    assert_eq!(
        ctx.db.get_online_session(1).map(|s| s.session_id),
        Some(sess2.session_id),
        "Prior session must be removed from active online_sessions and replaced by sess2"
    );

    // Canonical 1-arg `multiLoginBlocker.Login(computerhash)` succeeds for `sess2`
    let mlb_sess2 = ctx
        .dispatch_rpc(
            &mut sess2,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::String("hw-hash-alpha-02".into())],
            &kw,
            None,
        )
        .await
        .expect("V24 multiLoginBlocker.Login(computerhash) on sess2");
    assert_eq!(mlb_sess2, PyRep::Bool(true));

    // R35-P1-3: Displaced sess1 must NOT be able to bypass revocation via a previously bound object
    let sess1_bound_bypass = ctx
        .dispatch_rpc(
            &mut sess1,
            "authentication",
            Some(bound_id_sess1),
            "List",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        sess1_bound_bypass.unwrap_err().code,
        "SessionKicked",
        "R35-P1-3: Displaced session must be rejected with SessionKicked on bound-object calls"
    );
    assert!(sess1.user_id.is_none());

    // 3. D-4: Admin /kick, /ban, /unban, /suspend, and /unsuspend slash commands + live session revocation + ROLE_ADMIN ban hierarchy
    ctx.db
        .provision_gm_account("gm_d4", "gm-d4-secret", 90_000_003, "ISD Polaris");
    let mut gm_sess = SessionState::new(10_103);
    ctx.dispatch_rpc(
        &mut gm_sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("gm_d4".into()),
            PyRep::String("gm-d4-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login gm_sess");
    ctx.dispatch_rpc(
        &mut gm_sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_003)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID gm_sess");

    // Verify /ban and /kick refuse to target a ROLE_ADMIN account
    let ban_admin_err = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/ban gm_d4 3600 Self ban attempt".into())],
            &kw,
            None,
        )
        .await;
    assert_eq!(ban_admin_err.unwrap_err().code, "BanFailed");
    let kick_admin_err = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/kick gm_d4 Self kick attempt".into())],
            &kw,
            None,
        )
        .await;
    assert!(matches!(
        kick_admin_err.unwrap_err().code.as_str(),
        "KickRefusedAdmin" | "KickFailed"
    ));

    // Select character on sess2 so it is active in-game, bind invbroker, then kick "demo" via GM slash command
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess2");

    let bind_rsp2 = ctx
        .dispatch_rpc(
            &mut sess2,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(60_003_760), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject invbroker on sess2");
    let bound_id_sess2 = match bind_rsp2 {
        PyRep::Tuple(t) => t[0].as_i64().expect("bound_id i64") as u64,
        other => panic!("Expected Tuple from MachoBindObject, got {other:?}"),
    };

    ctx.dispatch_rpc(
        &mut gm_sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/kick demo Market bot check".into())],
        &kw,
        None,
    )
    .await
    .expect("/kick demo");

    // R36-P1-3: Kicked sess2 must NOT be able to bypass revocation via multiLoginBlocker.Login(computerhash)
    let sess2_mlb_after_kick = ctx
        .dispatch_rpc(
            &mut sess2,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::String("hw-hash-alpha-02".into())],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        sess2_mlb_after_kick.unwrap_err().code,
        "SessionKicked",
        "R36-P1-3: Kicked session calling multiLoginBlocker.Login(computerhash) must be rejected with SessionKicked"
    );
    assert!(
        sess2.user_id.is_none(),
        "R36-P1-3: Kicked session must have user_id cleared on first post-kick RPC"
    );
    assert!(
        ctx.db.get_online_session(1).is_none(),
        "R36-P1-3: Kicked session must not re-acquire the online lock via multiLoginBlocker.Login"
    );
    let sess2_bound_after_kick = ctx
        .dispatch_rpc(
            &mut sess2,
            "authentication",
            Some(bound_id_sess2),
            "List",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        sess2_bound_after_kick.unwrap_err().code,
        "InvalidBoundObject",
        "Kicked session's bound objects must be purged on kick cleanup"
    );
    let sess2_unbound_after_kick = ctx
        .dispatch_rpc(
            &mut sess2,
            "charUnboundMgr",
            None,
            "GetCharactersToSelect",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(sess2_unbound_after_kick.unwrap_err().code, "Unauthorized");

    // Re-login "demo" on a fresh live session (`sess_live_ban`) WITHOUT calling `/kick`, then run `/ban`
    // to prove `/ban` itself kicks and disconnects the live session (`feat:admin.gm-ban`, `feat:account.account-bans`)!
    let mut sess_live_ban = SessionState::new(10_104);
    let (ban_close_tx, mut ban_close_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    ctx.register_session_closer(sess_live_ban.session_id, ban_close_tx);
    ctx.dispatch_rpc(
        &mut sess_live_ban,
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
    .await
    .expect("Re-login demo before /ban");
    assert_eq!(
        ctx.db.get_online_session(1).map(|s| s.session_id),
        Some(sess_live_ban.session_id)
    );

    let ban_rsp = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(
                "/ban demo 3600 Automated botting violation".into(),
            )],
            &kw,
            None,
        )
        .await
        .expect("/ban demo 3600");
    let PyRep::KeyVal(ban_kv) = ban_rsp else {
        panic!("Expected KeyVal from /ban");
    };
    assert_eq!(
        ban_kv.get("kickedSessionID"),
        Some(&PyRep::Integer(sess_live_ban.session_id as i64)),
        "/ban must directly kick and report the target account's active live session"
    );
    assert_eq!(ban_kv.get("permanent"), Some(&PyRep::Bool(false)));
    let banned_until_unix = ban_kv.get("bannedUntilUnix").unwrap().as_i64().unwrap();
    let (ban_close_code, ban_close_msg) = ban_close_rx
        .try_recv()
        .expect("/ban must immediately send GPSTransportClosed disconnect notification to live session");
    assert_eq!(ban_close_code, "UserBanned");
    assert!(ban_close_msg.contains("Automated botting violation"));
    assert!(ctx.db.get_online_session(1).is_none());

    // Verify banned account is refused at login with UserBanned, reason, and expiry
    let banned_err = ctx
        .dispatch_rpc(
            &mut sess2,
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
        .await
        .expect_err("Banned account login must fail");
    assert_eq!(banned_err.code, "UserBanned");
    assert!(
        banned_err.message.contains("Automated botting violation")
            && banned_err
                .message
                .contains(&banned_until_unix.to_string()),
        "UserBanned error message must include ban reason and expiry timestamp: {}",
        banned_err.message
    );

    // Unban "demo", then test permanent `/ban demo permanent ...`
    ctx.dispatch_rpc(
        &mut gm_sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/unban demo".into())],
        &kw,
        None,
    )
    .await
    .expect("/unban demo");

    let perm_ban_rsp = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/ban demo permanent Permanent RMT ban".into())],
            &kw,
            None,
        )
        .await
        .expect("/ban demo permanent");
    assert_eq!(perm_ban_rsp.dict_get("permanent"), Some(&PyRep::Bool(true)));
    let perm_ban_err = ctx
        .dispatch_rpc(
            &mut sess2,
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
        .await
        .expect_err("Permanently banned account login must fail");
    assert_eq!(perm_ban_err.code, "UserBanned");
    assert!(perm_ban_err.message.contains("Permanent RMT ban"));

    // Unban "demo" via GM /unban command, log "demo" in on a live session (`sess_live_suspend`),
    // and test `/suspend` + `/unsuspend` (`feat:account.account-bans`, `feat:admin.gm-ban`)
    ctx.dispatch_rpc(
        &mut gm_sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/unban demo".into())],
        &kw,
        None,
    )
    .await
    .expect("/unban demo");

    let mut sess_live_suspend = SessionState::new(10_105);
    let (susp_close_tx, mut susp_close_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    ctx.register_session_closer(sess_live_suspend.session_id, susp_close_tx);
    ctx.dispatch_rpc(
        &mut sess_live_suspend,
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
    .await
    .expect("Login demo before /suspend");

    let susp_rsp = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(
                "/suspend demo 1800 Account sharing investigation".into(),
            )],
            &kw,
            None,
        )
        .await
        .expect("/suspend demo 1800");
    let PyRep::KeyVal(susp_kv) = susp_rsp else {
        panic!("Expected KeyVal from /suspend");
    };
    assert_eq!(
        susp_kv.get("kickedSessionID"),
        Some(&PyRep::Integer(sess_live_suspend.session_id as i64)),
        "/suspend must directly kick and report the target account's active live session"
    );
    let suspended_until_unix = susp_kv.get("suspendedUntilUnix").unwrap().as_i64().unwrap();
    let (susp_close_code, susp_close_msg) = susp_close_rx
        .try_recv()
        .expect("/suspend must immediately send GPSTransportClosed disconnect notification to live session");
    assert_eq!(susp_close_code, "AccountSuspended");
    assert!(susp_close_msg.contains("Account sharing investigation"));
    assert!(ctx.db.get_online_session(1).is_none());

    let suspended_err = ctx
        .dispatch_rpc(
            &mut sess2,
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
        .await
        .expect_err("Suspended account login must fail");
    assert_eq!(suspended_err.code, "AccountSuspended");
    assert!(
        suspended_err
            .message
            .contains("Account sharing investigation")
            && suspended_err
                .message
                .contains(&suspended_until_unix.to_string()),
        "AccountSuspended error message must include suspension reason and expiry timestamp: {}",
        suspended_err.message
    );

    ctx.dispatch_rpc(
        &mut gm_sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/unsuspend demo".into())],
        &kw,
        None,
    )
    .await
    .expect("/unsuspend demo");

    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    let now_unix = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            90_000_001,
            "evomu-launcher",
            "https://localhost/callback",
            vec!["esi-wallet.read_character_wallet.v1".into()],
            challenge,
            "S256",
            now_unix,
        )
        .expect("authorize_pkce");
    let token_bundle = ctx
        .db
        .exchange_pkce_code(&code, "evomu-launcher", verifier, now_unix + 1)
        .expect("exchange_pkce_code");
    assert!(
        matches!(
            ctx.db.verify_sso_access_token(
                &token_bundle.access_token,
                now_unix + 1 + token_bundle.expires_in
            ),
            Err(eve_db::DbError::InvalidCredentials)
        ),
        "Expired SSO JWT must be rejected at now_unix >= exp"
    );

    // R34-P0-1: Verify forged JWT (e.g. signed with another deployment's key or tampered payload) is strictly rejected
    let other_ctx = ServerContext::new();
    let other_code = other_ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            90_000_001,
            "evomu-launcher",
            "https://localhost/callback",
            vec!["esi-wallet.read_character_wallet.v1".into()],
            challenge,
            "S256",
            now_unix,
        )
        .expect("authorize_pkce on other_ctx");
    let other_bundle = other_ctx
        .db
        .exchange_pkce_code(&other_code, "evomu-launcher", verifier, now_unix + 1)
        .expect("exchange_pkce_code on other_ctx");
    let forged_login = ctx
        .dispatch_rpc(
            &mut sess2,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("__sso__".into()),
                PyRep::String(other_bundle.access_token),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        forged_login.unwrap_err().code,
        "AuthenticationFailed",
        "R34-P0-1: SSO JWT signed by a different deployment key must be rejected"
    );

    ctx.dispatch_rpc(
        &mut sess2,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("__sso__".into()),
            PyRep::String(token_bundle.access_token.clone()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login via OAuth2/PKCE SSO JWT token");
    assert_eq!(sess2.user_id, Some(1));

    // 4. D-3 & D-5: Load tracked evomu.toml and custom TOML config backed by durable SQLite world DB
    let tracked_cfg =
        ServerConfig::load_from_toml(root.join("evomu.toml")).expect("load tracked evomu.toml");
    assert_eq!(tracked_cfg.macho_port, 26_000);
    assert!(!tracked_cfg.feature_flags.seed_demo);

    let tmp_db = std::env::temp_dir().join(format!("evomu_d5_test_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&tmp_db);
    let custom_toml = format!(
        r#"
bind_address = "127.0.0.1"
macho_port = 26100
bridge_port = 26102
esi_port = 26104
sde_path = "{}"
world_db_path = "{}"

[feature_flags]
enable_quasar_gateway = false
enable_esi_http = true
enable_living_world = false
seed_demo = false

[[player_accounts]]
username = "capsuleer_toml"
password_token = "toml-pass-123"

[[admin_accounts]]
username = "admin_toml"
password_token = "admin-pass-456"
character_id = 90000099
character_name = "ISD Config"
"#,
        sde_path.display(),
        tmp_db.display()
    );
    let parsed_cfg = ServerConfig::from_toml_str(&custom_toml).expect("parse custom evomu.toml");
    let persisted_sso_token = {
        let cfg_ctx = ServerContext::from_config(&parsed_cfg).expect("ServerContext::from_config");
        let acc = cfg_ctx
            .db
            .authenticate("capsuleer_toml", "toml-pass-123")
            .expect("authenticate provisioned TOML player account");
        assert!(
            acc.password_token
                .starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "D-4: Provisioned account must use Argon2id m=19456,t=2,p=1 hash"
        );
        // Verify feature_flags.enable_quasar_gateway = false and enable_living_world = false are enforced
        let mut flag_sess = SessionState::new(10_200);
        let q_disabled = cfg_ctx
            .dispatch_rpc(
                &mut flag_sess,
                "publicGatewaySvc",
                None,
                "SendRequest",
                &[],
                &kw,
                None,
            )
            .await;
        assert_eq!(q_disabled.unwrap_err().code, "FeatureDisabled");
        let lw_disabled = cfg_ctx
            .dispatch_rpc(
                &mut flag_sess,
                "taleMgr",
                None,
                "MaterializeSystemLOD2",
                &[],
                &kw,
                None,
            )
            .await;
        assert_eq!(lw_disabled.unwrap_err().code, "FeatureDisabled");

        let p_code = cfg_ctx
            .db
            .authorize_pkce(
                "admin_toml",
                "admin-pass-456",
                90_000_099,
                "evomu-launcher",
                "https://localhost/callback",
                vec!["esi-wallet.read_character_wallet.v1".into()],
                challenge,
                "S256",
                now_unix,
            )
            .expect("authorize_pkce on cfg_ctx");
        cfg_ctx
            .db
            .exchange_pkce_code(&p_code, "evomu-launcher", verifier, now_unix + 1)
            .expect("exchange_pkce_code on cfg_ctx")
            .access_token
    };
    // Reopen SQLite world DB WITHOUT player_accounts/admin_accounts in config so nothing is re-provisioned on startup,
    // proving accounts AND the CSPRNG JWT signing secret survived restart in SQLite
    {
        let mut reopen_cfg = parsed_cfg.clone();
        reopen_cfg.player_accounts.clear();
        reopen_cfg.admin_accounts.clear();
        let reopened_ctx =
            ServerContext::from_config(&reopen_cfg).expect("Reopen ServerContext::from_config");
        assert!(reopened_ctx
            .db
            .authenticate("capsuleer_toml", "toml-pass-123")
            .is_ok());
        assert!(reopened_ctx
            .db
            .authenticate("admin_toml", "admin-pass-456")
            .is_ok());
        assert!(
            reopened_ctx
                .db
                .verify_sso_access_token(&persisted_sso_token, now_unix + 2)
                .is_ok(),
            "Persisted CSPRNG JWT secret in SQLite auth_secrets must verify SSO tokens across restart"
        );
    }
    let _ = std::fs::remove_file(&tmp_db);
}

#[tokio::test]
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

    // 1. Mismatched build version must be rejected with GPSTransportClosed (HANDSHAKE_INCOMPATIBLEBUILD)
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let srv_hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        assert_eq!(srv_hello, VersionExchange::default().to_pyrep());

        let mut bad_ver = VersionExchange::default();
        bad_ver.build_version = 9_999_999;
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
        let mut s = TcpStream::connect(addr).await.unwrap();
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
        let mut s = TcpStream::connect(addr).await.unwrap();
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

        let mut s = TcpStream::connect(addr).await.unwrap();
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
        let mut s2 = TcpStream::connect(addr).await.unwrap();
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
        let mut s = TcpStream::connect(addr).await.unwrap();
        let srv_hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let parsed_ver = VersionExchange::from_pyrep(&srv_hello).expect("server VersionExchange");
        assert_eq!(parsed_ver.birthday, 170_472);
        assert_eq!(parsed_ver.macho_version, 409);
        assert!((parsed_ver.version_number - 24.01).abs() < 1e-6);
        assert_eq!(parsed_ver.build_version, 3_619_879);
        assert_eq!(parsed_ver.project_version, "EVE-EVE-TRANQUILITY@ccp");

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
        assert_eq!(srv_hs.macho_version, 409);
        assert_eq!(srv_hs.boot_build, 3_619_879);
        assert_eq!(srv_hs.boot_codename, "EVE-EVE-TRANQUILITY");
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
