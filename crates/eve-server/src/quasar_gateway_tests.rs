//! Unit tests for `quasar_gateway` notice scopes, subscriber routing cache, idempotency cache,
//! policy checks, and subscription fan-out.

#![forbid(unsafe_code)]

use super::*;
use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_proto::quasar::{GoogleAny, NoticeTargetGroup, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX};
use eve_sde::SdeRegistry;
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn new_test_router() -> PublicGatewayRouter {
    PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::new(WorldRepository::new_empty()),
        shared_sde(),
    )
}

fn make_session(
    session_id: u64,
    user_id: Option<u64>,
    char_id: Option<i64>,
    corp_id: Option<i64>,
    alliance_id: Option<i64>,
    solar_system_id: Option<u32>,
    fleet_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(session_id);
    s.user_id = user_id;
    s.character_id = char_id;
    s.corporation_id = corp_id;
    s.alliance_id = alliance_id;
    s.solar_system_id = solar_system_id;
    s.fleet_id = fleet_id;
    s
}

fn make_subscriber(
    session_id: u64,
    char_id: Option<u32>,
    notice_types: Vec<String>,
) -> (GatewaySubscriber, mpsc::Receiver<EvePublicNotice>) {
    let (tx, rx) = mpsc::channel(MAX_SUBSCRIBER_INBOX_LEN);
    let sub = GatewaySubscriber {
        session_id,
        subscribed_at_unix: 1_000,
        application_instance_uuid: PublicGatewayRouter::default_app_instance_uuid(session_id),
        user_id: Some(10),
        character_id: char_id,
        corporation_id: Some(98_000_001),
        alliance_id: Some(99_000_001),
        solar_system_id: Some(30_000_142),
        fleet_id: Some(777),
        bubble_instance_uuid: Some(vec![9; 16]),
        subscribed_notice_types: notice_types,
        sender: tx,
        inbox: VecDeque::new(),
    };
    (sub, rx)
}

fn sample_notice(type_name: &str, byte_val: u8) -> EvePublicNotice {
    EvePublicNotice {
        dispatched: Some(GoogleTimestamp {
            seconds: 1_700_000_000,
            nanos: 0,
        }),
        uuid: vec![byte_val; 16],
        internal_origin: "test".into(),
        tenant: "tranquility".into(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: vec![byte_val; 8],
        }),
        target_group: None,
        trace_context: None,
    }
}

