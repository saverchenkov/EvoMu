use super::*;
use crate::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_UNAUTHORIZED};
use eve_db::PersistedInsurgencyCampaign;
use eve_proto::quasar::{
    ActiveIdentity, EvePublicCorpRoleCollection, GoogleTimestamp, WireMessage,
};
use std::collections::HashMap;
use std::sync::OnceLock;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn setup_router() -> (Arc<PublicGatewayRouter>, Arc<WorldRepository>, Arc<SdeRegistry>) {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = QuasarDescriptorRegistry::open_default_or_empty();
    let router = Arc::new(PublicGatewayRouter::new(
        descriptors,
        Arc::clone(&db),
        Arc::clone(&sde),
    ));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));
    (router, db, sde)
}

fn invoke(
    router: &PublicGatewayRouter,
    req_type: &str,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let handler = router
        .handlers
        .read()
        .unwrap()
        .get(req_type)
        .cloned()
        .unwrap_or_else(|| panic!("missing handler for {req_type}"));
    let raw_any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{req_type}"),
        value: Vec::new(),
    };
    handler(ctx, &raw_any, msg)
}

#[test]
fn unit_helpers_roles_extractors_and_domain_encoding() {
    let mut ctx = AuthoritativeContext::default();
    assert_eq!(effective_roles(&ctx), 0);

    ctx.identity = Some(ActiveIdentity {
        roles: Some(EvePublicCorpRoleCollection { bitmask: 0x10 }),
        ..Default::default()
    });
    assert_eq!(effective_roles(&ctx), 0x10);

    ctx.corp_role_mask = 0x20;
    assert_eq!(effective_roles(&ctx), 0x20);

    let mut msg = DynamicQuasarMessage::new("TestMsg");
    msg.set(1, DynamicValue::Int64(10));
    msg.set(2, DynamicValue::Uint64(20));
    msg.set(3, DynamicValue::Int32(30));
    msg.set(4, DynamicValue::Uint32(40));
    msg.set(5, DynamicValue::Bool(true));
    assert_eq!(get_int_field(&msg, 1), Some(10));
    assert_eq!(get_int_field(&msg, 2), Some(20));
    assert_eq!(get_int_field(&msg, 3), Some(30));
    assert_eq!(get_int_field(&msg, 4), Some(40));
    assert_eq!(get_int_field(&msg, 5), Some(1));
    assert_eq!(get_int_field(&msg, 99), None);

    // Nested identifier extraction
    let mut inner = DynamicQuasarMessage::new("Inner");
    inner.set(1, DynamicValue::Uint32(777));
    let mut outer = DynamicQuasarMessage::new("Outer");
    outer.set(1, DynamicValue::Message(Box::new(inner)));
    msg.set(6, DynamicValue::Message(Box::new(outer)));
    assert_eq!(get_identifier_or_int(&msg, 6), Some(777));
    assert_eq!(get_identifier_or_int(&msg, 1), Some(10));

    // String extractor
    msg.set(7, DynamicValue::String("direct".into()));
    let mut s_sub = DynamicQuasarMessage::new("Sub");
    s_sub.set(1, DynamicValue::String("nested".into()));
    msg.set(8, DynamicValue::Message(Box::new(s_sub)));
    msg.set(
        9,
        DynamicValue::Repeated(vec![DynamicValue::String("rep".into())]),
    );
    assert_eq!(get_string_field(&msg, 7).as_deref(), Some("direct"));
    assert_eq!(get_string_field(&msg, 8).as_deref(), Some("nested"));
    assert_eq!(get_string_field(&msg, 9).as_deref(), Some("rep"));
    assert_eq!(get_string_field(&msg, 99), None);
}

