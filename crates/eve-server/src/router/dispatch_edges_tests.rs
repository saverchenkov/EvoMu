//! Edge-case behavioral tests for `router::dispatch`.

use crate::router::ServerContext;
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
    let ping_err = ctx
        .dispatch_rpc(&mut player, "authentication", None, "Ping", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(ping_err.code, "UnimplementedRPC");
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

    // Flood MAX_TRACKED_RPC_METHODS + 12 distinct pre-auth forged-bound-id probes, plus
    // forged-bound-id calls targeting valid pre-auth RPC names and non-allowlisted pre-auth RPCs.
    let mut unauth = SessionState::new(901);
    let mut expected_collapsed = 0u64;
    let flood_count = MAX_TRACKED_RPC_METHODS + 12;
    for i in 0..flood_count {
        let svc = format!("probe_svc_{i}");
        let meth = format!("probe_meth_{i}");
        let err = ctx
            .dispatch_rpc(&mut unauth, &svc, Some(existing_bid), &meth, &[], &kw, None)
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

    // Pre-auth `ClientHasReleasedTheseObjects` with a forged bound_id returns `Ok(None)` early
    // before ownership check, so latency classification must still collapse it to `<unknown>`.
    assert!(ctx
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
        .is_ok());
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

