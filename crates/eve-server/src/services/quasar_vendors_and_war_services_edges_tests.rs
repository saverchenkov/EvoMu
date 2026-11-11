//! Edge-case, error-guard, boundary, and state-transition unit tests for `quasar_vendors_and_war_services`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN};
use eve_db::IskCents;
use eve_machonet::service_roles;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry, WireMessage,
    WireValue,
};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn setup() -> (Arc<PublicGatewayRouter>, Arc<WorldRepository>, Arc<SdeRegistry>) {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = Arc::new(PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
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
    let handler = router.handlers.read().unwrap()[req_type].clone();
    let raw = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{req_type}"),
        value: Vec::new(),
    };
    handler(ctx, &raw, msg)
}

#[test]
fn wire_extractors_and_encoder_boundary_cases() {
    // 1. extract_u64_checked across Sint64/Int64/Sint32/Int32 negative values, deep nesting, and field-2 fallback
    let mut m = DynamicQuasarMessage::new("Test");
    m.set(1, DynamicValue::Int64(-42));
    assert_eq!(extract_u64_checked(&m, 1).unwrap(), Some(42));
    m.set(2, DynamicValue::Sint64(-99));
    assert_eq!(extract_u64_checked(&m, 2).unwrap(), Some(99));
    m.set(3, DynamicValue::Int32(-7));
    assert_eq!(extract_u64_checked(&m, 3).unwrap(), Some(7));
    m.set(4, DynamicValue::Sint32(-13));
    assert_eq!(extract_u64_checked(&m, 4).unwrap(), Some(13));
    m.set(5, DynamicValue::Bool(true));
    assert_eq!(extract_u64_checked(&m, 5).unwrap(), None);

    // Nested field 1 -> inner field 1 -> Int32(-15)
    let mut inner = DynamicQuasarMessage::new("Inner");
    inner.set(1, DynamicValue::Int32(-15));
    let mut mid = DynamicQuasarMessage::new("Mid");
    mid.set(1, DynamicValue::Message(Box::new(inner)));
    let mut outer = DynamicQuasarMessage::new("Outer");
    outer.set(1, DynamicValue::Message(Box::new(mid)));
    assert_eq!(extract_u64_checked(&outer, 1).unwrap(), Some(15));

    // Submessage with field 1 absent falls back to field 2
    let mut sub_f2 = DynamicQuasarMessage::new("SubF2");
    sub_f2.set(2, DynamicValue::Uint64(555));
    outer.set(2, DynamicValue::Message(Box::new(sub_f2)));
    assert_eq!(extract_u64_checked(&outer, 2).unwrap(), Some(555));

    // extract_required_u64 & extract_required_u32 zero / u32 overflow guards
    let mut zero_m = DynamicQuasarMessage::new("Z");
    zero_m.set(1, DynamicValue::Uint64(0));
    assert_eq!(extract_required_u64(&zero_m, 1, "id").unwrap_err().0, STATUS_BAD_REQUEST);
    let mut big_m = DynamicQuasarMessage::new("B");
    big_m.set(1, DynamicValue::Uint64(u64::from(u32::MAX) + 1));
    assert_eq!(extract_required_u32(&big_m, 1, "sys").unwrap_err().0, STATUS_BAD_REQUEST);

    // 2. extract_string_field & extract_uuid_bytes direct, nested, and missing
    let mut sm = DynamicQuasarMessage::new("S");
    sm.set(1, DynamicValue::String("direct".into()));
    assert_eq!(extract_string_field(&sm, 1), Some("direct"));
    let mut s_sub = DynamicQuasarMessage::new("Sub");
    s_sub.set(1, DynamicValue::String("nested".into()));
    sm.set(2, DynamicValue::Message(Box::new(s_sub)));
    assert_eq!(extract_string_field(&sm, 2), Some("nested"));
    sm.set(3, DynamicValue::Uint32(1));
    assert_eq!(extract_string_field(&sm, 3), None);

    let mut um = DynamicQuasarMessage::new("U");
    let mut u_sub = DynamicQuasarMessage::new("USub");
    u_sub.set(1, DynamicValue::Bytes(vec![1, 2, 3]));
    um.set(1, DynamicValue::Message(Box::new(u_sub)));
    assert_eq!(extract_uuid_bytes(&um, 1), Some(vec![1, 2, 3]));
    assert_eq!(extract_uuid_bytes(&um, 2), None);

    // 3. encode_isk_currency negative cents & sub-unit cents (units==0, nanos!=0)
    let mut neg_buf = Vec::new();
    encode_isk_currency(1, IskCents(-12_345), &mut neg_buf);
    let wm_neg = WireMessage::decode(&neg_buf).unwrap();
    let inner_neg = WireMessage::decode(wm_neg.fields[0].value.as_bytes().unwrap()).unwrap();
    // -123 units zigzag = 245, -45_000_0000 nanos zigzag
    assert!(inner_neg.fields.iter().any(|f| f.field_number == 1 && f.value == WireValue::Varint(245)));
    assert!(inner_neg.fields.iter().any(|f| f.field_number == 2));

    let mut sub_isk = Vec::new();
    encode_isk_currency(1, IskCents(50), &mut sub_isk);
    let wm_sub = WireMessage::decode(&sub_isk).unwrap();
    let inner_sub = WireMessage::decode(wm_sub.fields[0].value.as_bytes().unwrap()).unwrap();
    assert!(!inner_sub.fields.iter().any(|f| f.field_number == 1));
    assert!(inner_sub.fields.iter().any(|f| f.field_number == 2));

    // 4. Encoders with empty/zero optional inputs + negative char_id + small now_unix
    let tpa_neg = encode_thirdpartyapi_request_received(-42, "", "", 0, "", 100);
    assert!(!tpa_neg.is_empty());
    assert!(!encode_tournament_arena(b"a", "", 0, 0, &[], &[]).is_empty());
    assert!(encode_tournament_layout(&[], "", 0, 0).is_empty());
    assert!(encode_tournament_ruleset(&[], "", 0, 0).is_empty());
    assert!(encode_ui_image("", 0, 0).is_empty());
    assert!(!encode_ui_image("", 0, 32).is_empty());
    assert!(encode_war_entity_identifier(None, None).is_empty());
    assert!(!encode_war_attributes(100, 200, false, false, 1_000).is_empty());
}