#[test]
fn notice_scope_matching_and_target_group_resolution_cover_all_variants() {
    let (mut sub_with_char, _rx1) = make_subscriber(1, Some(90_000_001), Vec::new());
    let (sub_no_char, _rx2) = make_subscriber(2, None, Vec::new());
    let app_uuid = sub_with_char.application_instance_uuid.clone();

    let cases: Vec<(NoticeScope, bool, Option<NoticeTargetGroup>)> = vec![
        (
            NoticeScope::ApplicationInstance(app_uuid.clone()),
            true,
            Some(NoticeTargetGroup::ApplicationInstanceUuid(app_uuid.clone())),
        ),
        (
            NoticeScope::ApplicationInstance(vec![0; 16]),
            false,
            Some(NoticeTargetGroup::ApplicationInstanceUuid(vec![0; 16])),
        ),
        (
            NoticeScope::SolarSystem(30_000_142),
            true,
            Some(NoticeTargetGroup::SolarSystem(30_000_142)),
        ),
        (
            NoticeScope::SolarSystem(30_000_144),
            false,
            Some(NoticeTargetGroup::SolarSystem(30_000_144)),
        ),
        (NoticeScope::User(10), true, Some(NoticeTargetGroup::User(10))),
        (NoticeScope::User(11), false, Some(NoticeTargetGroup::User(11))),
        (
            NoticeScope::Character(90_000_001),
            true,
            Some(NoticeTargetGroup::Character(90_000_001)),
        ),
        (
            NoticeScope::Character(90_000_002),
            false,
            Some(NoticeTargetGroup::Character(90_000_002)),
        ),
        (
            NoticeScope::Corporation(98_000_001),
            true,
            Some(NoticeTargetGroup::Corporation(98_000_001)),
        ),
        (
            NoticeScope::Corporation(98_000_002),
            false,
            Some(NoticeTargetGroup::Corporation(98_000_002)),
        ),
        (
            NoticeScope::Alliance(99_000_001),
            true,
            Some(NoticeTargetGroup::Alliance(99_000_001)),
        ),
        (
            NoticeScope::Alliance(99_000_002),
            false,
            Some(NoticeTargetGroup::Alliance(99_000_002)),
        ),
        (
            NoticeScope::BubbleInstance(vec![9; 16]),
            true,
            Some(NoticeTargetGroup::BubbleInstanceUuid(vec![9; 16])),
        ),
        (
            NoticeScope::BubbleInstance(vec![8; 16]),
            false,
            Some(NoticeTargetGroup::BubbleInstanceUuid(vec![8; 16])),
        ),
        (NoticeScope::Fleet(777), true, None),
        (NoticeScope::Fleet(778), false, None),
        (NoticeScope::Broadcast, true, None),
    ];

    for (scope, expected_match, expected_base) in cases {
        assert_eq!(scope.matches_subscriber(&sub_with_char), expected_match);
        assert_eq!(scope.base_target_group(), expected_base);
    }

    // Fleet and Broadcast stamp Character when present, or ApplicationInstanceUuid when absent.
    assert_eq!(
        NoticeScope::Fleet(777).target_group_for_subscriber(&sub_with_char),
        NoticeTargetGroup::Character(90_000_001)
    );
    assert_eq!(
        NoticeScope::Broadcast.target_group_for_subscriber(&sub_no_char),
        NoticeTargetGroup::ApplicationInstanceUuid(sub_no_char.application_instance_uuid.clone())
    );

    // update_from_session updates all tenancy fields.
    let updated_sess = make_session(
        1,
        Some(20),
        Some(90_000_099),
        Some(98_000_099),
        None,
        Some(30_002_187),
        Some(888),
    );
    sub_with_char.update_from_session(&updated_sess);
    assert_eq!(sub_with_char.user_id, Some(20));
    assert_eq!(sub_with_char.character_id, Some(90_000_099));
    assert_eq!(sub_with_char.corporation_id, Some(98_000_099));
    assert_eq!(sub_with_char.alliance_id, None);
    assert_eq!(sub_with_char.solar_system_id, Some(30_002_187));
    assert_eq!(sub_with_char.fleet_id, Some(888));
}

