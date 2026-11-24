use super::*;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

#[test]
fn track_2g_registry_eliminates_all_non_binding_allowlist_gaps() {
    let registry =
        RpcRegistry::try_from_modules(COMPILED_RPC_MODULES).expect("zero duplicate entries");
    assert_eq!(COMPILED_RPC_MODULES.len(), 122);
    assert_eq!(registry.all_entries().len(), 1962);

    let unique_pairs: BTreeSet<(&str, &str)> = registry
        .all_entries()
        .iter()
        .map(|entry| (entry.service, entry.method))
        .collect();
    assert_eq!(unique_pairs.len(), registry.all_entries().len());

    // All 38 RPCs added in Track 2G + exact-spelling corpmgr methods must be registered.
    for (svc, method) in [
        ("account", "GiveCashFromCorpAccount"),
        ("agentMgr", "GetMessagesForEpicArcMissions"),
        ("authentication", "Ping"),
        ("beyonce", "ActivateAccelerationGate"),
        ("beyonce", "GetFormations"),
        ("charUnboundMgr", "SelectCharacterID"),
        ("corpmgr", "createcorporation"),
        ("corpmgr", "getwalletdivisions"),
        ("dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances"),
        ("dungeonInstanceCacheMgr", "GetHomefrontSiteInstances"),
        ("dungeonInstanceCacheMgr", "GetSignatureInstances"),
        ("fleetObjectHandler", "CreateFleet"),
        ("fleetObjectHandler", "MakeLeader"),
        ("harness", "ClearSessionTimer"),
        ("harness", "FastForwardSessionTimer"),
        ("keeper", "ActivateAccelerationGate"),
        ("keeper", "Bind"),
        ("machoNet", "ClientHasReleasedTheseObjects"),
        ("miningScanMgr", "perform_scan"),
        ("publicGatewaySvc", "ConsumeNotices"), // rpc-lint: allow Quasar notice subscription and poll routes
        ("publicGatewaySvc", "PollNotices"), // rpc-lint: allow Quasar notice subscription and poll routes
        ("publicGatewaySvc", "PublishEvent"),
        ("publicGatewaySvc", "PublishEventPayload"),
        ("publicGatewaySvc", "Send"),
        ("publicGatewaySvc", "SendRequest"),
        ("publicGatewaySvc", "Subscribe"),
        ("publicGatewaySvc", "SubscribeToNotice"), // rpc-lint: allow Quasar notice subscription and poll routes
        ("scanMgr", "ConeScan"),
        ("scanMgr", "RequestScans"),
        ("serviceGatewayMgr", "qa_disable_hijack_mode"),
        ("serviceGatewayMgr", "qa_enable_hijack_mode"),
        ("serviceGatewayMgr", "qa_is_hijack_mode_enabled"),
        ("skillMgr2", "GetSkills"),
        ("slash", "SlashCmd"),
        ("stationSvc", "Dock"),
        ("structureDocking", "Dock"),
        ("timeControl", "AdvanceClock"),
        ("timeControl", "ClearSessionTimer"),
        ("timeControl", "FastForwardSessionTimer"),
        ("wormholeMgr", "GetWormholePolarization"),
    ] {
        assert!(
            registry.lookup(svc, method).is_some(),
            "missing Track 2G compiled registry entry for {svc}.{method}"
        );
    }

    // Rule 1 / Rule 3: non-client aliases and dead internal names stay out of the table.
    for (svc, method) in [
        ("fleetMgr", "MakeLeader"), // rpc-lint: allow non-client alias rejection check
        ("corpmgr", "CreateCorporation"), // rpc-lint: allow legacy PascalCase fallback alias check
        ("corpmgr", "GetWalletDivisions"), // rpc-lint: allow legacy PascalCase fallback alias check
        ("taleMgr", "MaterializeSystemLOD2"), // rpc-lint: allow non-client internal name check
    ] {
        assert!(
            registry.lookup(svc, method).is_none(),
            "non-client spelling {svc}.{method} must not be in COMPILED_RPC_MODULES"
        );
    }

    // Verify 0 non-binding gaps between rpc_negative_matrix_allowlist.json and COMPILED_RPC_MODULES.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root");
    let allowlist_path = root.join("crates/eve-testkit/ratchets/rpc_negative_matrix_allowlist.json");
    let raw = std::fs::read_to_string(&allowlist_path).expect("read allowlist json");
    let json: serde_json::Value = serde_json::from_str(&raw).expect("parse allowlist json");
    let allowlist_obj = json
        .get("rpcs")
        .and_then(|v| v.as_object())
        .expect("allowlist.rpcs object");

    let compiled_keys: BTreeSet<String> = registry
        .all_entries()
        .iter()
        .map(|e| format!("{}.{}", e.service, e.method))
        .collect();
    let allowlist_keys: BTreeSet<String> = allowlist_obj.keys().cloned().collect();

    let compiled_missing_allowlist: Vec<String> =
        compiled_keys.difference(&allowlist_keys).cloned().collect();
    assert!(
        compiled_missing_allowlist.is_empty(),
        "compiled entries missing from allowlist: {compiled_missing_allowlist:?}"
    );

    let mut binding_only = Vec::new();
    let mut non_binding_gaps = Vec::new();
    for key in allowlist_keys.difference(&compiled_keys) {
        if key.ends_with(".MachoBindObject") || key.ends_with(".MachoResolveObject") {
            binding_only.push(key.clone());
        } else {
            non_binding_gaps.push(key.clone());
        }
    }
    assert!(
        non_binding_gaps.is_empty(),
        "allowlist must have 0 non-binding gaps vs compiled registry, found: {non_binding_gaps:?}"
    );
    assert_eq!(
        binding_only.len(),
        72,
        "expected exactly 72 MachoBindObject/MachoResolveObject allowlist entries"
    );

    // Verify dispatch.rs contains no leftover Track 2G named RPC blocks or Abyssal guard.
    let dispatch_src =
        std::fs::read_to_string(root.join("crates/eve-server/src/router/dispatch.rs"))
            .expect("read dispatch.rs");
    let dispatch_inner_start = dispatch_src
        .find("async fn dispatch_rpc_inner(")
        .expect("dispatch_rpc_inner start");
    let dispatch_inner_end = dispatch_src
        .find("pub(crate) fn unimplemented_rpc_error(")
        .expect("unimplemented_rpc_error start");
    let dispatch_inner = &dispatch_src[dispatch_inner_start..dispatch_inner_end];
    assert!(
        !dispatch_inner.contains("check_abyssal_in_pocket_rpc_restrictions"),
        "Abyssal guard must no longer live in dispatch_rpc_inner"
    );
    for forbidden_svc in [
        "\"wormholeMgr\"",
        "\"timeControl\"",
        "\"harness\"",
        "\"serviceGatewayMgr\"",
        "\"skillMgr2\"",
        "\"stationSvc\"",
        "\"structureDocking\"",
        "\"dungeonInstanceCacheMgr\"",
        "\"agentMgr\"",
        "\"fleetObjectHandler\"",
        "\"fleetMgr\"", // rpc-lint: allow dispatch_inner audit assertion
        "\"miningScanMgr\"",
        "\"corpmgr\"",
        "\"account\"",
    ] {
        assert!(
            !dispatch_inner.contains(forbidden_svc),
            "dispatch_rpc_inner must not branch on {forbidden_svc}"
        );
    }
}

