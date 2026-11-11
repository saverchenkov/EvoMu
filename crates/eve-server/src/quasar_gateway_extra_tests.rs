//! Additional unit and mutation-killing tests for `quasar_gateway`.

#![forbid(unsafe_code)]

use super::*;
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    AuthoritativeContext, EvePublicEvent, EvePublicRequest, GoogleAny, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn new_router() -> PublicGatewayRouter {
    PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::new(WorldRepository::new_empty()),
        shared_sde(),
    )
}

fn make_session(session_id: u64, user_id: Option<u64>, char_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(session_id);
    s.user_id = user_id;
    s.character_id = char_id;
    s.corporation_id = Some(98_000_001);
    s.role = role;
    s
}

#[test]
fn requires_active_character_and_admin_role_cover_all_allowlisted_types_and_prefixes() {
    let no_char_exact = [
        "google.protobuf.Empty",
        "eve_public.character.GetNamesByUserRequest",
        "eve.quasar.config.api.GetRequest",
        "eve_public.solarsystem.api.GetRequest",
        "eve_public.station.api.GetRequest",
        "eve.station.GetRequest",
        "eve.station.GetNameRequest",
        "eve.station.GetStationsInSolarSystemRequest",
        "eve.constellation.GetRequest",
        "eve.constellation.GetAllRequest",
        "eve.constellation.SearchRequest",
        "eve.region.GetRequest",
        "eve.region.GetAllRequest",
        "eve.region.SearchRequest",
        "eve_public.debug.Request",
        "eve.quasar.admin.IsAuthorizedRequest",
        "eve.race.GetAllRequest",
        "eve.faction.SearchRequest",
        "eve.faction.api.GetRequest",
        "eve.universe.navigation.GetShortestGateJumpCountRequest",
        "eve.universe.navigation.GetShortestSystemJumpCountRequest",
        "eve_public.app.launcher.distribution.api.LauncherDistributionClientStartRequest",
        "eve_public.app.launcher.eve_client.GameClientStartRequest",
        "eve_public.app.launcher.eve_client.GameClientStopRequest",
        "eve_public.achievement.definition.api.cdn.GetLatestRequest",
        "eve.achievement.definition.api.GetLatestRequest",
        "eve_public.space.api.cdn.GetUpdateRequest",
        "eve_public.cosmetic.character.title.definition.api.GetLatestRequest",
        "eve.cosmetic.character.title.definition.api.GetLatestRequest",
        "eve_public.cosmetic.character.title.api.GetEquippedForCharacterRequest",
    ];
    for name in no_char_exact {
        assert!(
            !PublicGatewayRouter::requires_active_character(name),
            "expected {name} not to require active character"
        );
    }

    let user_prefixes = [
        "eve_public.auth",
        "eve.auth",
        "eve_public.login",
        "eve.login",
        "eve_public.user",
        "eve.user",
        "eve_public.entitlement",
        "eve.entitlement",
        "eve_public.omega",
        "eve.omega",
        "eve_public.membership",
        "eve.membership",
        "eve_public.payment",
        "eve.payment",
        "eve_public.plex",
        "eve.plex",
        "eve_public.store",
        "eve.store",
        "eve_public.vendors",
        "eve.vendors",
        "eve_public.fulfillment",
        "eve.fulfillment",
        "eve_public.isk",
        "eve.isk",
        "eve_public.legal",
        "eve.legal",
        "eve_public.pii",
        "eve.pii",
        "eve_public.device",
        "eve.device",
        "eve_public.cdn",
        "eve.cdn",
        "eve_public.client",
        "eve.client",
        "eve_public.content",
        "eve.content",
        "eve_public.deadspace",
        "eve.deadspace",
        "eve_public.localization",
        "eve.localization",
    ];
    for pfx in user_prefixes {
        let sample = format!("{pfx}.api.GetRequest");
        assert!(
            !PublicGatewayRouter::requires_active_character(&sample),
            "expected prefix {pfx} not to require active character"
        );
    }

    let admin_exact = [
        "eve.entitlement.user.api.GrantRequest",
        "eve.entitlement.user.subscription.api.GrantRequest",
        "eve.plex.vault.assetholding.operation.api.DepositRequest",
        "eve.plex.vault.assetholding.operation.api.WithdrawRequest",
        "eve.fulfillment.api.DeliverItemRequest",
        "eve.cosmetic.market.component.listing.api.GetAllRequest",
    ];
    for name in admin_exact {
        assert!(
            PublicGatewayRouter::requires_admin_role(name),
            "expected {name} to require admin role"
        );
    }

    let gm_ctx = AuthoritativeContext::for_session(Some(1), Some(1), None, 0, "t", vec![])
        .with_service_roles(service_roles::ROLE_GM_MASK);
    assert!(require_admin_role(&gm_ctx).is_ok());
}