#[test]
fn solarsystem_constellation_region_navigation_and_bubble_handlers() {
    let (router, _db, _sde) = setup_router();
    let ctx = AuthoritativeContext::for_session(Some(1), Some(90_000_001), Some(1_000_009), 0, "tranquility", vec![1]);
    let anon = AuthoritativeContext::default();

    // 1. solarsystem.api.GetRequest
    let mut sys_req = DynamicQuasarMessage::new("eve_public.solarsystem.api.GetRequest");
    sys_req.set(1, DynamicValue::Uint32(30_000_142));
    assert!(invoke(&router, "eve_public.solarsystem.api.GetRequest", &ctx, &sys_req).is_ok());
    assert!(invoke(
        &router,
        "eve_public.solarsystem.api.GetRequest",
        &anon,
        &DynamicQuasarMessage::new("eve_public.solarsystem.api.GetRequest")
    )
    .is_err());

    // 2. GetSolarSystemsInJumpRangeRequest
    assert!(invoke(
        &router,
        "eve.solarsystem.api.GetSolarSystemsInJumpRangeRequest",
        &ctx,
        &sys_req
    )
    .is_ok());

    // 3. Constellation Get / GetAll / Search
    let mut c_req = DynamicQuasarMessage::new("eve.constellation.GetRequest");
    c_req.set(1, DynamicValue::Uint32(20_000_020));
    assert!(invoke(&router, "eve.constellation.GetRequest", &ctx, &c_req).is_ok());
    c_req.set(1, DynamicValue::Uint32(0));
    assert_eq!(
        invoke(&router, "eve.constellation.GetRequest", &ctx, &c_req).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    assert!(invoke(
        &router,
        "eve.constellation.GetAllRequest",
        &ctx,
        &DynamicQuasarMessage::new("eve.constellation.GetAllRequest")
    )
    .is_ok());

    let mut c_search = DynamicQuasarMessage::new("eve.constellation.SearchRequest");
    assert_eq!(
        invoke(&router, "eve.constellation.SearchRequest", &ctx, &c_search).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    c_search.set(1, DynamicValue::String("Kimotoro".into()));
    assert!(invoke(&router, "eve.constellation.SearchRequest", &ctx, &c_search).is_ok());

    // 4. Region Get / GetAll / Search
    let mut r_req = DynamicQuasarMessage::new("eve.region.GetRequest");
    r_req.set(1, DynamicValue::Uint32(10_000_002));
    assert!(invoke(&router, "eve.region.GetRequest", &ctx, &r_req).is_ok());
    r_req.set(1, DynamicValue::Uint32(0));
    assert_eq!(
        invoke(&router, "eve.region.GetRequest", &ctx, &r_req).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    assert!(invoke(
        &router,
        "eve.region.GetAllRequest",
        &ctx,
        &DynamicQuasarMessage::new("eve.region.GetAllRequest")
    )
    .is_ok());

    let mut r_search = DynamicQuasarMessage::new("eve.region.SearchRequest");
    assert_eq!(
        invoke(&router, "eve.region.SearchRequest", &ctx, &r_search).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    r_search.set(1, DynamicValue::String("Forge".into()));
    assert!(invoke(&router, "eve.region.SearchRequest", &ctx, &r_search).is_ok());

    // 5. GetAllInRegion & GetShortestGateJumpCount
    let mut u_reg = DynamicQuasarMessage::new("eve.universe.constellation.GetAllInRegionRequest");
    u_reg.set(1, DynamicValue::Uint32(10_000_002));
    assert!(invoke(&router, "eve.universe.constellation.GetAllInRegionRequest", &ctx, &u_reg).is_ok());

    let mut nav = DynamicQuasarMessage::new("eve.universe.navigation.GetShortestGateJumpCountRequest");
    nav.set(1, DynamicValue::Uint32(30_000_142));
    nav.set(2, DynamicValue::Uint32(30_000_142));
    assert!(invoke(
        &router,
        "eve.universe.navigation.GetShortestGateJumpCountRequest",
        &ctx,
        &nav
    )
    .is_ok());
    nav.set(2, DynamicValue::Uint32(30_000_144));
    assert!(invoke(
        &router,
        "eve.universe.navigation.GetShortestGateJumpCountRequest",
        &ctx,
        &nav
    )
    .is_ok());

    // 6. Bubble presence
    assert!(invoke(
        &router,
        "eve.bubble.presence.api.GetInBubbleRequest",
        &ctx,
        &DynamicQuasarMessage::new("eve.bubble.presence.api.GetInBubbleRequest")
    )
    .is_ok());
    let mut b_char = DynamicQuasarMessage::new("eve.bubble.presence.api.GetInBubbleByCharacterRequest");
    b_char.set(1, DynamicValue::Uint32(90_000_001));
    assert!(invoke(
        &router,
        "eve.bubble.presence.api.GetInBubbleByCharacterRequest",
        &ctx,
        &b_char
    )
    .is_ok());
    b_char.set(1, DynamicValue::Uint32(0));
    assert_eq!(
        invoke(
            &router,
            "eve.bubble.presence.api.GetInBubbleByCharacterRequest",
            &ctx,
            &b_char
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
}

#[test]
fn pirate_insurgency_corruption_and_suppression_handlers() {
    let (router, db, _sde) = setup_router();
    let mut ctx = AuthoritativeContext::for_session(Some(1), Some(90_000_001), Some(1_000_009), 0, "tranquility", vec![1]);
    ctx.received = Some(GoogleTimestamp {
        seconds: 1_700_000_500,
        nanos: 0,
    });

    // Seed two insurgency campaigns (warzone 1 and warzone 2)
    db.with_mut_snapshot(|snap| {
        let mut cp1 = HashMap::new();
        cp1.insert(30_000_142, 1000); // Stage 5 pirate victory
        let mut sp1 = HashMap::new();
        sp1.insert(30_000_142, 400);
        snap.insurgency_runtime.campaigns.insert(
            1,
            PersistedInsurgencyCampaign {
                campaign_id: 1,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: 30_000_142,
                systems: vec![30_000_142, 30_000_144],
                created_at_unix: 1_700_000_000,
                forecasting_end_unix: 1_700_000_100,
                active_end_unix: 1_700_010_000,
                winddown_end_unix: 1_700_020_000,
                corruption_points: cp1,
                suppression_points: sp1,
            },
        );
        let mut sp2 = HashMap::new();
        sp2.insert(30_002_187, 1000); // Stage 5 anti-pirate victory
        snap.insurgency_runtime.campaigns.insert(
            2,
            PersistedInsurgencyCampaign {
                campaign_id: 2,
                pirate_faction_id: 500_011,
                target_faction_id: 500_003,
                fob_solar_system_id: 30_002_187,
                systems: vec![30_002_187],
                created_at_unix: 1_700_000_000,
                forecasting_end_unix: 1_700_000_100,
                active_end_unix: 0,
                winddown_end_unix: 0,
                corruption_points: HashMap::new(),
                suppression_points: sp2,
            },
        );
    });

    // GetRequest: campaign 1 (warzone 1, pirate won), campaign 2 (warzone 2, anti-pirate won), 999 (not found), 0 (bad request)
    let mut get_req = DynamicQuasarMessage::new("eve.pirate.insurgency.api.GetRequest");
    assert_eq!(
        invoke(&router, "eve.pirate.insurgency.api.GetRequest", &ctx, &get_req).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    get_req.set(1, DynamicValue::Int64(999));
    assert_eq!(
        invoke(&router, "eve.pirate.insurgency.api.GetRequest", &ctx, &get_req).unwrap_err().0,
        STATUS_NOT_FOUND
    );
    get_req.set(1, DynamicValue::Int64(1));
    let rsp1 = invoke(&router, "eve.pirate.insurgency.api.GetRequest", &ctx, &get_req).unwrap();
    let wm1 = WireMessage::decode(&rsp1.value).unwrap();
    assert!(wm1.fields.iter().any(|f| f.field_number == 5 && f.value.as_varint() == Some(3)));

    get_req.set(1, DynamicValue::Int64(2));
    let rsp2 = invoke(&router, "eve.pirate.insurgency.api.GetRequest", &ctx, &get_req).unwrap();
    let wm2 = WireMessage::decode(&rsp2.value).unwrap();
    assert!(wm2.fields.iter().any(|f| f.field_number == 5 && f.value.as_varint() == Some(4)));

    // GetCurrentRequest, GetSystemsRequest, GetForWarzoneRequest
    let cur = invoke(
        &router,
        "eve.pirate.insurgency.api.GetCurrentRequest",
        &ctx,
        &DynamicQuasarMessage::new("eve.pirate.insurgency.api.GetCurrentRequest"),
    )
    .unwrap();
    assert!(!cur.value.is_empty());

    let mut sys_req = DynamicQuasarMessage::new("eve.pirate.insurgency.api.GetSystemsRequest");
    sys_req.set(1, DynamicValue::Int64(1));
    let sys_rsp = invoke(&router, "eve.pirate.insurgency.api.GetSystemsRequest", &ctx, &sys_req).unwrap();
    assert!(!sys_rsp.value.is_empty());

    let mut wz_req = DynamicQuasarMessage::new("eve.pirate.insurgency.api.GetForWarzoneRequest");
    wz_req.set(1, DynamicValue::Uint32(1));
    assert!(invoke(&router, "eve.pirate.insurgency.api.GetForWarzoneRequest", &ctx, &wz_req).is_ok());
    wz_req.set(1, DynamicValue::Uint32(99));
    assert!(invoke(&router, "eve.pirate.insurgency.api.GetForWarzoneRequest", &ctx, &wz_req).is_ok());

    // Corruption & Suppression system info and stage thresholds
    let mut info_req = DynamicQuasarMessage::new("InfoReq");
    info_req.set(1, DynamicValue::Uint32(30_000_142));
    assert!(invoke(
        &router,
        "eve_public.pirate.corruption.api.GetSystemInfoRequest",
        &ctx,
        &info_req
    )
    .is_ok());
    assert!(invoke(
        &router,
        "eve_public.pirate.suppression.api.GetSystemInfoRequest",
        &ctx,
        &info_req
    )
    .is_ok());

    info_req.set(1, DynamicValue::Uint32(99_999_999));
    assert_eq!(
        invoke(
            &router,
            "eve_public.pirate.corruption.api.GetSystemInfoRequest",
            &ctx,
            &info_req
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    assert!(invoke(
        &router,
        "eve_public.pirate.corruption.api.GetStageThresholdsRequest",
        &ctx,
        &DynamicQuasarMessage::new("Req")
    )
    .is_ok());
    assert!(invoke(
        &router,
        "eve_public.pirate.suppression.api.GetStageThresholdsRequest",
        &ctx,
        &DynamicQuasarMessage::new("Req")
    )
    .is_ok());
}

#[test]
fn dbs_dungeons_npc_ship_stargate_lock_ai_assistant_and_cdn_handlers() {
    let (router, db, _sde) = setup_router();
    let mut ctx = AuthoritativeContext::for_session(Some(1), Some(90_000_001), Some(1_000_009), 0, "tranquility", vec![1]);
    ctx.received = Some(GoogleTimestamp {
        seconds: 1_700_000_000,
        nanos: 0,
    });
    let anon = AuthoritativeContext::default();

    // DBS, Dungeons, NPC Ship Owner
    for endpoint in [
        "eve.dynamicbountysystem.GetDynamicBountySettingsRequest",
        "eve.dynamicbountysystem.GetActiveSystemsRequest",
        "eve.dynamicbountysystem.GetPaidSystemBountiesRequest",
        "eve.dynamicbountysystem.GetAccumulatedSystemShipLossValueRequest",
        "eve.dungeon.sovereignty.api.GetInstancesRequest",
        "eve_public.dungeon.sovereignty.api.GetVisibleInstancesRequest",
        "eve.npc.ship.api.GetOwnerRequest",
    ] {
        let mut req = DynamicQuasarMessage::new(endpoint);
        req.set(1, DynamicValue::Uint32(30_000_142));
        assert!(invoke(&router, endpoint, &ctx, &req).is_ok(), "{endpoint}");
    }

    // Stargate lock API
    assert_eq!(
        invoke(
            &router,
            "eve_public.stargate.lock.api.GetRequest",
            &anon,
            &DynamicQuasarMessage::new("Req")
        )
        .unwrap_err()
        .0,
        STATUS_UNAUTHORIZED
    );
    db.with_mut_snapshot(|snap| {
        snap.zarzakh_gate_locks.insert(90_000_001, (50_000_001, 1_700_003_600));
    });
    let mut lock_req = DynamicQuasarMessage::new("eve_public.stargate.lock.api.GetRequest");
    lock_req.set(1, DynamicValue::Uint32(30_100_000));
    let lock_rsp = invoke(&router, "eve_public.stargate.lock.api.GetRequest", &ctx, &lock_req).unwrap();
    assert!(!lock_rsp.value.is_empty());

    let rest_rsp = invoke(
        &router,
        "eve_public.stargate.lock.api.GetRestrictedSystemsRequest",
        &ctx,
        &DynamicQuasarMessage::new("Req"),
    )
    .unwrap();
    assert!(!rest_rsp.value.is_empty());

    // AI assistant QueryRequest: empty -> BAD_REQUEST, solar system match, item type match, fallback
    let empty_ai = DynamicQuasarMessage::new("eve_public.ai.assistant.chat.api.QueryRequest");
    assert_eq!(
        invoke(&router, "eve_public.ai.assistant.chat.api.QueryRequest", &ctx, &empty_ai)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    let mut sys_ai = DynamicQuasarMessage::new("eve_public.ai.assistant.chat.api.QueryRequest");
    sys_ai.set(1, DynamicValue::String("Where is Jita located?".into()));
    let sys_ai_rsp = invoke(&router, "eve_public.ai.assistant.chat.api.QueryRequest", &ctx, &sys_ai).unwrap();
    assert!(!sys_ai_rsp.value.is_empty());

    let mut item_ai = DynamicQuasarMessage::new("eve_public.ai.assistant.chat.api.QueryRequest");
    item_ai.set(1, DynamicValue::String("Tell me about Tritanium".into()));
    let item_ai_rsp = invoke(&router, "eve_public.ai.assistant.chat.api.QueryRequest", &ctx, &item_ai).unwrap();
    assert!(!item_ai_rsp.value.is_empty());

    let mut fb_ai = DynamicQuasarMessage::new("eve_public.ai.assistant.chat.api.QueryRequest");
    fb_ai.set(1, DynamicValue::String("xyzzy unknown topic".into()));
    let fb_ai_rsp = invoke(&router, "eve_public.ai.assistant.chat.api.QueryRequest", &ctx, &fb_ai).unwrap();
    assert!(!fb_ai_rsp.value.is_empty());

    // CDN GetUpdateRequest: neither set -> BAD_REQUEST, no_local = true -> returns url
    let empty_cdn = DynamicQuasarMessage::new("eve_public.space.api.cdn.GetUpdateRequest");
    assert_eq!(
        invoke(&router, "eve_public.space.api.cdn.GetUpdateRequest", &ctx, &empty_cdn)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    let mut cdn_req = DynamicQuasarMessage::new("eve_public.space.api.cdn.GetUpdateRequest");
    let mut ver_m = DynamicQuasarMessage::new("Ver");
    ver_m.set(1, DynamicValue::Uint32(24));
    ver_m.set(2, DynamicValue::Uint32(1));
    ver_m.set(3, DynamicValue::Uint32(100));
    cdn_req.set(1, DynamicValue::Message(Box::new(ver_m)));
    cdn_req.set(2, DynamicValue::Bool(true));
    let cdn_rsp = invoke(&router, "eve_public.space.api.cdn.GetUpdateRequest", &ctx, &cdn_req).unwrap();
    assert!(!cdn_rsp.value.is_empty());
}

#[test]
fn space_handlers_fail_closed_when_ids_are_missing() {
    let (router, _db, _sde) = setup_router();
    let anon = AuthoritativeContext::default();
    let empty_msg = DynamicQuasarMessage::new("Empty");

    assert_eq!(
        invoke(&router, "eve.bubble.presence.api.GetInBubbleRequest", &anon, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_UNAUTHORIZED
    );
    assert_eq!(
        invoke(
            &router,
            "eve.bubble.presence.api.GetInBubbleByCharacterRequest",
            &anon,
            &empty_msg
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        invoke(&router, "eve.region.GetRequest", &anon, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    let mut missing_region_search = DynamicQuasarMessage::new("Search");
    missing_region_search.set(1, DynamicValue::String("ZyzNonexistentRegion999".into()));
    assert_eq!(
        invoke(&router, "eve.region.SearchRequest", &anon, &missing_region_search)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
}