#[tokio::test]
async fn track_2g_session_lifecycle_hooks_execute_inside_handlers() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let kw = HashMap::new();

    // 1. authentication.Ping succeeds pre-auth AND during shutdown, tracking wallclock ticks (#1031).
    let mut sess = SessionState::new(98_701);
    ctx.cluster_state.write().unwrap().shutdown_in_progress = true;
    ctx.advance_secs_without_sim_clock_rebase(3_600);
    let ping = ctx
        .dispatch_rpc(&mut sess, "authentication", None, "Ping", &[], &kw, None)
        .await
        .expect("authentication.Ping succeeds via RpcRegistry during shutdown");
    ctx.cluster_state.write().unwrap().shutdown_in_progress = false;
    let wall_now = ctx
        .dispatch_rpc(&mut sess, "machoNet", None, "GetTime", &[], &kw, None)
        .await
        .expect("machoNet.GetTime succeeds");
    let (PyRep::Integer(ping_ticks), PyRep::Integer(wall_ticks)) = (ping, wall_now) else {
        panic!("expected Integer timestamps from Ping and GetTime");
    };
    assert!(ping_ticks > 0 && (ping_ticks - wall_ticks).abs() < 50_000_000);

    // 2. authentication.Login authenticates session and populates user_id.
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
    .expect("authentication.Login succeeds via RpcRegistry");
    assert!(sess.user_id.is_some());

    // 3. charUnboundMgr.SelectCharacterID runs pre-dispatch guards + post-select cleanup hooks.
    ctx.mark_space_ballpark_bootstrapped(sess.session_id, 90_000_001, 30_000_142);
    assert!(ctx.mark_brain_primed_for_session(sess.session_id, 90_000_001));
    assert!(!ctx.mark_brain_primed_for_session(sess.session_id, 90_000_001));
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
    .expect("charUnboundMgr.SelectCharacterID succeeds via RpcRegistry");
    assert_eq!(sess.character_id, Some(90_000_001));
    assert!(
        !ctx.is_space_ballpark_bootstrapped(sess.session_id, 90_000_001, 30_000_142),
        "SelectCharacterID post-hook must clear ballpark bootstrapped flag"
    );
    assert!(
        ctx.mark_brain_primed_for_session(sess.session_id, 90_000_001),
        "SelectCharacterID post-hook must clear brain-primed state"
    );

    // 4. machoNet.ClientHasReleasedTheseObjects increments calls_received and releases bound oid.
    let bid_macho = ctx
        .bind_object(&sess, "beyonce", 30_000_142)
        .expect("bind beyonce for machoNet release test");
    let before_calls = ctx
        .cluster_state
        .read()
        .unwrap()
        .calls_received
        .get("machoNet")
        .copied()
        .unwrap_or(0);
    let released_rep = PyRep::List(vec![PyRep::String(format!(
        "N={}:{bid_macho}",
        eve_machonet::MACHO_PROXY_NODE_ID
    ))]);
    ctx.dispatch_rpc(
        &mut sess,
        "machoNet",
        None,
        "ClientHasReleasedTheseObjects",
        &[released_rep],
        &kw,
        None,
    )
    .await
    .expect("machoNet.ClientHasReleasedTheseObjects succeeds");
    assert_eq!(
        ctx.verify_bound_object(&sess, bid_macho).unwrap_err().code,
        "InvalidBoundObject"
    );
    let after_calls = ctx
        .cluster_state
        .read()
        .unwrap()
        .calls_received
        .get("machoNet")
        .copied()
        .unwrap_or(0);
    assert_eq!(after_calls, before_calls + 1);

    // 5. Authenticated non-machoNet universal release clears structure ballpark state.
    let bid_ship = ctx
        .bind_object(&sess, "beyonce", 30_000_142)
        .expect("bind beyonce for ship release test");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.structure_id = Some(1_040_000_000_001);
        }
    });
    sess.station_id = None;
    sess.structure_id = Some(1_040_000_000_001);
    ctx.mark_space_ballpark_bootstrapped(sess.session_id, 90_000_001, 30_000_142);
    let ship_release = PyRep::List(vec![PyRep::String(format!(
        "N={}:{bid_ship}",
        eve_machonet::MACHO_PROXY_NODE_ID
    ))]);
    ctx.dispatch_rpc(
        &mut sess,
        "ship",
        None,
        "ClientHasReleasedTheseObjects",
        &[ship_release],
        &kw,
        None,
    )
    .await
    .expect("authenticated ship.ClientHasReleasedTheseObjects succeeds");
    assert!(!ctx.is_space_ballpark_bootstrapped(sess.session_id, 90_000_001, 30_000_142));

    // 6. userSvc.UserLogOffCharacter & authentication.Logout release bindings and clear session.
    let bid_logoff = ctx
        .bind_object(&sess, "beyonce", 30_000_142)
        .expect("bind beyonce before logoff");
    ctx.dispatch_rpc(
        &mut sess,
        "userSvc",
        None,
        "UserLogOffCharacter",
        &[],
        &kw,
        None,
    )
    .await
    .expect("UserLogOffCharacter succeeds via RpcRegistry");
    assert_eq!(
        ctx.verify_bound_object(&sess, bid_logoff).unwrap_err().code,
        "InvalidBoundObject"
    );
    assert!(sess.character_id.is_none());

    ctx.dispatch_rpc(&mut sess, "authentication", None, "Logout", &[], &kw, None)
        .await
        .expect("authentication.Logout succeeds via RpcRegistry");
    assert!(sess.user_id.is_none());
}