#[test]
fn subscribe_session_cache_flush_filter_and_leave_notice_ephemeral() {
    let router = new_router();
    let now = 10_000u64;

    // 1. LeaveNotice is ephemeral and not cached when delivered == 0
    let leave_any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.LeaveNotice"),
        value: vec![1],
    };
    let (_, del_leave) = router
        .publish_notice(NoticeScope::Character(90_000_010), leave_any, now)
        .unwrap();
    assert_eq!(del_leave, 0);
    assert!(router.routing_cache.read().unwrap().entries.is_empty());

    // 2. Cache a notice for Character(90_000_010), then subscribe_session WITH that character already set
    let dbg_any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.debug.Notice"),
        value: vec![7, 8],
    };
    router
        .publish_notice(NoticeScope::Character(90_000_010), dbg_any.clone(), now)
        .unwrap();
    let sess = make_session(10, Some(1), Some(90_000_010), service_roles::ROLE_PLAYER_DEFAULT);
    let _rx = router.subscribe_session(&sess, vec!["eve_public.debug.Notice".into()], now + 1);
    assert_eq!(router.poll_notices(10).len(), 1);

    // 3. Connected subscriber whose filter rejects payload is skipped during publish_notice
    let (_, del_filtered) = router
        .publish_notice(
            NoticeScope::Character(90_000_010),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.unsubscribed.Notice"),
                value: vec![9],
            },
            now + 2,
        )
        .unwrap();
    assert_eq!(del_filtered, 0);
    assert!(router.poll_notices(10).is_empty());
}

