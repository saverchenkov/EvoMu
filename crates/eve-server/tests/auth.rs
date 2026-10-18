//! Authentication, service roles bitmasks, SSO tokens, and SDE persistence integration tests.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

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

    let ctx = ServerContext::new();
    let mut sess = SessionState::new(909);
    ctx.dispatch_rpc(
        &mut sess,
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
    .expect("Login");
    assert_eq!(
        sess.role,
        eve_machonet::service_roles::ROLE_PLAYER_DEFAULT,
        "S-9: Player login must assign 64-bit ROLE_PLAYER | ROLE_LOGIN"
    );
    assert!(!sess.has_gm_role());

    // Verify ROLE_CONTENT (1 << 4) does not grant GM access
    sess.role |= eve_machonet::service_roles::ROLE_CONTENT;
    assert!(
        !sess.has_gm_role(),
        "S-9: ROLE_CONTENT (1 << 4) must not be treated as a GM role"
    );

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    for (svc, mth) in [
        ("slash", "SlashCmd"),
        ("publicQaToolsServer", "Command"),
        ("devToolsProvider", "GetDevTools"),
        ("keeper", "GetLevelEditor"),
    ] {
        let denied = ctx
            .dispatch_rpc(
                &mut sess,
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

    sess.role |= eve_machonet::service_roles::ROLE_GML;
    assert!(sess.has_gm_role());
    ctx.dispatch_rpc(
        &mut sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/giveisk 100".into())],
        &kw,
        None,
    )
    .await
    .expect("S-9: Session with 64-bit ROLE_GML must pass GM check");
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
    assert_eq!(ctx.sde.npc_spawn_groups.len(), 392);
    assert!(
        !ctx.sde.variations_of(587).is_empty(),
        "D-2: SdeRegistry::variations_of(587) must return Rifter hull variations in O(1)"
    );

    // Authenticate session 1 as "demo"
    let mut sess1 = SessionState::new(10_101);
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
            "RequestMission",
            &[PyRep::Integer(3_012_018)],
            &kw,
            None,
        )
        .await
        .expect_err("RequestMission for remote Maurasi agent from Jita must fail");
    assert_eq!(remote_err.code, "AgentNotCoLocated");

    // Storyline agents (agentType 7 = 3_009_966, agentType 6 = 3_017_107) must be rejected even when co-located
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_004_969;
            ch.station_id = Some(60_011_920);
        }
    });
    let storyline_t7_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "agentMgr",
            None,
            "RequestMission",
            &[PyRep::Integer(3_009_966)],
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
    let storyline_t6_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "agentMgr",
            None,
            "RequestMission",
            &[PyRep::Integer(3_017_107)],
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
    let l2_standing_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "agentMgr",
            None,
            "RequestMission",
            &[PyRep::Integer(3_016_842)],
            &kw,
            None,
        )
        .await
        .expect_err(
            "Level 2 agent with 0.0 corp standing must fail with InsufficientAgentStanding",
        );
    assert_eq!(l2_standing_err.code, "InsufficientAgentStanding");

    // Default (no-arg) RequestMission in Maurasi (60_003_754) selects co-located L1 Security agent 3_012_018
    // and draws a BasicKillMission from that agent's division pool
    let maurasi_offer = ctx
        .dispatch_rpc(
            &mut sess1,
            "agentMgr",
            None,
            "RequestMission",
            &[],
            &kw,
            None,
        )
        .await
        .expect("Default RequestMission in Maurasi must select co-located L1 agent");
    let PyRep::KeyVal(maurasi_kv) = maurasi_offer else {
        panic!("Expected KeyVal from RequestMission");
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
    let epic_arc_offer = ctx
        .dispatch_rpc(
            &mut sess1,
            "agentMgr",
            None,
            "RequestMission",
            &[PyRep::Integer(3_019_356)],
            &kw,
            None,
        )
        .await
        .expect("Epic Arc agent 3_019_356 (type 10) must offer an EpicArc mission");
    let PyRep::KeyVal(epic_kv) = epic_arc_offer else {
        panic!("Expected KeyVal from Epic Arc RequestMission");
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
    let aura_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "agentMgr",
            None,
            "RequestMission",
            &[PyRep::Integer(3_019_486)],
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
            "CompleteMission",
            &[PyRep::Integer(no_standing_mid as i64)],
            &kw,
            None,
        )
        .await
        .expect("CompleteMission for non-standing mission");
    let PyRep::KeyVal(comp_no_st_kv) = comp_no_st else {
        panic!("Expected KeyVal from CompleteMission");
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

    // 2. D-4: Online session lock enforcement (`R34-P1-4`, `R35-P1-3`):
    // Second concurrent session (`sess2`) logging into "demo" cannot call game RPCs or bound-object RPCs until it displaces `sess1` via `multiLoginBlocker.Login(true)`.
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
    .expect("Login sess2");

    let premature_rpc = ctx
        .dispatch_rpc(
            &mut sess2,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        premature_rpc.unwrap_err().code,
        "MultiLoginBlocked",
        "R34-P1-4: Second session without online lock must be rejected with MultiLoginBlocked on game RPCs"
    );
    assert_eq!(sess2.user_id, Some(1));

    let dup_blocked = ctx
        .dispatch_rpc(
            &mut sess2,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::Bool(false)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        dup_blocked.unwrap_err().code,
        "AlreadyLoggedIn",
        "multiLoginBlocker.Login(false) must reject a second concurrent session on the same account"
    );

    ctx.dispatch_rpc(
        &mut sess2,
        "multiLoginBlocker",
        None,
        "Login",
        &[PyRep::Bool(true)],
        &kw,
        None,
    )
    .await
    .expect("multiLoginBlocker.Login(true) must displace the prior session");

    // R35-P1-3: Displaced sess1 must NOT be able to bypass revocation via a previously bound object
    // even if the client spoofs service="authentication" with bound_id=Some(bound_id_sess1)
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

    // 3. D-4: Admin /kick, /ban, and /unban slash commands + live session revocation + ROLE_ADMIN ban hierarchy
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

    // R36-P1-3: Kicked sess2 must NOT be able to bypass revocation via multiLoginBlocker.Login(true)
    // and must fail both multiLoginBlocker.Login and bound-object calls with SessionKicked / Unauthorized
    let sess2_mlb_after_kick = ctx
        .dispatch_rpc(
            &mut sess2,
            "multiLoginBlocker",
            None,
            "Login",
            &[PyRep::Bool(true)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        sess2_mlb_after_kick.unwrap_err().code,
        "SessionKicked",
        "R36-P1-3: Kicked session calling multiLoginBlocker.Login(true) must be rejected with SessionKicked"
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

    // Ban "demo" for 3600 seconds via GM /ban command
    ctx.dispatch_rpc(
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
    assert_eq!(banned_err.code, "AccountBanned");
    assert!(
        banned_err.message.contains("Automated botting violation")
            && banned_err.message.contains("until"),
        "AccountBanned error message must include ban reason and expiry timestamp: {}",
        banned_err.message
    );

    // Unban "demo" via GM /unban command and verify login via OAuth2 PKCE SSO JWT access token
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
