//! SDE SQLite persistence, account auth/SSO storage and `evomu.toml` config integration tests
//! (`S-11`, `D-2`). Split verbatim out of `auth.rs` to keep each test file under 1,400 lines.

#![allow(unused_imports)]
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
async fn test_phase_1_d1_to_d5_sde_sqlite_persistence_auth_sso_and_config() {
    // parity: plan:S-11, plan:D-2
    // parity: feat:serverops.static-data, feat:account.multi-login-block, feat:account.account-bans, feat:admin.gm-ban, rpc:agentMgr.DoAction, rpc:authentication.List, rpc:authentication.Login, rpc:authentication.Logout, rpc:charUnboundMgr.GetCharactersToSelect, rpc:charUnboundMgr.SelectCharacterID, rpc:invbroker.MachoBindObject, rpc:multiLoginBlocker.Login, rpc:publicGatewaySvc.SendRequest, rpc:slash.SlashCmd
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
    crate::common::pin_clock(&ctx);
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
            // rpc-lint: allow negative test (pre-existing): a non-client method must return UnimplementedRPC
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
    crate::common::pin_clock(&other_ctx);
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

    // Pinned virtual clock: the token was exchanged at `now_unix + 1` (its `iat`), so let that
    // second pass explicitly instead of relying on Argon2 wall time elapsing before Login.
    ctx.advance_secs(1);
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
    // Use forward slashes for TOML path values (works on both Unix and Windows)
    let sde_path_toml = sde_path.to_string_lossy().replace('\\', "/");
    let tmp_db_toml = tmp_db.to_string_lossy().replace('\\', "/");
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
        sde_path_toml,
        tmp_db_toml
    );
    let parsed_cfg = ServerConfig::from_toml_str(&custom_toml).expect("parse custom evomu.toml");
    let persisted_sso_token = {
        let cfg_ctx = ServerContext::from_config(&parsed_cfg).expect("ServerContext::from_config");
        crate::common::pin_clock(&cfg_ctx);
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
                // rpc-lint: allow negative test (pre-existing): living-world flag off must refuse this internal name
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