#[test]
fn hijack_arg_parsing_and_all_rpc_method_aliases_and_error_paths() {
    let router = new_router();
    let kwargs = HashMap::new();

    // Test all valid HTTP status codes in qa_enable_hijack_mode
    for code in [400i64, 401, 403, 404, 405, 409, 412, 415, 500, 501, 503, 504] {
        assert!(router
            .dispatch_service_gateway_mgr_rpc(
                "qa_enable_hijack_mode",
                &[PyRep::Integer(code), PyRep::Integer(0)],
                &kwargs
            )
            .unwrap()
            .is_ok());
    }
    // Test "None", "", "UnpackException", Token("TimeoutException"), and kwargs
    for s in ["None", "", "UnpackException"] {
        assert!(router
            .dispatch_service_gateway_mgr_rpc(
                "qa_enable_hijack_mode",
                &[PyRep::String(s.into()), PyRep::Real(300.0)],
                &kwargs
            )
            .unwrap()
            .is_ok());
    }
    let kw_hijack = HashMap::from([
        ("error".to_string(), PyRep::Token("TimeoutException".into())),
        ("latency".to_string(), PyRep::Integer(300)),
    ]);
    assert!(router
        .dispatch_service_gateway_mgr_rpc("qa_enable_hijack_mode", &[], &kw_hijack)
        .unwrap()
        .is_ok());

    // Invalid error type, negative float latency, NaN latency, integer > 300, invalid latency type
    assert!(router
        .dispatch_service_gateway_mgr_rpc("qa_enable_hijack_mode", &[PyRep::Bool(true)], &kwargs)
        .unwrap()
        .is_err());
    assert!(router
        .dispatch_service_gateway_mgr_rpc(
            "qa_enable_hijack_mode",
            &[PyRep::None, PyRep::Real(-0.01)],
            &kwargs
        )
        .unwrap()
        .is_err());
    assert!(router
        .dispatch_service_gateway_mgr_rpc(
            "qa_enable_hijack_mode",
            &[PyRep::None, PyRep::Real(f64::NAN)],
            &kwargs
        )
        .unwrap()
        .is_err());
    assert!(router
        .dispatch_service_gateway_mgr_rpc(
            "qa_enable_hijack_mode",
            &[PyRep::None, PyRep::Integer(301)],
            &kwargs
        )
        .unwrap()
        .is_err());
    assert!(router
        .dispatch_service_gateway_mgr_rpc(
            "qa_enable_hijack_mode",
            &[PyRep::None, PyRep::String("bad".into())],
            &kwargs
        )
        .unwrap()
        .is_err());
    router.set_server_hijack_mode(false, None, None);

    // dispatch_public_gateway_rpc aliases & edge cases
    let sess = make_session(20, Some(1), Some(90_000_001), service_roles::ROLE_PLAYER_DEFAULT);
    let now = 50_000u64;
    assert!(router
        .dispatch_public_gateway_rpc(&sess, "UnknownGatewayMethod", &[], &kwargs, now)
        .is_none());

    for sub_method in ["SubscribeToNotice", "ConsumeNotices"] {
        let rep = router
            .dispatch_public_gateway_rpc(
                &sess,
                sub_method,
                &[PyRep::Tuple(vec![PyRep::String("eve_public.debug.Notice".into())])],
                &kwargs,
                now,
            )
            .unwrap()
            .unwrap();
        assert!(matches!(rep, PyRep::KeyVal(_)));
    }

    // PublishEvent & PublishEventPayload valid / invalid / non-bytes
    let valid_ev = EvePublicEvent {
        uuid: vec![1; 16],
        external_origin: "test".into(),
        ..Default::default()
    }
    .encode_proto();
    assert_eq!(
        router
            .dispatch_public_gateway_rpc(&sess, "PublishEvent", &[PyRep::Buffer(valid_ev.clone())], &kwargs, now)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        router
            .dispatch_public_gateway_rpc(&sess, "PublishEventPayload", &[PyRep::Bytes(valid_ev)], &kwargs, now)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert!(router
        .dispatch_public_gateway_rpc(&sess, "PublishEvent", &[PyRep::Bytes(vec![0xFF])], &kwargs, now)
        .unwrap()
        .is_err());
    assert!(router
        .dispatch_public_gateway_rpc(&sess, "PublishEventPayload", &[PyRep::Bytes(vec![0xFF])], &kwargs, now)
        .unwrap()
        .is_err());
    assert!(router
        .dispatch_public_gateway_rpc(&sess, "PublishEvent", &[PyRep::Integer(1)], &kwargs, now)
        .is_none());

    // CallQuasar / Send with kwargs["request"] and bytes idempotency_key, plus missing request -> None
    let req_bytes = EvePublicRequest {
        correlation_uuid: vec![3; 16],
        payload: Some(GoogleEmpty.to_any()),
        application_instance_uuid: vec![7; 16],
        ..Default::default()
    }
    .encode_proto();
    let call_kw = HashMap::from([
        ("request".to_string(), PyRep::Buffer(req_bytes)),
        ("idempotency_key".to_string(), PyRep::Bytes(vec![1, 2, 3, 4])),
    ]);
    for m in ["CallQuasar", "Send"] {
        assert!(router
            .dispatch_public_gateway_rpc(&sess, m, &[], &call_kw, now)
            .unwrap()
            .is_ok());
    }
    assert!(router
        .dispatch_public_gateway_rpc(&sess, "Send", &[], &kwargs, now)
        .is_none());
}

#[test]
fn quasar_gateway_meta_hydration_clamping_and_empty_reset() {
    use std::sync::atomic::Ordering;

    let router = new_router();
    // Dirty RAM state on empty DB -> hydrate_meta_from_db resets all fields to defaults
    *router.server_hijack_state.write().unwrap() = QuasarHijackState {
        enabled: true,
        error_code: Some(503),
        latency_sec: Some(1.25),
    };
    router.next_notice_seq.store(42, Ordering::Relaxed);
    router.free_fast_checkout_plex.store(true, Ordering::Relaxed);
    router
        .has_persisted_free_fast_checkout_plex
        .store(true, Ordering::Relaxed);

    router.hydrate_meta_from_db();
    let hijack = router.server_hijack_state.read().unwrap().clone();
    assert!(!hijack.enabled);
    assert_eq!(hijack.error_code, None);
    assert_eq!(hijack.latency_sec, None);
    assert_eq!(router.next_notice_seq.load(Ordering::Relaxed), 1);
    assert!(!router.free_fast_checkout_plex.load(Ordering::Relaxed));
    assert!(
        !router
            .has_persisted_free_fast_checkout_plex
            .load(Ordering::Relaxed)
    );

    // Seed meta blob with next_notice_seq: 0 and free_fast_checkout_plex: None while RAM flag is true
    router.free_fast_checkout_plex.store(true, Ordering::Relaxed);
    router.db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert(
            QUASAR_GATEWAY_META_BLOB_KEY.to_string(),
            serde_json::json!({
                "server_hijack_state": { "enabled": false, "error_code": null, "latency_sec": null },
                "next_notice_seq": 0,
                "free_fast_checkout_plex": null
            })
            .to_string(),
        );
    });
    router.hydrate_meta_from_db();
    assert_eq!(router.next_notice_seq.load(Ordering::Relaxed), 1);
    assert!(
        !router
            .has_persisted_free_fast_checkout_plex
            .load(Ordering::Relaxed)
    );

    // Seed free_fast_checkout_plex: Some(false) -> clears RAM flag & sets has_persisted = true
    router.free_fast_checkout_plex.store(true, Ordering::Relaxed);
    router.db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert(
            QUASAR_GATEWAY_META_BLOB_KEY.to_string(),
            serde_json::json!({
                "server_hijack_state": { "enabled": false, "error_code": null, "latency_sec": null },
                "next_notice_seq": 7,
                "free_fast_checkout_plex": false
            })
            .to_string(),
        );
    });
    router.hydrate_meta_from_db();
    assert_eq!(router.next_notice_seq.load(Ordering::Relaxed), 7);
    assert!(!router.free_fast_checkout_plex.load(Ordering::Relaxed));
    assert!(
        router
            .has_persisted_free_fast_checkout_plex
            .load(Ordering::Relaxed)
    );

    // sync_meta_into_snapshot clamping & ternary None vs Some(false)
    let mut snap = router.db.with_snapshot(|s| s.clone());
    router.next_notice_seq.store(0, Ordering::Relaxed);
    router
        .has_persisted_free_fast_checkout_plex
        .store(false, Ordering::Relaxed);
    router.sync_meta_into_snapshot(&mut snap);
    let parsed: QuasarGatewayMetaPayload =
        serde_json::from_str(&snap.quasar_state_blobs[QUASAR_GATEWAY_META_BLOB_KEY]).unwrap();
    assert_eq!(parsed.next_notice_seq, 1);
    assert_eq!(parsed.free_fast_checkout_plex, None);

    router
        .has_persisted_free_fast_checkout_plex
        .store(true, Ordering::Relaxed);
    router.free_fast_checkout_plex.store(false, Ordering::Relaxed);
    router.sync_meta_into_snapshot(&mut snap);
    let parsed2: QuasarGatewayMetaPayload =
        serde_json::from_str(&snap.quasar_state_blobs[QUASAR_GATEWAY_META_BLOB_KEY]).unwrap();
    assert_eq!(parsed2.free_fast_checkout_plex, Some(false));
}

