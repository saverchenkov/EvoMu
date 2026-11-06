use super::*;
use crate::quasar_gateway::STATUS_BAD_REQUEST;
use eve_db::WarRecord;
use eve_proto::quasar::{AuthoritativeContext, QuasarDescriptorRegistry, WireMessage};
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
fn wire_helpers_and_domain_encoders_produce_valid_wire_bytes() {
    let mut buf = Vec::new();
    encode_id_msg(1, 42, &mut buf);
    encode_uuid_msg(2, b"uuid-1234", &mut buf);
    encode_timestamp_field(3, 1_700_000_000, &mut buf);
    encode_isk_currency(4, IskCents(12_345), &mut buf);
    encode_isk_currency(5, IskCents(0), &mut buf);
    let wm = WireMessage::decode(&buf).unwrap();
    assert_eq!(wm.fields.len(), 5);

    // ThirdPartyApi encoder
    let tpa = encode_thirdpartyapi_request_received(90_000_001, "tranquility", "/v1/status", 1, "EvoMu", 1_700_000_000);
    assert!(!tpa.is_empty());
    let tpa_empty = encode_thirdpartyapi_request_received(0, "", "", 0, "", 1_700_000_000);
    assert!(tpa.len() > tpa_empty.len());

    // Tournament encoders
    assert!(!encode_tournament_identifier(b"t-1").is_empty());
    assert!(!encode_tournament_arena(b"a-1", "Arena", 30_000_142, 100_000, b"t-1", b"r-1").is_empty());
    assert!(!encode_tournament_layout(b"l-1", "Layout", 2, 150_000).is_empty());
    assert!(!encode_tournament_ruleset(b"r-1", "Ruleset", 100, 10).is_empty());

    // UI encoders
    assert!(!encode_ui_annotation("k", "v").is_empty());
    assert!(encode_ui_annotation("", "").is_empty());
    assert!(!encode_ui_contenttag("tag1").is_empty());
    assert!(encode_ui_contenttag("").is_empty());
    assert!(!encode_ui_icon("icon1").is_empty());
    assert!(encode_ui_icon("").is_empty());
    assert!(!encode_ui_image("res:/ui/img.png", 64, 64).is_empty());
    assert!(encode_ui_image("", 0, 0).is_empty());

    // Universe encoders
    assert!(!encode_universe_location(30_000_142).is_empty());
    assert!(!encode_universe_station(60_003_760).is_empty());
    assert!(!encode_universe_constellation(20_000_020).is_empty());

    // Vanguard encoders
    assert!(!encode_vanguard_commission(b"comm-1").is_empty());
    assert!(!encode_vanguard_assignment(b"comm-1", "eliminate").is_empty());

    // Vendors encoders
    assert!(!encode_epic_user("epic-1").is_empty());
    assert!(encode_epic_user("").is_empty());
    assert!(!encode_epic_entitlement("ent-1", "Omega", "active", 1_700_000_000).is_empty());
    assert!(!encode_steam_user("765611", "Pilot", "IS").is_empty());
    assert!(!encode_twitch_entitlement(b"tw-1", "FULFILLED", 1_700_000_000).is_empty());
    assert!(!encode_contentful_asset("asset-1").is_empty());
    assert!(!encode_payletter_receipt("tx-1", "AUTH").is_empty());

    // War & Wormhole encoders
    assert!(!encode_war_identifier(10).is_empty());
    assert!(!encode_war_entity_identifier(Some(99_000_001), Some(1_000_009)).is_empty());
    assert!(!encode_war_attributes(1_000_170, 1_000_171, true, false, 1_700_000_000).is_empty());
    assert!(!encode_war_destruction_statistics(5, 100_000_000).is_empty());
    assert!(!encode_wormhole_identifier(999).is_empty());
}

#[test]
fn notice_publishers_publish_to_gateway() {
    let (router, _db, _sde) = setup_router();
    let now = 1_700_000_000;

    assert!(publish_thirdpartyapi_request_received(
        &router,
        NoticeScope::Broadcast,
        90_000_001,
        "tranquility",
        "/v1/vars",
        1,
        "Agent",
        now
    )
    .is_ok());

    assert!(publish_vanguard_commission_completed(
        &router,
        NoticeScope::User(1),
        1,
        b"comm-1",
        "assault",
        "daily",
        500_001,
        now
    )
    .is_ok());

    assert!(publish_vanguard_skirmish_completed(&router, NoticeScope::Broadcast, 1, 2, now).is_ok());
    assert!(publish_vanguard_contract_completed(&router, NoticeScope::Broadcast, true, now).is_ok());
    assert!(publish_vanguard_contract_completed(&router, NoticeScope::Broadcast, false, now).is_ok());

    assert!(publish_twitch_drop_received(&router, NoticeScope::User(1), 1, b"tw-1", 55, now).is_ok());
    assert!(publish_fusionauth_user_linked(&router, NoticeScope::User(1), b"fa-1", 1, now).is_ok());
    assert!(publish_war_declared_notice(&router, NoticeScope::Broadcast, 1, 100, 200, now).is_ok());
    assert!(publish_war_end_scheduled_notice(&router, NoticeScope::Broadcast, 1, now + 3600, now).is_ok());
}