#[tokio::test]
async fn track_2g_time_control_gateway_and_slash_dispatch_through_registry() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let kw = HashMap::new();

    let mut player = SessionState::new(98_702);
    player.user_id = Some(1);
    player.character_id = Some(90_000_001);
    player.corporation_id = Some(98_000_001);
    player.solar_system_id = Some(30_000_142);

    let mut gm = player.clone();
    gm.session_id = 98_703;
    gm.role = ServerContext::ROLE_GM_CANONICAL_MASK;

    // 1. timeControl & harness reject non-GM and validate arguments cleanly.
    for (svc, method) in [
        ("timeControl", "AdvanceClock"),
        ("timeControl", "ClearSessionTimer"),
        ("timeControl", "FastForwardSessionTimer"),
        ("harness", "ClearSessionTimer"),
        ("harness", "FastForwardSessionTimer"),
    ] {
        let err = ctx
            .dispatch_rpc(&mut player, svc, None, method, &[], &kw, None)
            .await
            .expect_err("non-GM must be rejected by timeControl/harness");
        assert_eq!(err.code, "InsufficientRole");
    }

    let neg_err = ctx
        .dispatch_rpc(
            &mut gm,
            "timeControl",
            None,
            "AdvanceClock",
            &[PyRep::Integer(-1)],
            &kw,
            None,
        )
        .await
        .expect_err("negative AdvanceClock must return InvalidArgument");
    assert_eq!(neg_err.code, "InvalidArgument");

    for bad_secs in [
        PyRep::String("abc".into()),
        PyRep::Real(1.5),
        PyRep::Bool(true),
    ] {
        let bad_err = ctx
            .dispatch_rpc(
                &mut gm,
                "timeControl",
                None,
                "AdvanceClock",
                &[bad_secs],
                &kw,
                None,
            )
            .await
            .expect_err("malformed AdvanceClock seconds must return InvalidArgument");
        assert_eq!(bad_err.code, "InvalidArgument");
    }

    let bad_target_err = ctx
        .dispatch_rpc(
            &mut gm,
            "timeControl",
            None,
            "AdvanceClock",
            &[PyRep::Integer(1), PyRep::String("not-an-id".into())],
            &kw,
            None,
        )
        .await
        .expect_err("malformed AdvanceClock target_char_id must return InvalidArgument");
    assert_eq!(bad_target_err.code, "InvalidArgument");

    let bad_clear_err = ctx
        .dispatch_rpc(
            &mut gm,
            "timeControl",
            None,
            "ClearSessionTimer",
            &[PyRep::String("bad-char-id".into())],
            &kw,
            None,
        )
        .await
        .expect_err("malformed ClearSessionTimer target_char_id must return InvalidArgument");
    assert_eq!(bad_clear_err.code, "InvalidArgument");

    let before_default_unix = ctx.now_unix();
    let default_adv = ctx
        .dispatch_rpc(
            &mut gm,
            "timeControl",
            None,
            "AdvanceClock",
            &[PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("GM AdvanceClock(None) defaults to 12s");
    assert_eq!(default_adv, PyRep::Bool(true));
    assert!(ctx.now_unix() >= before_default_unix + 12);

    let before_unix = ctx.now_unix();
    let adv_res = ctx
        .dispatch_rpc(
            &mut gm,
            "timeControl",
            None,
            "AdvanceClock",
            &[PyRep::Integer(25)],
            &kw,
            None,
        )
        .await
        .expect("GM AdvanceClock succeeds");
    assert_eq!(adv_res, PyRep::Bool(true));
    assert!(ctx.now_unix() >= before_unix + 25);

    for (svc, method) in [
        ("timeControl", "ClearSessionTimer"),
        ("timeControl", "FastForwardSessionTimer"),
        ("harness", "ClearSessionTimer"),
        ("harness", "FastForwardSessionTimer"),
    ] {
        crate::services::space_destiny::record_session_change_timer(
            &ctx,
            90_000_001,
            ctx.now_unix(),
            30_000_142,
            0,
            "track_2g_test",
        );
        assert!(
            crate::router::session_transition::get_session_change_timer(&ctx, 90_000_001)
                .is_some()
        );
        let clear_res = ctx
            .dispatch_rpc(&mut gm, svc, None, method, &[], &kw, None)
            .await
            .expect("GM session timer clear succeeds");
        assert_eq!(clear_res, PyRep::Bool(true));
        assert!(
            crate::router::session_transition::get_session_change_timer(&ctx, 90_000_001)
                .is_none(),
            "{svc}.{method} must clear session_change_timers"
        );
    }

    // 2. serviceGatewayMgr hijack toggles and role checks.
    assert_eq!(
        ctx.dispatch_rpc(
            &mut player,
            "serviceGatewayMgr",
            None,
            "qa_enable_hijack_mode",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientRole" // rpc-lint: allow InsufficientRole error code assertion
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut gm,
            "serviceGatewayMgr",
            None,
            "qa_enable_hijack_mode",
            &[],
            &kw,
            None,
        )
        .await
        .expect("qa_enable_hijack_mode"),
        PyRep::None
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut gm,
            "serviceGatewayMgr",
            None,
            "qa_is_hijack_mode_enabled",
            &[],
            &kw,
            None,
        )
        .await
        .expect("qa_is_hijack_mode_enabled"),
        PyRep::Bool(true)
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut gm,
            "serviceGatewayMgr",
            None,
            "qa_disable_hijack_mode",
            &[],
            &kw,
            None,
        )
        .await
        .expect("qa_disable_hijack_mode"),
        PyRep::None
    );
    assert!(!ctx.quasar_gateway.is_server_hijack_mode_enabled());

    // 3. publicGatewaySvc Subscribe/PollNotices and SendRequest catalog/unimpl errors.
    ctx.dispatch_rpc(
        &mut player,
        "publicGatewaySvc",
        None,
        "Subscribe",
        &[PyRep::String("eve_public.debug.Notice".into())],
        &kw,
        None,
    )
    .await
    .expect("publicGatewaySvc.Subscribe succeeds");
    let notices = ctx
        .dispatch_rpc(
            &mut player,
            "publicGatewaySvc",
            None,
            "PollNotices", // rpc-lint: allow Quasar notice subscription and poll routes
            &[],
            &kw,
            None,
        )
        .await
        .expect("publicGatewaySvc.PollNotices succeeds");
    assert!(matches!(notices, PyRep::List(_)));
    let unknown_dom = ctx
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
    assert_eq!(unknown_dom.code, "UnknownQuasarDomain");

    // 4. slash.SlashCmd and publicQaToolsServer.SlashCmd route through RpcRegistry.
    let player_list_err = ctx
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
    assert_eq!(player_list_err.code, "SlashError");

    assert!(ctx
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
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut gm,
            "publicQaToolsServer",
            None,
            "SlashCmd",
            &[PyRep::String("/whoami".into())],
            &kw,
            None,
        )
        .await
        .is_ok());
}