#[test]
fn quasar_gateway_sync_meta_reentrancy_direct_persistence_and_hooks() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let router1 = new_router();
    // publish_notice & set_server_hijack_mode persist directly via sync_meta_to_db without sync_all_to_db
    router1
        .publish_notice(NoticeScope::Broadcast, GoogleAny::default(), 1_700_000_000)
        .expect("publish notice");
    router1.set_server_hijack_mode(true, Some(502), Some(0.5));

    let router2 = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&router1.db),
        shared_sde(),
    );
    assert_eq!(router2.next_notice_seq.load(Ordering::Relaxed), 2);
    let h2 = router2.server_hijack_state.read().unwrap().clone();
    assert!(h2.enabled);
    assert_eq!(h2.error_code, Some(502));
    assert_eq!(h2.latency_sec, Some(0.5));

    // Reentrancy guard: calling sync_meta_to_db inside with_mut_snapshot must not deadlock or overwrite snapshot
    router1.next_notice_seq.store(99, Ordering::Relaxed);
    router1.db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert(
            QUASAR_GATEWAY_META_BLOB_KEY.to_string(),
            "{\"next_notice_seq\":12}".to_string(),
        );
        router1.sync_meta_to_db();
        assert_eq!(
            snap.quasar_state_blobs[QUASAR_GATEWAY_META_BLOB_KEY],
            "{\"next_notice_seq\":12}"
        );
    });

    // register_persistence_hooks ordering & distinct dispatch for sync_all_to_db vs rehydrate_all_from_db
    let rehydrate_calls = Arc::new(AtomicUsize::new(0));
    let sync_calls = Arc::new(AtomicUsize::new(0));
    let r_c = Arc::clone(&rehydrate_calls);
    let s_c = Arc::clone(&sync_calls);
    router1.register_persistence_hooks(
        Arc::new(move || {
            r_c.fetch_add(1, Ordering::Relaxed);
        }),
        Arc::new(move || {
            s_c.fetch_add(1, Ordering::Relaxed);
        }),
    );
    router1.sync_all_to_db();
    assert_eq!(sync_calls.load(Ordering::Relaxed), 1);
    assert_eq!(rehydrate_calls.load(Ordering::Relaxed), 0);

    router1.rehydrate_all_from_db();
    assert_eq!(sync_calls.load(Ordering::Relaxed), 1);
    assert_eq!(rehydrate_calls.load(Ordering::Relaxed), 1);
}

