//! Edge-case behavioral tests for `router::dispatch`.

use crate::router::{
    ServerContext, RPC_BOOTSTRAP_BURST_ALLOWANCE, RPC_BOOTSTRAP_GRANT_COOLDOWN,
    RPC_BOOTSTRAP_TRANSITION_ALLOWANCE, RPC_RATE_BURST_TOKENS, RPC_RATE_IDLE_TTL,
    RPC_RATE_PRUNE_THRESHOLD, RPC_RATE_REFILL_PER_SEC,
};
use eve_db::{VirtualServerClock, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn edge_ctx() -> ServerContext {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_750_000_000)));
    ctx
}

fn demo_session(ctx: &ServerContext, session_id: u64, role: u64) -> SessionState {
    let char_id = 90_000_001;
    let (uid, corp_id, sol_id, ship_id) = ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        (
            ch.user_id,
            ch.corporation_id,
            ch.solar_system_id,
            ch.active_ship_id,
        )
    });
    let mut s = SessionState {
        session_id,
        user_id: Some(uid),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sol_id),
        station_id: Some(60_003_760),
        ship_id: Some(ship_id),
        role,
        ..Default::default()
    };
    ctx.sync_session(&mut s);
    s
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_gate_exemptions_gm_bypass_and_session_wipe() {
    let ctx = edge_ctx();
    let kw = HashMap::new();

    // Mark cluster shutting down
    ctx.cluster_state.write().unwrap().shutdown_in_progress = true;

    // Exempt calls bypass shutdown gate (not rejected with ClusterShuttingDown)
    let mut player = demo_session(&ctx, 10, service_roles::ROLE_PLAYER);
    let ping_res = ctx
        .dispatch_rpc(&mut player, "authentication", None, "Ping", &[], &kw, None)
        .await;
    assert!(
        ping_res.is_ok(),
        "authentication.Ping must stay exempt and succeed during shutdown: {ping_res:?}"
    );
    let may_pet = ctx
        .dispatch_rpc(&mut player, "petitioner", None, "MayPetition", &[], &kw, None)
        .await;
    assert!(
        may_pet.is_ok(),
        "petitioner.MayPetition must stay exempt during shutdown: {may_pet:?}"
    );

    // GM session bypasses shutdown gate
    let mut gm = demo_session(
        &ctx,
        11,
        service_roles::ROLE_PLAYER | service_roles::ROLE_GML,
    );
    let gm_res = ctx
        .dispatch_rpc(
            &mut gm,
            "timeControl",
            None,
            "ClearSessionTimer",
            &[],
            &kw,
            None,
        )
        .await;
    assert!(gm_res.is_ok(), "GM must bypass shutdown gate: {gm_res:?}");

    // Non-GM gameplay call fails with ClusterShuttingDown and wipes session identity/location fields
    let err = ctx
        .dispatch_rpc(&mut player, "skillMgr2", None, "GetSkills", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "ClusterShuttingDown");
    assert!(player.user_id.is_none());
    assert!(player.character_id.is_none());
    assert!(player.corporation_id.is_none());
    assert!(player.station_id.is_none());
    assert!(player.solar_system_id.is_none());
    assert!(player.ship_id.is_none());
    assert_eq!(player.role, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slash_qa_and_gm_role_boundary_checks() {
    let ctx = edge_ctx();
    let kw = HashMap::new();

    // Non-GM/non-QA player can run slash.SlashCmd("/") (passes role check, returns SlashError "Commands: []")
    // but NOT other slash commands (InsufficientRole)
    let mut player = demo_session(&ctx, 20, service_roles::ROLE_PLAYER);
    let slash_list = ctx
        .dispatch_rpc(
            &mut player,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(slash_list.code, "SlashError");
    assert_eq!(slash_list.message, "Commands: []");
    let slash_denied = ctx
        .dispatch_rpc(
            &mut player,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/help".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(slash_denied.code, "InsufficientRole");

    // ROLE_QA passes central slash/QA gate (returns non-empty command list on '/' and is allowed past dispatch.rs),
    // yet is still denied GM-only services like timeControl and GM-only /help
    let mut qa = demo_session(&ctx, 21, service_roles::ROLE_PLAYER | service_roles::ROLE_QA);
    let qa_list = ctx
        .dispatch_rpc(
            &mut qa,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(qa_list.code, "SlashError");
    assert_ne!(
        qa_list.message, "Commands: []",
        "ROLE_QA should see QA commands in slash list, got: {qa_list:?}"
    );
    let qa_help_denied = ctx
        .dispatch_rpc(
            &mut qa,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/help".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(qa_help_denied.code, "InsufficientRole");

    let mut gm = demo_session(&ctx, 22, service_roles::ROLE_PLAYER | service_roles::ROLE_GML);
    let gm_help = ctx
        .dispatch_rpc(
            &mut gm,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/help".into())],
            &kw,
            None,
        )
        .await
        .expect("ROLE_GML /help must succeed");
    assert!(
        gm_help
            .as_str()
            .is_some_and(|s| s.contains("Available slash commands")),
        "ROLE_GML /help must return command summary string, got: {gm_help:?}"
    );
    let gm_unknown = ctx
        .dispatch_rpc(
            &mut gm,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/nonexistent_gm_cmd".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        gm_unknown.code, "UnknownSlashCommand",
        "ROLE_GML must pass role gate into slash dispatcher, got: {gm_unknown:?}"
    );

    let qa_gm_denied = ctx
        .dispatch_rpc(
            &mut qa,
            "timeControl",
            None,
            "ClearSessionTimer",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(qa_gm_denied.code, "InsufficientRole");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn macho_bind_validation_fleet_bound_wrap_quasar_and_location_transition() {
    let ctx = edge_ctx();
    let kw = HashMap::new();
    let mut player = demo_session(&ctx, 30, service_roles::ROLE_PLAYER);

    // MachoBindObject invalid empty-string bind key rejected with InvalidArgument
    let empty_bind_err = ctx
        .dispatch_rpc(
            &mut player,
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::String(String::new())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(empty_bind_err.code, "InvalidArgument");

    // MachoBindObject with non-string piggybacked method name rejected with InvalidArgument
    let bad_piggy = ctx
        .dispatch_rpc(
            &mut player,
            "beyonce",
            None,
            "MachoBindObject",
            &[
                PyRep::Integer(30_000_142),
                PyRep::Tuple(vec![PyRep::Integer(12345), PyRep::Tuple(vec![])]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(bad_piggy.code, "InvalidArgument");

    // publicGatewaySvc with unknown Quasar domain returns UnknownQuasarDomain
    let bad_quasar = ctx
        .dispatch_rpc(
            &mut player,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::String("nonexistent.invalid.domain".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(bad_quasar.code, "UnknownQuasarDomain");

    // fleetObjectHandler.CreateFleet with machoNet:26000 wraps returned fleet_id as bound object
    let bound_fleet = ctx
        .dispatch_rpc(
            &mut player,
            "fleetObjectHandler",
            None,
            "CreateFleet",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("CreateFleet succeeds");
    let has_bound = matches!(&bound_fleet, PyRep::SubStruct(_))
        || matches!(&bound_fleet, PyRep::Tuple(t) if !t.is_empty())
        || matches!(&bound_fleet, PyRep::Object { .. });
    assert!(
        has_bound,
        "expected bound object wrapper from CreateFleet with machoNet:26000, got {bound_fleet:?}"
    );

    // Location transition during sync_session invalidates previously bound objects
    let bind_id = ctx
        .bind_verified_sub_object(&player, "beyonce", 30_000_142)
        .expect("bind beyonce");
    assert!(ctx.bound_objects.read().unwrap().contains_key(&bind_id));

    // Change character's persisted station_id -> next dispatch_rpc triggers transition invalidation
    let char_id = player.character_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_761);
    });
    let _ = ctx
        .dispatch_rpc(&mut player, "skillMgr2", None, "GetSkills", &[], &kw, Some("machoNet:26000"))
        .await
        .expect("GetSkills succeeds");
    assert!(
        !ctx.bound_objects.read().unwrap().contains_key(&bind_id),
        "stale bound object should be invalidated across station change"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_mgr2_get_skills_live_machonet_converts_rowset_to_dict() {
    let ctx = edge_ctx();
    let kw = HashMap::new();
    let mut player = demo_session(&ctx, 40, service_roles::ROLE_PLAYER);

    let rep = ctx
        .dispatch_rpc(
            &mut player,
            "skillMgr2",
            None,
            "GetSkills",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetSkills succeeds");
    match rep {
        PyRep::Dict(entries) => {
            assert!(!entries.is_empty(), "expected non-empty skills dict");
            let (_, first_val) = &entries[0];
            assert!(
                first_val.dict_get("skillLevel").is_some()
                    && first_val.dict_get("skillRank").is_some()
                    && first_val.dict_get("effectiveSkillLevel").is_some(),
                "expected V24 client skill attributes in dict value, got {first_val:?}"
            );
        }
        other => panic!("expected Dict for machoNet:26000 GetSkills, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_and_call_req_collapse_unknown_and_preauth_latency_on_ops() {
    use crate::flight_recorder::UNKNOWN_RPC_LABEL;
    use crate::router::client_wire::dispatch_call_on_wire;
    use crate::sso::handle_http_request_with_peer;
    use std::net::{IpAddr, Ipv4Addr};

    let ctx = edge_ctx();
    let kw = HashMap::new();
    let mut player = demo_session(&ctx, 41, service_roles::ROLE_PLAYER);
    let sys_id = player.solar_system_id.expect("demo solar system") as i64;

    // 1. Known V24 RPCs dispatched over the live `CallReq` wire path retain distinct buckets.
    let known_calls = [
        ("machoNet", "GetTime", vec![]),
        ("machoNet", "GetGlobalConfig", vec![]),
        ("charUnboundMgr", "GetCharactersToSelect", vec![]),
        ("beyonce", "MachoBindObject", vec![PyRep::Integer(sys_id)]),
    ];
    for (svc, meth, args) in &known_calls {
        let out = dispatch_call_on_wire(&ctx, &mut player, svc, None, meth, args, &kw).await;
        assert!(
            out.rpc_result.is_ok(),
            "known V24 RPC {svc}.{meth} failed: {:?}",
            out.rpc_result
        );
    }

    // 2. Authenticated `CallReq` probes against unknown methods or arbitrary-service universal
    // hooks must collapse into `("<unknown>", "<unknown>")`.
    // rpc-lint: allow negative test probes with synthetic service/method names
    let auth_unknown_calls = [
        ("evil_wire_svc", "evil_wire_meth"),
        ("evil_wire_svc", "MachoResolveObject"),
        ("evil_wire_svc", "ClientHasReleasedTheseObjects"),
        ("evil_wire_svc", "MachoBindObject"),
    ];
    for (svc, meth) in &auth_unknown_calls {
        let _ = dispatch_call_on_wire(&ctx, &mut player, svc, None, meth, &[], &kw).await;
    }

    // 3. Unauthenticated flood of 10,000 unknown, 64 KiB, and pre-auth rejected RPCs must all
    // collapse into one `("<unknown>", "<unknown>")` bucket with zero per-method bloat.
    let mut unauth_sess = SessionState::new(901);
    let pad_64k = "z".repeat(65_536);
    for i in 0..10_000usize {
        let (svc, meth) = match i % 4 {
            0 => (format!("unknown_svc_{i}"), format!("{pad_64k}_{i}")),
            1 => (format!("unknown_svc_{i}"), format!("unknown_meth_{i}")),
            2 => (
                "charUnboundMgr".to_string(),
                "GetCharactersToSelect".to_string(),
            ),
            _ => (
                format!("unknown_rel_{i}"),
                "ClientHasReleasedTheseObjects".to_string(),
            ),
        };
        let _ = ctx
            .dispatch_rpc(
                &mut unauth_sess,
                &svc,
                None,
                &meth,
                &[],
                &kw,
                Some("machoNet:26000"),
            )
            .await;
    }

    // 4. Verify `/ops` JSON exposes the 4 known buckets plus a single `<unknown>` aggregate.
    ctx.set_gm_ops_enabled(true);
    let mut headers = HashMap::new();
    headers.insert("host".into(), "localhost:26004".into());
    let resp = handle_http_request_with_peer(
        &ctx,
        "GET",
        "/ops",
        &headers,
        b"",
        Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
    );
    assert_eq!(resp.status_code, 200);
    let body: serde_json::Value = serde_json::from_slice(&resp.body).expect("parse /ops json");
    let hist = &body["latency_histograms"];
    let expected_total = known_calls.len() + auth_unknown_calls.len() + 10_000;
    assert_eq!(hist["total_calls"].as_u64(), Some(expected_total as u64));

    let methods = hist["methods"].as_array().expect("latency methods array");
    assert_eq!(methods.len(), known_calls.len() + 1);
    for (svc, meth, _) in &known_calls {
        let count = methods
            .iter()
            .find(|m| m["service"] == *svc && m["method"] == *meth)
            .and_then(|m| m["count"].as_u64());
        assert_eq!(count, Some(1), "missing or wrong count for {svc}.{meth}");
    }
    let unknown_count = methods
        .iter()
        .find(|m| m["service"] == UNKNOWN_RPC_LABEL && m["method"] == UNKNOWN_RPC_LABEL)
        .and_then(|m| m["count"].as_u64());
    assert_eq!(
        unknown_count,
        Some((auth_unknown_calls.len() + 10_000) as u64)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forged_bound_id_and_kicked_session_probes_collapse_preserving_preauth_rpcs() {
    use crate::flight_recorder::{MAX_TRACKED_RPC_METHODS, UNKNOWN_RPC_LABEL};

    let ctx = edge_ctx();
    let kw = HashMap::new();

    // Victim binds a sub-object before any RPC latency buckets exist.
    let victim = demo_session(&ctx, 900, service_roles::ROLE_PLAYER);
    let existing_bid = ctx
        .bind_verified_sub_object(&victim, "beyonce", 30_000_142)
        .expect("bind victim beyonce sub-object");

    // Flood MAX_TRACKED_RPC_METHODS + 12 distinct pre-auth forged-bound-id probes sharded across
    // 16 unauth sessions (~129 calls/session, below per-session burst), plus forged-bound-id calls
    // targeting valid pre-auth RPC names and non-allowlisted pre-auth RPCs.
    let mut unauth_sessions: Vec<SessionState> =
        (0..16u64).map(|k| SessionState::new(901 + k)).collect();
    let mut expected_collapsed = 0u64;
    let flood_count = MAX_TRACKED_RPC_METHODS + 12;
    for i in 0..flood_count {
        let svc = format!("probe_svc_{i}");
        let meth = format!("probe_meth_{i}");
        let sess_idx = i % unauth_sessions.len();
        let sess = &mut unauth_sessions[sess_idx];
        let err = ctx
            .dispatch_rpc(sess, &svc, Some(existing_bid), &meth, &[], &kw, None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err.code.as_str(),
                "AuthenticationRequired" | "BoundObjectOwnershipViolation"
            ),
            "unexpected forged probe error: {err:?}"
        );
        expected_collapsed += 1;
    }
    let mut unauth = SessionState::new(920);

    let forged_preauth_calls: &[(&str, Option<u64>, &str, &[PyRep])] = &[
        ("machoNet", Some(existing_bid), "GetTime", &[]),
        (
            "authentication",
            Some(existing_bid),
            "Login",
            &[
                PyRep::String("demo".into()),
                PyRep::String("demo-secret".into()),
            ],
        ),
        (
            "cache",
            Some(existing_bid),
            "Rowset",
            &[],
        ),
        ("charUnboundMgr", None, "GetCharactersToSelect", &[]),
        ("skillMgr2", None, "GetSkills", &[]),
    ];
    for &(svc, bid, meth, args) in forged_preauth_calls {
        let err = ctx
            .dispatch_rpc(&mut unauth, svc, bid, meth, args, &kw, None)
            .await
            .unwrap_err();
        if bid == Some(existing_bid) {
            assert_eq!(err.code, "BoundObjectOwnershipViolation");
        }
        expected_collapsed += 1;
    }

    // Exempt pre-auth `machoNet.GetGlobalConfig` succeeds, yet must collapse to `<unknown>`
    // because unauthenticated individual tracking only permits `machoNet.{GetTime,GetServiceInfo,NoBlock}`.
    assert!(ctx
        .dispatch_rpc(&mut unauth, "machoNet", None, "GetGlobalConfig", &[], &kw, None)
        .await
        .is_ok());
    expected_collapsed += 1;

    // Unbound pre-auth `machoNet.ClientHasReleasedTheseObjects` succeeds, yet collapses to `<unknown>`.
    assert!(ctx
        .dispatch_rpc(
            &mut unauth,
            "machoNet",
            None,
            "ClientHasReleasedTheseObjects",
            &[],
            &kw,
            None,
        )
        .await
        .is_ok());
    expected_collapsed += 1;

    // Pre-auth `machoNet.ClientHasReleasedTheseObjects` with a forged bound_id must be rejected
    // by `verify_bound_object` and collapse to `<unknown>`.
    let forged_rel_err = ctx
        .dispatch_rpc(
            &mut unauth,
            "machoNet",
            Some(existing_bid),
            "ClientHasReleasedTheseObjects",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(forged_rel_err.code, "BoundObjectOwnershipViolation");
    expected_collapsed += 1;

    // Unauthenticated non-machoNet `ClientHasReleasedTheseObjects` must reject with `Unauthorized`.
    let non_macho_rel_err = ctx
        .dispatch_rpc(
            &mut unauth,
            "ship",
            None,
            "ClientHasReleasedTheseObjects",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(non_macho_rel_err.code, "Unauthorized");
    expected_collapsed += 1;

    // Authenticated cross-session forged bound_id must collapse to `<unknown>`, not attribute to `beyonce`.
    let other_uid = ctx
        .db
        .provision_player_account("other_probe_hf1184", "other-probe-secret");
    let mut other_sess = SessionState::new(902);
    other_sess.user_id = Some(other_uid);
    other_sess.character_id = Some(90_000_002);
    other_sess.role = service_roles::ROLE_PLAYER;
    let cross_err = ctx
        .dispatch_rpc(
            &mut other_sess,
            "beyonce",
            Some(existing_bid),
            "GetFormations",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(cross_err.code, "BoundObjectOwnershipViolation");
    expected_collapsed += 1;

    // Authenticated arbitrary non-cataloged service with universal release method collapses.
    assert!(ctx
        .dispatch_rpc(
            &mut other_sess,
            "bogus_noncataloged_svc_hf1184",
            None,
            "ClientHasReleasedTheseObjects",
            &[],
            &kw,
            None,
        )
        .await
        .is_ok());
    expected_collapsed += 1;

    // Kicked session probes (both pre-auth `machoNet.GetTime` and bound-object/authed calls) must
    // return `SessionKicked` and collapse into `<unknown>`.
    let kicked_uid1 = ctx
        .db
        .provision_player_account("kicked_probe1_hf1184", "kicked-probe1-secret");
    let mut kicked_sess1 = SessionState::new(903);
    kicked_sess1.user_id = Some(kicked_uid1);
    kicked_sess1.role = service_roles::ROLE_PLAYER;
    ctx.db
        .kick_user(kicked_uid1, "GM kick test", ctx.now_unix());
    let kicked_time_err = ctx
        .dispatch_rpc(&mut kicked_sess1, "machoNet", None, "GetTime", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(kicked_time_err.code, "SessionKicked");
    assert!(!kicked_sess1.is_authenticated());
    expected_collapsed += 1;

    let kicked_uid2 = ctx
        .db
        .provision_player_account("kicked_probe2_hf1184", "kicked-probe2-secret");
    let mut kicked_sess2 = SessionState::new(904);
    kicked_sess2.user_id = Some(kicked_uid2);
    kicked_sess2.character_id = Some(90_000_002);
    kicked_sess2.role = service_roles::ROLE_PLAYER;
    let kicked_bid = ctx
        .bind_verified_sub_object(&kicked_sess2, "beyonce", 30_000_142)
        .expect("bind kicked_sess2 beyonce");
    ctx.db
        .kick_user(kicked_uid2, "GM kick test 2", ctx.now_unix());
    let kicked_bound_err = ctx
        .dispatch_rpc(
            &mut kicked_sess2,
            "beyonce",
            Some(kicked_bid),
            "GetFormations",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(kicked_bound_err.code, "SessionKicked");
    expected_collapsed += 1;

    // None of the forged bound_id, cross-session, or kicked probes may occupy any individual bucket.
    let pre_legit = ctx.flight_recorder.latency_histogram_report();
    assert_eq!(pre_legit.total_calls, expected_collapsed);
    assert_eq!(pre_legit.methods.len(), 1);
    assert_eq!(pre_legit.methods[0].service, UNKNOWN_RPC_LABEL);
    assert_eq!(pre_legit.methods[0].method, UNKNOWN_RPC_LABEL);
    assert_eq!(pre_legit.methods[0].count, expected_collapsed);

    // Valid pre-auth RPCs (`machoNet.GetTime` and `authentication.Login`) still receive individual buckets.
    let mut legit_sess = SessionState::new(905);
    assert!(ctx
        .dispatch_rpc(&mut legit_sess, "machoNet", None, "GetTime", &[], &kw, None)
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut legit_sess,
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
        .is_ok());
    assert!(ctx
        .dispatch_rpc(&mut legit_sess, "authentication", None, "Logout", &[], &kw, None)
        .await
        .is_ok());

    let post_legit = ctx.flight_recorder.latency_histogram_report();
    assert_eq!(post_legit.total_calls, expected_collapsed + 3);
    assert_eq!(post_legit.methods.len(), 4);
    for (svc, meth) in [
        ("machoNet", "GetTime"),
        ("authentication", "Login"),
        ("authentication", "Logout"),
    ] {
        assert_eq!(
            post_legit
                .methods
                .iter()
                .find(|m| m.service == svc && m.method == meth)
                .map(|m| m.count),
            Some(1),
            "missing individual bucket for {svc}.{meth}"
        );
    }
    assert_eq!(
        post_legit
            .methods
            .iter()
            .find(|m| m.service == UNKNOWN_RPC_LABEL && m.method == UNKNOWN_RPC_LABEL)
            .map(|m| m.count),
        Some(expected_collapsed)
    );
}

#[test]
fn session_rpc_rate_bucket_burst_refill_and_isolation() {
    let ctx = edge_ctx();
    let t0 = std::time::Instant::now();
    let sid_a = 9_101;
    let sid_b = 9_102;

    // Bootstrap burst allowance handles 100+ login/undock bootstrap calls without eating
    // into standard RPC burst capacity.
    for _ in 0..RPC_BOOTSTRAP_BURST_ALLOWANCE {
        assert!(ctx.try_consume_rpc_token_at(sid_a, t0, true));
    }

    let burst = RPC_RATE_BURST_TOKENS as usize;
    for i in 0..burst {
        assert!(
            ctx.try_consume_rpc_token_at(sid_a, t0, false),
            "expected burst token {i} to be available"
        );
    }
    assert!(
        !ctx.try_consume_rpc_token_at(sid_a, t0, false),
        "expected bucket exhaustion after {burst} calls"
    );

    // Another session's bucket stays completely unaffected.
    assert!(ctx.try_consume_rpc_token_at(sid_b, t0, false));

    // After 1 second of elapsed monotonic time, exactly `RPC_RATE_REFILL_PER_SEC` tokens refill.
    let t1 = t0 + std::time::Duration::from_secs(1);
    let refilled = RPC_RATE_REFILL_PER_SEC as usize;
    for i in 0..refilled {
        assert!(
            ctx.try_consume_rpc_token_at(sid_a, t1, false),
            "expected refilled token {i} after 1s"
        );
    }
    assert!(!ctx.try_consume_rpc_token_at(sid_a, t1, false));

    // Closing the connection cleans up the bucket and resets quota on reconnect.
    ctx.release_session_rate_bucket(sid_a);
    assert!(ctx.try_consume_rpc_token_at(sid_a, t1, false));
}

#[tokio::test]
async fn dispatch_rpc_rate_limits_tight_loop_without_starving_other_sessions() {
    let ctx = edge_ctx();
    let kw = HashMap::new();
    let mut attacker = demo_session(&ctx, 9_201, service_roles::ROLE_PLAYER);
    let mut bystander = demo_session(&ctx, 9_202, service_roles::ROLE_PLAYER);

    // Bootstrap burst of 150 calls passes cleanly without throttling.
    for _ in 0..75 {
        assert!(ctx
            .dispatch_rpc(&mut attacker, "machoNet", None, "GetTime", &[], &kw, None)
            .await
            .is_ok());
        assert!(ctx
            .dispatch_rpc(
                &mut attacker,
                "machoNet",
                None,
                "GetGlobalConfig",
                &[],
                &kw,
                None,
            )
            .await
            .is_ok());
    }

    // Tight loop of non-bootstrap RPCs exhausts the standard burst and triggers `RateLimited`.
    let total_calls = RPC_RATE_BURST_TOKENS as usize + 120;
    let mut allowed = 0usize;
    let mut rate_limited = 0usize;
    for _ in 0..total_calls {
        match ctx
            .dispatch_rpc(&mut attacker, "account", None, "GetCashBalance", &[], &kw, None)
            .await
        {
            Ok(_) => allowed += 1,
            Err(err) => {
                assert_eq!(err.code, "RateLimited", "unexpected error: {err:?}");
                rate_limited += 1;
            }
        }
    }
    assert!(
        allowed >= RPC_RATE_BURST_TOKENS as usize,
        "expected full burst before throttling, allowed={allowed}"
    );
    assert!(
        rate_limited > 0,
        "expected tight loop past burst to be rejected with RateLimited"
    );

    // Bystander session remains unaffected and unthrottled.
    assert!(ctx
        .dispatch_rpc(&mut bystander, "account", None, "GetCashBalance", &[], &kw, None)
        .await
        .is_ok());
}

#[tokio::test]
async fn dispatch_rpc_gm_soak_exemption_requires_gm_mode_and_gm_role() {
    let ctx = edge_ctx();
    let kw = HashMap::new();
    let gm_role = service_roles::ROLE_ADMIN | service_roles::ROLE_GMH | service_roles::ROLE_LOGIN;
    let mut gm_no_flag = demo_session(&ctx, 9_301, gm_role);
    let mut gm_with_flag = demo_session(&ctx, 9_302, gm_role);
    let mut normal_player = demo_session(&ctx, 9_303, service_roles::ROLE_PLAYER);

    let loop_calls = RPC_RATE_BURST_TOKENS as usize + 50;

    // Without `--gm`, even a GM role session hits `RateLimited` beyond burst.
    ctx.set_gm_ops_enabled(false);
    let mut saw_rate_limit = false;
    for _ in 0..loop_calls {
        if let Err(err) = ctx
            .dispatch_rpc(&mut gm_no_flag, "account", None, "GetCashBalance", &[], &kw, None)
            .await
        {
            assert_eq!(err.code, "RateLimited");
            saw_rate_limit = true;
            break;
        }
    }
    assert!(saw_rate_limit, "GM role without --gm must still be rate-limited");

    // With `--gm` active, GM soak sessions bypass throttling while regular players do not.
    ctx.set_gm_ops_enabled(true);
    for i in 0..(loop_calls + 100) {
        ctx.dispatch_rpc(&mut gm_with_flag, "account", None, "GetCashBalance", &[], &kw, None)
            .await
            .unwrap_or_else(|e| panic!("--gm soak call {i} unexpectedly failed: {e:?}"));
    }
    let mut player_limited = false;
    for _ in 0..loop_calls {
        if let Err(err) = ctx
            .dispatch_rpc(&mut normal_player, "account", None, "GetCashBalance", &[], &kw, None)
            .await
        {
            assert_eq!(err.code, "RateLimited");
            player_limited = true;
            break;
        }
    }
    assert!(
        player_limited,
        "regular player session must remain rate-limited even when --gm is active"
    );

    // BF-1186B-01: non-GM callers targeting `slash` (unbound or bound) must NOT bypass throttling under `--gm`.
    // Keep advancing through any early role/syntax `Err` replies until burst depletion triggers `RateLimited`.
    let mut player_slash_unbound = demo_session(&ctx, 9_304, service_roles::ROLE_PLAYER);
    let mut slash_unbound_limited = false;
    for _ in 0..loop_calls {
        if let Err(err) = ctx
            .dispatch_rpc(
                &mut player_slash_unbound,
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String("/".into())],
                &kw,
                None,
            )
            .await
        {
            if err.code == "RateLimited" {
                slash_unbound_limited = true;
                break;
            }
        }
    }
    assert!(
        slash_unbound_limited,
        "ROLE_PLAYER calling unbound `slash` must still be rate-limited under --gm"
    );

    let mut player_slash_bound = demo_session(&ctx, 9_305, service_roles::ROLE_PLAYER);
    let slash_bid = ctx
        .bind_verified_sub_object(&player_slash_bound, "slash", 30_000_142)
        .expect("bind slash for ROLE_PLAYER");
    let mut slash_bound_limited = false;
    for _ in 0..loop_calls {
        if let Err(err) = ctx
            .dispatch_rpc(
                &mut player_slash_bound,
                "slash",
                Some(slash_bid),
                "SlashCmd",
                &[PyRep::String("/".into())],
                &kw,
                None,
            )
            .await
        {
            if err.code == "RateLimited" {
                slash_bound_limited = true;
                break;
            }
        }
    }
    assert!(
        slash_bound_limited,
        "ROLE_PLAYER calling bound `slash` must still be rate-limited under --gm"
    );
}

#[tokio::test]
async fn dispatch_rpc_login_and_select_char_charge_standard_bucket_and_gate_bootstrap_grants() {
    let ctx = edge_ctx();
    let kw = HashMap::new();

    // BF-1186B-02: Login, SelectCharacterID, and heavy config RPCs (`GetMap`,
    // `GetAverageMarketPrices`) do not consume the bootstrap burst pool, whereas explicit light
    // login/char-select `config` methods do.
    assert!(!ServerContext::is_bootstrap_burst_rpc("authentication", "Login"));
    assert!(!ServerContext::is_bootstrap_burst_rpc(
        "charUnboundMgr",
        "SelectCharacterID"
    ));
    assert!(!ServerContext::is_bootstrap_burst_rpc("config", "GetMap"));
    assert!(!ServerContext::is_bootstrap_burst_rpc(
        "config",
        "GetAverageMarketPrices"
    ));
    assert!(ServerContext::is_bootstrap_burst_rpc(
        "config",
        "GetBlackListedPlanets"
    ));
    assert!(ServerContext::is_bootstrap_burst_rpc(
        "config",
        "GetMultiOwnersEx"
    ));

    let burst = RPC_RATE_BURST_TOKENS as usize;

    // `authentication.Login` consumes standard burst tokens (never bootstrap) and cannot draw
    // from `bootstrap_tokens` when the standard bucket is empty.
    let mut login_spam = SessionState::new(9_401);
    ctx.dispatch_rpc(
        &mut login_spam,
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
    .expect("initial Login must succeed");
    assert!(login_spam.user_id.is_some());
    assert_eq!(
        ctx.bootstrap_tokens_remaining(login_spam.session_id),
        Some(RPC_BOOTSTRAP_BURST_ALLOWANCE),
        "Login must not draw from bootstrap_tokens"
    );

    // Exhaust bootstrap_tokens intentionally and confirm repeat `Login` on same user_id
    // does NOT grant transition bootstrap tokens.
    for _ in 0..RPC_BOOTSTRAP_BURST_ALLOWANCE {
        ctx.dispatch_rpc(&mut login_spam, "machoNet", None, "GetTime", &[], &kw, None)
            .await
            .expect("bootstrap call succeeds");
    }
    assert_eq!(ctx.bootstrap_tokens_remaining(login_spam.session_id), Some(0));
    ctx.dispatch_rpc(
        &mut login_spam,
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
    .expect("repeat Login before standard burst exhaustion succeeds");
    assert_eq!(
        ctx.bootstrap_tokens_remaining(login_spam.session_id),
        Some(0),
        "repeat Login for unchanged user_id must not replenish bootstrap tokens"
    );

    // Select a character so we can drain standard tokens cleanly with `account.GetCashBalance`
    // while keeping bootstrap tokens full, then verify both `Login` and `SelectCharacterID`
    // reject with `RateLimited` instead of consuming `bootstrap_tokens`.
    let mut select_spam = SessionState::new(9_402);
    ctx.dispatch_rpc(
        &mut select_spam,
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
    .expect("pre-select Login succeeds");
    ctx.dispatch_rpc(
        &mut select_spam,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("initial SelectCharacterID succeeds");
    assert_eq!(
        ctx.bootstrap_tokens_remaining(select_spam.session_id),
        Some(RPC_BOOTSTRAP_BURST_ALLOWANCE),
        "SelectCharacterID must charge standard bucket, leaving bootstrap_tokens untouched"
    );
    // Drain bootstrap tokens too, repeat `SelectCharacterID` on the same char, and verify it
    // does NOT restore bootstrap_tokens.
    for _ in 0..RPC_BOOTSTRAP_BURST_ALLOWANCE {
        ctx.dispatch_rpc(&mut select_spam, "machoNet", None, "GetTime", &[], &kw, None)
            .await
            .expect("bootstrap call succeeds");
    }
    assert_eq!(ctx.bootstrap_tokens_remaining(select_spam.session_id), Some(0));
    ctx.dispatch_rpc(
        &mut select_spam,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("repeat SelectCharacterID within standard burst succeeds");
    assert_eq!(
        ctx.bootstrap_tokens_remaining(select_spam.session_id),
        Some(0),
        "repeat SelectCharacterID for same character must not replenish bootstrap tokens"
    );

    // Fresh logged-in session with untouched 512 bootstrap tokens, standard burst drained:
    // both `Login` and `SelectCharacterID` immediately fail with `RateLimited`.
    let ctx_std = edge_ctx();
    let mut empty_std = demo_session(&ctx_std, 9_405, service_roles::ROLE_PLAYER);
    for _ in 0..(burst + 20) {
        if let Err(err) = ctx_std
            .dispatch_rpc(&mut empty_std, "account", None, "GetCashBalance", &[], &kw, None)
            .await
        {
            assert_eq!(err.code, "RateLimited");
            break;
        }
    }
    assert_eq!(
        ctx_std.bootstrap_tokens_remaining(empty_std.session_id),
        Some(RPC_BOOTSTRAP_BURST_ALLOWANCE)
    );
    let login_err = ctx_std
        .dispatch_rpc(
            &mut empty_std,
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
        .unwrap_err();
    assert_eq!(
        login_err.code, "RateLimited",
        "Login must not bypass empty standard bucket via bootstrap_tokens"
    );
    let select_err = ctx_std
        .dispatch_rpc(
            &mut empty_std,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        select_err.code, "RateLimited",
        "SelectCharacterID must not bypass empty standard bucket via bootstrap_tokens"
    );

    // Repeated `stationSvc.Undock` while already in space (`station_id: None`, `structure_id: None`
    // in both DB and session) must NOT grant transition bootstrap tokens.
    let ctx_space = edge_ctx();
    ctx_space.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let mut in_space = demo_session(&ctx_space, 9_403, service_roles::ROLE_PLAYER);
    in_space.station_id = None;
    in_space.structure_id = None;
    in_space.solar_system_id = Some(30_000_142);
    for _ in 0..RPC_BOOTSTRAP_BURST_ALLOWANCE {
        ctx_space
            .dispatch_rpc(&mut in_space, "machoNet", None, "GetTime", &[], &kw, None)
            .await
            .expect("drain bootstrap pool");
    }
    assert_eq!(ctx_space.bootstrap_tokens_remaining(in_space.session_id), Some(0));
    let _ = ctx_space
        .dispatch_rpc(&mut in_space, "stationSvc", None, "Undock", &[], &kw, None)
        .await;
    assert_eq!(
        ctx_space.bootstrap_tokens_remaining(in_space.session_id),
        Some(0),
        "calling Undock when already in space must not replenish bootstrap tokens"
    );

    // Cooldown prevents repeated transition grants within `RPC_BOOTSTRAP_GRANT_COOLDOWN`.
    let t0 = std::time::Instant::now();
    assert!(ctx.try_consume_rpc_token_at(9_404, t0, true));
    for _ in 0..(RPC_BOOTSTRAP_BURST_ALLOWANCE - 1) {
        assert!(ctx.try_consume_rpc_token_at(9_404, t0, true));
    }
    assert_eq!(ctx.bootstrap_tokens_remaining(9_404), Some(0));
    assert!(ctx.grant_session_bootstrap_burst_at(9_404, t0));
    assert_eq!(
        ctx.bootstrap_tokens_remaining(9_404),
        Some(RPC_BOOTSTRAP_TRANSITION_ALLOWANCE)
    );
    for _ in 0..RPC_BOOTSTRAP_TRANSITION_ALLOWANCE {
        assert!(ctx.try_consume_rpc_token_at(9_404, t0, true));
    }
    assert_eq!(ctx.bootstrap_tokens_remaining(9_404), Some(0));
    let t_soon = t0 + std::time::Duration::from_secs(1);
    assert!(
        !ctx.grant_session_bootstrap_burst_at(9_404, t_soon),
        "cooldown must suppress immediate repeat transition bootstrap grant"
    );
    assert_eq!(ctx.bootstrap_tokens_remaining(9_404), Some(0));
    let t_ready = t0 + RPC_BOOTSTRAP_GRANT_COOLDOWN;
    assert!(ctx.grant_session_bootstrap_burst_at(9_404, t_ready));
    assert_eq!(
        ctx.bootstrap_tokens_remaining(9_404),
        Some(RPC_BOOTSTRAP_TRANSITION_ALLOWANCE)
    );
}

#[test]
fn session_rpc_rate_bucket_evicts_idle_bootstrap_entries_past_ttl() {
    let ctx = edge_ctx();
    let t0 = std::time::Instant::now();

    // Create > `RPC_RATE_PRUNE_THRESHOLD` sessions that each consume 1 bootstrap token
    // (`bootstrap_tokens` drops to 511 < 512 while standard `tokens` stays 300.0).
    let idle_sessions = RPC_RATE_PRUNE_THRESHOLD + 25;
    for offset in 0..idle_sessions {
        let sid = 10_000 + offset as u64;
        assert!(ctx.try_consume_rpc_token_at(sid, t0, true));
    }
    assert_eq!(ctx.rpc_rate_bucket_count(), idle_sessions);

    // Recent active throttled session added 1s before TTL expiry must be retained.
    let t_after_ttl = t0 + RPC_RATE_IDLE_TTL;
    let t_recent = t_after_ttl - std::time::Duration::from_secs(1);
    let sid_active = 20_000_u64;
    for _ in 0..(RPC_RATE_BURST_TOKENS as usize) {
        assert!(ctx.try_consume_rpc_token_at(sid_active, t_recent, false));
    }

    // Inserting a new session once age >= `RPC_RATE_IDLE_TTL` triggers pruning that evicts all
    // stale bootstrap-touched entries before `refill`, keeping only recent active entries.
    let sid_fresh = 20_001_u64;
    assert!(ctx.try_consume_rpc_token_at(sid_fresh, t_after_ttl, false));
    assert_eq!(
        ctx.rpc_rate_bucket_count(),
        2,
        "all stale bootstrap-touched buckets idle >= RPC_RATE_IDLE_TTL must be evicted"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn time_control_target_char_id_routing_fail_closed_and_role_gates() {
    use crate::services::space_destiny::{get_session_change_timer, record_session_change_timer};
    use eve_destiny::{Ball, MovementMode};
    use glam::DVec3;

    let ctx = edge_ctx();
    let kw = HashMap::new();
    let gm_cid = 90_000_001_i64;
    let player_cid = 90_000_002_i64;
    let gm_sys = 30_000_142_u32;
    let player_sys = 30_002_187_u32;

    let (gm_ship, player_ship) = ctx.db.with_mut_snapshot(|snap| {
        let gm_ship = snap.characters[&gm_cid].active_ship_id;
        let player_ch = snap.characters.get_mut(&player_cid).expect("player char");
        player_ch.solar_system_id = player_sys;
        player_ch.station_id = None;
        player_ch.structure_id = None;
        let p_ship = player_ch.active_ship_id;
        (gm_ship, p_ship)
    });

    let _ = ctx
        .systems
        .get_or_spawn(gm_sys)
        .spawn_ball(Ball {
            id: gm_ship,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::new(200.0, 0.0, 0.0),
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 1.0,
            mode: MovementMode::GotoDirection { direction: DVec3::X },
        })
        .await;
    let _ = ctx
        .systems
        .get_or_spawn(player_sys)
        .spawn_ball(Ball {
            id: player_ship,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::new(200.0, 0.0, 0.0),
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 1.0,
            mode: MovementMode::GotoDirection { direction: DVec3::X },
        })
        .await;

    let mut gm = demo_session(&ctx, 701, service_roles::ROLE_ADMIN | service_roles::ROLE_GML);
    gm.solar_system_id = Some(gm_sys);
    gm.ship_id = Some(gm_ship);

    let mut player = SessionState {
        session_id: 702,
        user_id: Some(2),
        character_id: Some(player_cid),
        solar_system_id: Some(player_sys),
        ship_id: Some(player_ship),
        role: service_roles::ROLE_PLAYER_DEFAULT,
        ..Default::default()
    };

    // Non-GM caller must be rejected even with an explicit target_char_id.
    for (method, args) in [
        ("AdvanceClock", vec![PyRep::Integer(10), PyRep::Integer(player_cid)]),
        ("ClearSessionTimer", vec![PyRep::Integer(player_cid)]),
        ("FastForwardSessionTimer", vec![PyRep::Integer(player_cid)]),
    ] {
        let err = ctx
            .dispatch_rpc(&mut player, "timeControl", None, method, &args, &kw, None)
            .await
            .expect_err("non-GM caller must be denied");
        assert_eq!(err.code, "InsufficientRole", "{method}");
    }

    let t0 = ctx.clock().now_unix_secs();
    record_session_change_timer(&ctx, gm_cid, t0, gm_sys, 1, "dock");
    record_session_change_timer(&ctx, player_cid, t0, player_sys, 1, "stargate_jump");

    // Unknown target_cid fails closed BEFORE mutating clock or timers; <=0 target is InvalidArgument.
    let unknown_adv = ctx
        .dispatch_rpc(
            &mut gm,
            "timeControl",
            None,
            "AdvanceClock",
            &[PyRep::Integer(25), PyRep::Integer(999_888_777)],
            &kw,
            None,
        )
        .await
        .expect_err("unknown AdvanceClock target must fail closed");
    assert_eq!(unknown_adv.code, "CharacterNotFound");
    assert_eq!(ctx.clock().now_unix_secs(), t0, "failed AdvanceClock must not advance clock");

    let unknown_clr = ctx
        .dispatch_rpc(
            &mut gm,
            "timeControl",
            None,
            "ClearSessionTimer",
            &[PyRep::Integer(999_888_777)],
            &kw,
            None,
        )
        .await
        .expect_err("unknown ClearSessionTimer target must fail closed");
    assert_eq!(unknown_clr.code, "CharacterNotFound");
    assert!(get_session_change_timer(&ctx, gm_cid).is_some());
    assert!(get_session_change_timer(&ctx, player_cid).is_some());

    let bad_arg = ctx
        .dispatch_rpc(
            &mut gm,
            "timeControl",
            None,
            "ClearSessionTimer",
            &[PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect_err("non-positive target_char_id must return InvalidArgument");
    assert_eq!(bad_arg.code, "InvalidArgument");

    // ClearSessionTimer(player_cid) and FastForwardSessionTimer(player_cid) mutate ONLY player_cid.
    ctx.dispatch_rpc(
        &mut gm,
        "timeControl",
        None,
        "ClearSessionTimer",
        &[PyRep::Integer(player_cid)],
        &kw,
        None,
    )
    .await
    .expect("ClearSessionTimer(player_cid) succeeds");
    assert!(get_session_change_timer(&ctx, player_cid).is_none());
    assert!(get_session_change_timer(&ctx, gm_cid).is_some(), "GM timer must remain intact");

    record_session_change_timer(&ctx, player_cid, t0, player_sys, 2, "stargate_jump");
    ctx.dispatch_rpc(
        &mut gm,
        "timeControl",
        None,
        "FastForwardSessionTimer",
        &[PyRep::Integer(player_cid)],
        &kw,
        None,
    )
    .await
    .expect("FastForwardSessionTimer(player_cid) succeeds");
    assert!(get_session_change_timer(&ctx, player_cid).is_none());
    assert!(get_session_change_timer(&ctx, gm_cid).is_some(), "GM timer must remain intact");

    // AdvanceClock(20, player_cid) advances wall clock and fast-forwards ONLY player_cid's ship.
    ctx.dispatch_rpc(
        &mut gm,
        "timeControl",
        None,
        "AdvanceClock",
        &[PyRep::Integer(20), PyRep::Integer(player_cid)],
        &kw,
        None,
    )
    .await
    .expect("AdvanceClock(20, player_cid) succeeds");
    assert_eq!(ctx.clock().now_unix_secs(), t0 + 20);
    let (_, player_ball) = ctx
        .systems
        .get_or_spawn(player_sys)
        .get_ball_snapshot(player_ship)
        .await
        .expect("player sys ok")
        .expect("player ship ball exists");
    let (_, gm_ball) = ctx
        .systems
        .get_or_spawn(gm_sys)
        .get_ball_snapshot(gm_ship)
        .await
        .expect("gm sys ok")
        .expect("gm ship ball exists");
    assert!(
        player_ball.position.x > 1_000.0,
        "expected player ship in system {player_sys} to advance, got {:?}",
        player_ball.position
    );
    assert_eq!(
        gm_ball.position,
        DVec3::ZERO,
        "GM ship in system {gm_sys} must not be fast-forwarded by targeted AdvanceClock"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_has_released_objects_routes_through_macho_net_table_and_unlisted_returns_unimplemented() {
    use crate::router::rpc_registry::{
        rpc_entry_with_caller, RpcCallerClass, RpcDispatchHandler, RpcEntry, RpcRegistry,
    };
    use eve_proto::BridgeRpcError;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static MACHO_RELEASE_HITS: AtomicUsize = AtomicUsize::new(0);

    fn spy_macho_release(
        ctx: &ServerContext,
        session: &mut SessionState,
        _service: &str,
        _method: &str,
        args: &[PyRep],
        _kwargs: &HashMap<String, PyRep>,
    ) -> Option<Result<PyRep, BridgeRpcError>> {
        MACHO_RELEASE_HITS.fetch_add(1, Ordering::SeqCst);
        Some(crate::services::macho_cluster_mgmt::handle_client_has_released_objects(
            ctx, session, args,
        ))
    }

    // 1. Authenticated `ClientHasReleasedTheseObjects` under another service name (`stationSvc`)
    //    routes through the `machoNet` table entry rather than a post-table fallback.
    static SPY_RELEASE_SLICE: &[RpcEntry] = &[rpc_entry_with_caller(
        "machoNet",
        "ClientHasReleasedTheseObjects",
        RpcCallerClass::PreAuth,
        RpcDispatchHandler::Sync(spy_macho_release),
    )];

    MACHO_RELEASE_HITS.store(0, Ordering::SeqCst);
    let mut spy_ctx = edge_ctx();
    spy_ctx.rpc_registry = Arc::new(RpcRegistry::from_slices(&[SPY_RELEASE_SLICE]));
    let mut sess = demo_session(&spy_ctx, 94_980, service_roles::ROLE_PLAYER_DEFAULT);
    let kw = HashMap::new();

    let bid = spy_ctx
        .bind_object(&sess, "beyonce", 30_000_142)
        .expect("bind beyonce");
    let release_arg = PyRep::List(vec![PyRep::String(format!(
        "N={}:{bid}",
        eve_machonet::MACHO_PROXY_NODE_ID
    ))]);
    let rep = spy_ctx
        .dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "ClientHasReleasedTheseObjects",
            &[release_arg],
            &kw,
            None,
        )
        .await
        .expect("stationSvc.ClientHasReleasedTheseObjects must route via machoNet entry");
    assert_eq!(rep, PyRep::None);
    assert_eq!(MACHO_RELEASE_HITS.load(Ordering::SeqCst), 1);
    assert_eq!(
        spy_ctx.verify_bound_object(&sess, bid).unwrap_err().code,
        "InvalidBoundObject"
    );

    // 2. Production registry: `ClientHasReleasedTheseObjects` under another service reaches the
    //    `machoNet` handler when unbound, while a bound call on an unrelated service or an unknown
    //    method under any service returns `UnimplementedRPC`.
    let prod_ctx = edge_ctx();
    let mut prod_sess = demo_session(&prod_ctx, 94_981, service_roles::ROLE_PLAYER_DEFAULT);
    let beyonce_bid = prod_ctx
        .bind_object(&prod_sess, "beyonce", 30_000_142)
        .expect("bind beyonce on prod_ctx");
    let bound_rel_err = prod_ctx
        .dispatch_rpc(
            &mut prod_sess,
            "beyonce",
            Some(beyonce_bid),
            "ClientHasReleasedTheseObjects",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("bound ClientHasReleasedTheseObjects on beyonce must not alias machoNet");
    assert_eq!(bound_rel_err.code, crate::router::ERR_UNIMPLEMENTED);

    let prod_rel = PyRep::List(vec![PyRep::String(format!(
        "N={}:{beyonce_bid}",
        eve_machonet::MACHO_PROXY_NODE_ID
    ))]);
    let ok_other = prod_ctx
        .dispatch_rpc(
            &mut prod_sess,
            "stationSvc",
            None,
            "ClientHasReleasedTheseObjects",
            &[prod_rel],
            &kw,
            None,
        )
        .await
        .expect("unbound stationSvc.ClientHasReleasedTheseObjects succeeds via machoNet handler");
    assert_eq!(ok_other, PyRep::None);
    assert_eq!(
        prod_ctx.verify_bound_object(&prod_sess, beyonce_bid).unwrap_err().code,
        "InvalidBoundObject"
    );

    for (svc, method) in [
        // rpc-lint: allow negative unknown-method dispatch verification
        ("stationSvc", "NoSuchDispatchMethod"),
        // rpc-lint: allow negative unknown-method dispatch verification
        ("unknownServiceFixture", "NoSuchDispatchMethod"),
    ] {
        let err = prod_ctx
            .dispatch_rpc(&mut prod_sess, svc, None, method, &[], &kw, None)
            .await
            .expect_err("unlisted RPC must fail with UnimplementedRPC");
        assert_eq!(err.code, crate::router::ERR_UNIMPLEMENTED, "{svc}.{method}");
    }
}