#[test]
fn subscriber_payload_filter_and_inbox_capacity_rollover() {
    let (sub_all, _rx1) = make_subscriber(1, Some(1), Vec::new());
    assert!(sub_all.accepts_payload_type("eve_public.debug.Notice"));

    let (mut sub_filtered, _rx2) = make_subscriber(
        2,
        Some(2),
        vec![format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.debug.Notice")],
    );
    assert!(sub_filtered.accepts_payload_type("eve_public.debug.Notice"));
    assert!(sub_filtered.accepts_payload_type(&format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.debug.Notice")));
    assert!(!sub_filtered.accepts_payload_type("eve_public.other.Notice"));

    // Push 260 notices into inbox (MAX_SUBSCRIBER_INBOX_LEN = 256) -> oldest 4 evicted.
    for i in 0..260u16 {
        sub_filtered.push_notice(sample_notice("eve_public.debug.Notice", (i % 250) as u8));
    }
    assert_eq!(sub_filtered.inbox.len(), MAX_SUBSCRIBER_INBOX_LEN);
    assert_eq!(sub_filtered.inbox.front().unwrap().uuid[0], 4);
    assert_eq!(sub_filtered.inbox.back().unwrap().uuid[0], 9);
}

#[test]
fn notice_routing_cache_enforces_oom_eviction_and_expiry_boundaries() {
    let n1 = sample_notice("eve_public.debug.Notice", 1);
    let n1_size = n1.encode_proto().len().max(1);

    // Oversized notice triggers CacheOOMException.
    let mut tiny_cache = NoticeRoutingCache::new(n1_size - 1);
    let err = tiny_cache
        .add_notice(n1.clone(), NoticeScope::Character(100), 500)
        .unwrap_err();
    assert_eq!(err.code, "CacheOOMException");

    // Cache sized for 2 notices evicts oldest when 3rd is added.
    let mut cache = NoticeRoutingCache::new(n1_size * 2);
    cache
        .add_notice(sample_notice("eve_public.debug.Notice", 1), NoticeScope::Character(100), 500)
        .unwrap();
    cache
        .add_notice(sample_notice("eve_public.debug.Notice", 2), NoticeScope::Character(200), 505)
        .unwrap();
    assert_eq!(cache.entries.len(), 2);
    assert_eq!(cache.current_bytes, n1_size * 2);

    // Adding a 3rd evicts entry 1 (char 100), keeping entries 2 and 3.
    cache
        .add_notice(sample_notice("eve_public.debug.Notice", 3), NoticeScope::Broadcast, 508)
        .unwrap();
    assert_eq!(cache.entries.len(), 2);
    assert_eq!(cache.entries[0].notice.uuid[0], 2);
    assert_eq!(cache.entries[1].notice.uuid[0], 3);

    // At now = 514 (< 505 + 10), entry 2 (expiry 515) is still alive; drain for char 200 matches entry 2 & 3.
    let (sub_200, _rx) = make_subscriber(9, Some(200), Vec::new());
    let drained = cache.drain_newly_valid_for_subscriber(&sub_200, 514);
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0].target_group, Some(NoticeTargetGroup::Character(200)));
    assert_eq!(drained[1].target_group, Some(NoticeTargetGroup::Character(200)));
    assert_eq!(cache.current_bytes, 0);
    assert!(cache.entries.is_empty());

    // Exact expiry boundary: expiry_unix == now_unix (500 + 10 = 510) expires it; 509 keeps it.
    cache
        .add_notice(sample_notice("eve_public.debug.Notice", 4), NoticeScope::Character(300), 500)
        .unwrap();
    cache.remove_expired(509);
    assert_eq!(cache.entries.len(), 1);
    cache.remove_expired(510);
    assert!(cache.entries.is_empty());
    assert_eq!(cache.current_bytes, 0);
}

#[test]
fn bounded_idempotency_cache_handles_ttl_overwrite_and_capacity_eviction() {
    let mut cache = BoundedIdempotencyCache::default();
    let k1: IdempotencyKeyTuple = (1, 10, "req.A".into(), vec![1]);
    let rsp1 = CachedIdempotentResponse {
        status_code: 200,
        status_message: "OK".into(),
        payload: None,
        created_at_unix: 1_000,
    };
    cache.insert(k1.clone(), rsp1, 1_000);

    // Still valid at +599s, expired at +600s.
    assert!(cache.get(&k1, 1_599).is_some());
    assert!(cache.get(&k1, 1_600).is_none());

    // Re-inserting an existing key with a newer timestamp prevents old order entry from evicting it.
    let k2: IdempotencyKeyTuple = (1, 10, "req.B".into(), vec![2]);
    cache.insert(
        k2.clone(),
        CachedIdempotentResponse {
            status_code: 200,
            status_message: "v1".into(),
            payload: None,
            created_at_unix: 2_000,
        },
        2_000,
    );
    cache.insert(
        k2.clone(),
        CachedIdempotentResponse {
            status_code: 201,
            status_message: "v2".into(),
            payload: None,
            created_at_unix: 2_200,
        },
        2_200,
    );
    // At 2_600, old ts 2_000 pops from order, but cur.created_at_unix is 2_200 so k2 remains!
    let got = cache.get(&k2, 2_600).expect("newer entry must survive stale order pop");
    assert_eq!(got.status_message, "v2");
    assert!(cache.get(&k2, 2_800).is_none());

    // Fill to MAX_IDEMPOTENCY_ENTRIES (4096) and verify 4097th evicts oldest without exceeding cap.
    let mut full_cache = BoundedIdempotencyCache::default();
    for i in 0..MAX_IDEMPOTENCY_ENTRIES {
        full_cache.insert(
            (1, i as u32, "req".into(), vec![1]),
            CachedIdempotentResponse {
                status_code: 200,
                status_message: "OK".into(),
                payload: None,
                created_at_unix: 5_000,
            },
            5_000,
        );
    }
    assert_eq!(full_cache.map.len(), MAX_IDEMPOTENCY_ENTRIES);
    full_cache.insert(
        (1, 999_999, "req".into(), vec![9]),
        CachedIdempotentResponse {
            status_code: 200,
            status_message: "OK".into(),
            payload: None,
            created_at_unix: 5_000,
        },
        5_000,
    );
    assert_eq!(full_cache.map.len(), MAX_IDEMPOTENCY_ENTRIES);
    assert!(full_cache.get(&(1, 0, "req".into(), vec![1]), 5_001).is_none());
    assert!(full_cache.get(&(1, 999_999, "req".into(), vec![9]), 5_001).is_some());
}

#[test]
fn gateway_policies_and_notice_subscription_lifecycle() {
    assert!(!PublicGatewayRouter::requires_active_character("google.protobuf.Empty"));
    assert!(!PublicGatewayRouter::requires_active_character("eve_public.character.GetNamesByUserRequest"));
    assert!(!PublicGatewayRouter::requires_active_character("eve.quasar.config.api.GetRequest"));
    assert!(!PublicGatewayRouter::requires_active_character("eve_public.store.offer.api.GetAllRequest"));
    assert!(!PublicGatewayRouter::requires_active_character("eve.plex.vault.api.BalanceRequest"));
    assert!(PublicGatewayRouter::requires_active_character("eve_public.cosmetic.market.skin.listing.api.GetAllRequest"));

    assert!(PublicGatewayRouter::requires_admin_role("eve.some.admin.DoThingRequest"));
    assert!(PublicGatewayRouter::requires_admin_role("eve.content.orchestrator.SpawnRequest"));
    assert!(PublicGatewayRouter::requires_admin_role("eve.fulfillment.api.DeliverItemRequest"));
    assert!(!PublicGatewayRouter::requires_admin_role("eve_public.store.offer.api.GetAllRequest"));

    let router = new_test_router();

    // Publish a notice BEFORE subscriber connects -> cached in routing_cache.
    let any_notice = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.debug.Notice"),
        value: vec![1, 2, 3],
    };
    let (_, delivered_early) = router
        .publish_notice(NoticeScope::Character(90_000_055), any_notice.clone(), 1_000)
        .unwrap();
    assert_eq!(delivered_early, 0);

    // Ephemeral chat notices are NOT cached when delivered == 0.
    let join_notice = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.JoinNotice"),
        value: vec![1],
    };
    let (_, delivered_join) = router
        .publish_notice(NoticeScope::Character(90_000_055), join_notice, 1_000)
        .unwrap();
    assert_eq!(delivered_join, 0);

    // Subscribe session without character first, then transition via on_session_changed.
    let mut sess = make_session(55, Some(10), None, Some(98_000_001), None, Some(30_000_142), None);
    let mut rx = router.subscribe_session(&sess, Vec::new(), 1_001);
    assert!(router.has_subscriber(55));
    assert!(router.poll_notices(55).is_empty());

    // Transition session to character 90_000_055 -> flushes cached notice (but not ephemeral JoinNotice).
    sess.character_id = Some(90_000_055);
    let flushed = router.on_session_changed(&sess, 1_002);
    assert_eq!(flushed, 1);
    let polled = router.poll_notices(55);
    assert_eq!(polled.len(), 1);
    assert_eq!(polled[0].uuid.len(), 16);
    assert!(rx.try_recv().is_ok());

    // Unknown session on_session_changed returns 0.
    let unknown_sess = make_session(99_999, Some(10), Some(1), None, None, None, None);
    assert_eq!(router.on_session_changed(&unknown_sess, 1_003), 0);

    router.unsubscribe_session(55);
    assert!(!router.has_subscriber(55));

    // Verify MAX_GATEWAY_SUBSCRIBERS (4096) eviction when subscribing 4097th session.
    for sid in 1..=(MAX_GATEWAY_SUBSCRIBERS as u64) {
        let s = make_session(sid, Some(sid), Some(sid as i64), None, None, None, None);
        let _ = router.subscribe_session(&s, Vec::new(), 2_000 + sid);
    }
    assert!(router.has_subscriber(1));
    let overflow_s = make_session(999_001, Some(999_001), Some(999_001), None, None, None, None);
    let _ = router.subscribe_session(&overflow_s, Vec::new(), 9_000);
    assert!(!router.has_subscriber(1));
    assert!(router.has_subscriber(999_001));
}