#[test]
fn quasar_gateway_ensure_rollback_synced_epoch_equality_guard() {
    use std::sync::atomic::Ordering;

    let router = new_router();
    router.sync_all_to_db();
    assert_eq!(router.next_notice_seq.load(Ordering::Relaxed), 1);

    // Mutate RAM only (no DB sync) while rollback_epoch is unchanged -> ensure_rollback_synced must NOT clobber RAM
    router.next_notice_seq.store(9, Ordering::Relaxed);
    router.ensure_rollback_synced();
    assert_eq!(router.next_notice_seq.load(Ordering::Relaxed), 9);

    // Trigger rollback via failing try_with_mut_snapshot -> bumps rollback_epoch
    let err: Result<(), String> = router
        .db
        .try_with_mut_snapshot(|_| Err("force rollback".to_string()));
    assert!(err.is_err());

    // Epoch changed -> ensure_rollback_synced rehydrates persisted DB state (`next_notice_seq = 1`)
    router.ensure_rollback_synced();
    assert_eq!(router.next_notice_seq.load(Ordering::Relaxed), 1);

    // Subsequent in-memory change while epoch matches again stays untouched
    router.next_notice_seq.store(15, Ordering::Relaxed);
    router.ensure_rollback_synced();
    assert_eq!(router.next_notice_seq.load(Ordering::Relaxed), 15);
}