#[test]
fn registered_handlers_for_tournament_universe_vanguard_vendors_and_war() {
    let (router, db, _sde) = setup_router();
    let ctx = AuthoritativeContext::for_session(Some(1), Some(90_000_001), Some(1_000_009), 0, "tranquility", vec![1])
        .with_service_roles(eve_machonet::service_roles::ROLE_ADMIN);

    // 1. ThirdPartyApi
    let mut tpa_req = DynamicQuasarMessage::new("eve.thirdpartyapi.RequestReceived");
    tpa_req.set(2, DynamicValue::Uint64(90_000_001));
    tpa_req.set(3, DynamicValue::String("tranquility".into()));
    tpa_req.set(5, DynamicValue::String("/v2/characters".into()));
    tpa_req.set(6, DynamicValue::Uint32(2));
    tpa_req.set(7, DynamicValue::String("TestUA".into()));
    assert!(invoke(&router, "eve.thirdpartyapi.RequestReceived", &ctx, &tpa_req).is_ok());

    // 2. Tournament handlers
    for endpoint in [
        "eve.tournament.arena.api.GetAllRequest",
        "eve.tournament.arena.api.GetSettingsRequest",
        "eve.tournament.arena.layout.api.GetRequest",
        "eve.tournament.ruleset.api.GetAllRequest",
        "eve.tournament.ruleset.api.GetRequest",
        "eve.tournament.arena.api.CreateRequest",
        "eve.tournament.arena.api.DeleteRequest",
    ] {
        let mut req = DynamicQuasarMessage::new(endpoint);
        req.set(1, DynamicValue::Bytes(b"uuid-test".to_vec()));
        assert!(invoke(&router, endpoint, &ctx, &req).is_ok(), "{endpoint}");
    }

    // 3. Universe navigation & constellation handlers
    for endpoint in [
        "eve.universe.navigation.GetShortestGateJumpCountRequest",
        "eve.universe.navigation.GetShortestSystemJumpCountRequest",
    ] {
        let empty = DynamicQuasarMessage::new(endpoint);
        assert_eq!(invoke(&router, endpoint, &ctx, &empty).unwrap_err().0, STATUS_BAD_REQUEST);

        let mut same = DynamicQuasarMessage::new(endpoint);
        same.set(1, DynamicValue::Uint32(30_000_142));
        same.set(2, DynamicValue::Uint32(30_000_142));
        let rsp_same = invoke(&router, endpoint, &ctx, &same).unwrap();
        assert!(rsp_same.value.is_empty() || WireMessage::decode(&rsp_same.value).unwrap().fields[0].value.as_varint() == Some(0));

        let mut diff = DynamicQuasarMessage::new(endpoint);
        diff.set(1, DynamicValue::Uint32(30_000_142));
        diff.set(2, DynamicValue::Uint32(30_000_144));
        let rsp_diff = invoke(&router, endpoint, &ctx, &diff).unwrap();
        assert!(!rsp_diff.value.is_empty());
    }

    let mut reg_req = DynamicQuasarMessage::new("eve.universe.constellation.GetAllInRegionRequest");
    assert_eq!(
        invoke(&router, "eve.universe.constellation.GetAllInRegionRequest", &ctx, &reg_req)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    reg_req.set(1, DynamicValue::Uint32(10_000_002));
    assert!(invoke(&router, "eve.universe.constellation.GetAllInRegionRequest", &ctx, &reg_req).is_ok());
    reg_req.set(1, DynamicValue::Uint32(99_999_999));
    assert!(invoke(&router, "eve.universe.constellation.GetAllInRegionRequest", &ctx, &reg_req).is_ok());

    // 4. Vanguard & 5. Vendors
    for endpoint in [
        "eve.vanguard.commission.assignment.admin.api.TriggerCompletedEventRequest",
        "eve.vendors.epic.api.GetUserByEveUserIDRequest",
        "eve.vendors.epic.api.GetEntitlementsRequest",
        "eve.vendors.steam.user.api.GetByEveUserRequest",
        "eve.vendors.payletter.GetUserIdentityVerificationReceiptRequest",
    ] {
        let empty = DynamicQuasarMessage::new(endpoint);
        assert_eq!(invoke(&router, endpoint, &ctx, &empty).unwrap_err().0, STATUS_BAD_REQUEST);

        let mut req = DynamicQuasarMessage::new(endpoint);
        req.set(1, DynamicValue::Uint64(1001));
        assert!(invoke(&router, endpoint, &ctx, &req).is_ok(), "{endpoint}");
    }

    for endpoint in [
        "eve.vendors.steam.user.api.GetRequest",
        "eve.vendors.steam.user.api.CreateRequest",
        "eve.vendors.fusionauth.api.LinkUserRequest",
    ] {
        let req = DynamicQuasarMessage::new(endpoint);
        assert!(invoke(&router, endpoint, &ctx, &req).is_ok(), "{endpoint}");
    }

    // 6. War handlers (empty db vs populated war)
    assert!(invoke(
        &router,
        "eve.war.GetAllRequest",
        &ctx,
        &DynamicQuasarMessage::new("eve.war.GetAllRequest")
    )
    .is_ok());

    db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            77,
            WarRecord {
                war_id: 77,
                aggressor_corp_id: 1_000_170,
                defender_corp_id: 1_000_171,
                mutual: true,
                started_unix: 1_700_000_000,
                active: true,
            },
        );
    });
    assert!(invoke(
        &router,
        "eve.war.GetAllRequest",
        &ctx,
        &DynamicQuasarMessage::new("eve.war.GetAllRequest")
    )
    .is_ok());

    for endpoint in [
        "eve.war.GetRequest",
        "eve.war.GetDestructionStatisticsRequest",
        "eve.war.GetAlliesRequest",
    ] {
        let empty = DynamicQuasarMessage::new(endpoint);
        assert_eq!(invoke(&router, endpoint, &ctx, &empty).unwrap_err().0, STATUS_BAD_REQUEST);

        let mut req = DynamicQuasarMessage::new(endpoint);
        req.set(1, DynamicValue::Uint64(77));
        assert!(invoke(&router, endpoint, &ctx, &req).is_ok(), "{endpoint}");
    }
}