#[tokio::test]
async fn track_2g_after_lookup_arms_and_rule3_rejections() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let kw = HashMap::new();

    // Seed a skill and an active ship in space for skillMgr2.GetSkills + beyonce.GetFormations.
    let active_ship_id = ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            3300,
            eve_db::TrainedSkillRecord {
                type_id: 3300,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        snap.corporations.entry(98_000_001).or_insert_with(|| {
            eve_db::CorporationRecord::new(98_000_001, "Track2G Corp", "T2GC", 90_000_001)
        });
        let ch = snap.characters.get_mut(&90_000_001).expect("demo character");
        ch.corporation_id = 98_000_001;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = 30_000_142;
        let ship_id = ch.active_ship_id;
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = 30_000_142;
        }
        ship_id
    });

    let mut sess = SessionState::new(98_704);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(30_000_142);
    sess.ship_id = Some(active_ship_id);

    // 1. skillMgr2.GetSkills returns Rowset normally, Dict for machoNet:26000 or bound calls.
    let rowset_skills = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkills", &[], &kw, None)
        .await
        .expect("skillMgr2.GetSkills Rowset");
    assert!(matches!(rowset_skills, PyRep::Rowset { .. }));

    let trace_skills = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "GetSkills",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("skillMgr2.GetSkills trace Dict");
    assert!(matches!(trace_skills, PyRep::Dict(_)));

    let skill_bid = ctx
        .bind_object(&sess, "skillMgr2", 90_000_001)
        .expect("bind skillMgr2");
    let bound_skills = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            Some(skill_bid),
            "GetSkills",
            &[],
            &kw,
            None,
        )
        .await
        .expect("bound skillMgr2.GetSkills Dict");
    assert!(matches!(bound_skills, PyRep::Dict(_)));

    // 2. beyonce.GetFormations bootstraps unbootstrapped undocked ballpark observer state.
    assert!(active_ship_id > 0, "demo seed character must have an active ship");
    ctx.clear_space_ballpark_bootstrapped(90_000_001);
    let formations = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "GetFormations", &[], &kw, None)
        .await
        .expect("beyonce.GetFormations succeeds");
    assert!(matches!(formations, PyRep::Tuple(_)));
    assert!(
        ctx.is_space_ballpark_bootstrapped(sess.session_id, 90_000_001, 30_000_142),
        "beyonce.GetFormations post-hook must bootstrap undocked observer ballpark"
    );

    // 3. Verify Rule 3 deletions: dead/alias spellings fail with UnimplementedRPC.
    for (svc, method) in [
        ("fleetMgr", "MakeLeader"), // rpc-lint: allow non-client alias rejection check
        ("corpmgr", "CreateCorporation"), // rpc-lint: allow legacy PascalCase fallback alias check
        ("corpmgr", "GetWalletDivisions"), // rpc-lint: allow legacy PascalCase fallback alias check
    ] {
        let err = ctx
            .dispatch_rpc(&mut sess, svc, None, method, &[], &kw, None)
            .await
            .expect_err("deleted Rule 3 spelling must fail");
        assert_eq!(err.code, "UnimplementedRPC"); // rpc-lint: allow MachoNet error code assertion
    }

    // 4. Real client spellings for remaining migrated Track 2G arms route cleanly.
    let wdiv = ctx
        .dispatch_rpc(
            &mut sess,
            "corpmgr",
            None,
            "getwalletdivisions",
            &[PyRep::Integer(98_000_001)],
            &kw,
            None,
        )
        .await
        .expect("corpmgr.getwalletdivisions succeeds");
    assert!(matches!(wdiv, PyRep::Rowset { .. }));
    let perform_err = ctx
        .dispatch_rpc(&mut sess, "miningScanMgr", None, "perform_scan", &[], &kw, None)
        .await
        .expect_err("0-arg perform_scan in space without a fitted survey scanner must return ModuleNotFound");
    assert_eq!(perform_err.code, "ModuleNotFound"); // rpc-lint: allow ModuleNotFound error code assertion
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_000_144;
        }
    });
    let mut no_ball_sess = sess.clone();
    no_ball_sess.solar_system_id = Some(30_000_144);
    let not_in_space_err = ctx
        .dispatch_rpc(
            &mut no_ball_sess,
            "miningScanMgr",
            None,
            "perform_scan",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("0-arg perform_scan without an active ship ball in space must return ShipNotInSpace");
    assert_eq!(not_in_space_err.code, "ShipNotInSpace"); // rpc-lint: allow ShipNotInSpace error code assertion
    assert!(!ctx.systems.is_system_loaded(30_000_144));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_000_142;
        }
    });

    for (svc, method) in [
        ("stationSvc", "Dock"),
        ("structureDocking", "Dock"),
        ("beyonce", "ActivateAccelerationGate"),
        ("keeper", "ActivateAccelerationGate"),
        ("dungeonInstanceCacheMgr", "GetSignatureInstances"),
        ("dungeonInstanceCacheMgr", "GetHomefrontSiteInstances"),
        ("dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances"),
        ("agentMgr", "GetMessagesForEpicArcMissions"),
        ("fleetObjectHandler", "MakeLeader"),
        ("fleetObjectHandler", "CreateFleet"),
        ("wormholeMgr", "GetWormholePolarization"),
        ("keeper", "Bind"),
        ("scanMgr", "ConeScan"),
        ("scanMgr", "RequestScans"),
        ("account", "GiveCashFromCorpAccount"),
        ("corpmgr", "createcorporation"),
    ] {
        let res = ctx.dispatch_rpc(&mut sess, svc, None, method, &[], &kw, None).await;
        if let Err(err) = res {
            assert_ne!(
                err.code, "UnimplementedRPC", // rpc-lint: allow MachoNet error code assertion
                "{svc}.{method} unexpectedly fell back to UnimplementedRPC"
            );
        }
    }

    // Abyssal in-pocket scanMgr.ConeScan guard still runs inside space_cyno_filaments.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 32_000_001;
        }
    });
    sess.solar_system_id = Some(32_000_001);
    let abyssal_err = ctx
        .dispatch_rpc(&mut sess, "scanMgr", None, "ConeScan", &[], &kw, None)
        .await
        .expect_err("abyssal guard must block scanMgr.ConeScan");
    assert_eq!(abyssal_err.code, "CannotDscanInThisSystem");
}