#[test]
fn admin_guards_and_handler_fallback_branches() {
    let (router, db, _sde) = setup();
    let player = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(1_000_009),
        0,
        "tranquility",
        vec![1],
    );
    let admin = player.clone().with_service_roles(service_roles::ROLE_ADMIN);
    let no_char = AuthoritativeContext::for_session(Some(1), None, None, 0, "tranquility", vec![1]);

    // 1. All admin-gated endpoints reject non-admin with 403 FORBIDDEN
    for ep in [
        "eve.tournament.arena.api.CreateRequest",
        "eve.tournament.arena.api.DeleteRequest",
        "eve.vanguard.commission.assignment.admin.api.TriggerCompletedEventRequest",
        "eve.vendors.epic.api.GetUserByEveUserIDRequest",
        "eve.vendors.epic.api.GetEntitlementsRequest",
        "eve.vendors.steam.user.api.GetRequest",
        "eve.vendors.steam.user.api.GetByEveUserRequest",
        "eve.vendors.steam.user.api.CreateRequest",
        "eve.vendors.fusionauth.api.LinkUserRequest",
        "eve.vendors.payletter.GetUserIdentityVerificationReceiptRequest",
    ] {
        assert_eq!(
            invoke(&router, ep, &player, &DynamicQuasarMessage::new(ep))
                .unwrap_err()
                .0,
            STATUS_FORBIDDEN,
            "{ep}"
        );
    }

    // 2. ThirdPartyApi defaults when all fields omitted
    let tpa_def = invoke(
        &router,
        "eve.thirdpartyapi.RequestReceived",
        &player,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    assert!(!tpa_def.value.is_empty());

    // 3. Tournament arena GetAllRequest: fallback to caller's solar_system_id, and 0 when no character
    let arena_char = invoke(
        &router,
        "eve.tournament.arena.api.GetAllRequest",
        &player,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    assert!(!arena_char.value.is_empty());
    let arena_nochar = invoke(
        &router,
        "eve.tournament.arena.api.GetAllRequest",
        &no_char,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    assert!(!arena_nochar.value.is_empty());

    // 4. Navigation error guards: missing field 2 (destination/end) -> 400; u32 overflow -> 400
    for ep in [
        "eve.universe.navigation.GetShortestGateJumpCountRequest",
        "eve.universe.navigation.GetShortestSystemJumpCountRequest",
    ] {
        let mut only_origin = DynamicQuasarMessage::new(ep);
        only_origin.set(1, DynamicValue::Uint32(30_000_142));
        assert_eq!(invoke(&router, ep, &player, &only_origin).unwrap_err().0, STATUS_BAD_REQUEST);

        let mut overflow = DynamicQuasarMessage::new(ep);
        overflow.set(1, DynamicValue::Uint64(u64::from(u32::MAX) + 5));
        overflow.set(2, DynamicValue::Uint32(30_000_142));
        assert_eq!(invoke(&router, ep, &player, &overflow).unwrap_err().0, STATUS_BAD_REQUEST);
    }

    // 5. War GetRequest for non-existent war uses default corps (1_000_170, 1_000_171)
    let mut war_missing = DynamicQuasarMessage::new("W");
    war_missing.set(1, DynamicValue::Uint64(999_888));
    let war_rsp = invoke(&router, "eve.war.GetRequest", &player, &war_missing).unwrap();
    assert!(!war_rsp.value.is_empty());

    // 6. Steam GetRequest with custom steam_id vs default
    let mut steam_req = DynamicQuasarMessage::new("Steam");
    steam_req.set(1, DynamicValue::String("76561198999999999".into()));
    let steam_rsp = invoke(&router, "eve.vendors.steam.user.api.GetRequest", &admin, &steam_req).unwrap();
    assert!(!steam_rsp.value.is_empty());

    // 7. Notice publisher with active subscriber verifies actual delivery count == 1
    let mut sub_sess = eve_machonet::SessionState::new(1);
    sub_sess.user_id = Some(1);
    let mut rx = router.subscribe_session(&sub_sess, Vec::new(), 1_700_000_000);
    let delivered = publish_vanguard_commission_completed(
        &router,
        NoticeScope::User(1),
        1,
        b"uuid-vanguard",
        "raid",
        "weekly",
        0, // faction_id == 0 branch omits field 4
        1_700_000_000,
    )
    .unwrap();
    assert_eq!(delivered, 1);
    let notice = rx.try_recv().unwrap();
    assert!(notice
        .payload
        .unwrap()
        .type_url
        .ends_with("eve.vanguard.commission.assignment.api.Completed"));
    let _ = db;
}
