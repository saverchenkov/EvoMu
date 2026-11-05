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
