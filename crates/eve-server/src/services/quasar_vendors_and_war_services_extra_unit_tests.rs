//! Additional branch-coverage and mutation-killing unit tests for `quasar_vendors_and_war_services.rs`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST};
use crate::services::quasar_vendors_and_war_services::*;
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn setup_router() -> (
    Arc<PublicGatewayRouter>,
    Arc<WorldRepository>,
    Arc<SdeRegistry>,
) {
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
fn isk_currency_zigzag_and_partial_field_encoders_kill_mutations() {
    // 1. encode_isk_currency: units-only, nanos-only, positive & negative zigzag encoding
    let mut b_zero = Vec::new();
    encode_isk_currency(1, IskCents(0), &mut b_zero);
    let inner_zero = WireMessage::decode(&b_zero).unwrap().fields[0]
        .value
        .as_bytes()
        .unwrap()
        .to_vec();
    assert!(inner_zero.is_empty());

    let mut b_units = Vec::new();
    encode_isk_currency(1, IskCents(500), &mut b_units);
    let wm_units = WireMessage::decode(
        WireMessage::decode(&b_units).unwrap().fields[0]
            .value
            .as_bytes()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(wm_units.fields.len(), 1);
    assert_eq!(wm_units.fields[0].field_number, 1);
    assert_eq!(wm_units.fields[0].value.as_varint(), Some(10)); // zigzag(5) == 10

    let mut b_nanos = Vec::new();
    encode_isk_currency(1, IskCents(45), &mut b_nanos);
    let wm_nanos = WireMessage::decode(
        WireMessage::decode(&b_nanos).unwrap().fields[0]
            .value
            .as_bytes()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(wm_nanos.fields.len(), 1);
    assert_eq!(wm_nanos.fields[0].field_number, 2);
    assert_eq!(wm_nanos.fields[0].value.as_varint(), Some(900_000_000));

    let mut b_neg = Vec::new();
    encode_isk_currency(1, IskCents(-125), &mut b_neg);
    let wm_neg = WireMessage::decode(
        WireMessage::decode(&b_neg).unwrap().fields[0]
            .value
            .as_bytes()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(wm_neg.fields.len(), 2);
    assert_eq!(wm_neg.fields[0].value.as_varint(), Some(1)); // zigzag(-1) == 1

    // 2. ThirdPartyApi negative char_id zigzag & selective fields
    let tpa_neg = encode_thirdpartyapi_request_received(-1, "", "", 0, "", 1_700_000_000);
    let wm_tpa = WireMessage::decode(&tpa_neg).unwrap();
    assert_eq!(wm_tpa.fields.len(), 2);
    assert_eq!(wm_tpa.fields[1].field_number, 2);
    assert_eq!(wm_tpa.fields[1].value.as_varint(), Some(1));

    // 3. Tournament arena, layout, and ruleset boundary combinations (0/empty vs >0)
    let arena_min = encode_tournament_arena(b"a", "", 0, 0, b"", b"");
    let arena_full = encode_tournament_arena(b"a", "N", 30_000_142, 50_000, b"t", b"r");
    assert!(arena_full.len() > arena_min.len());

    assert!(encode_tournament_layout(b"", "", 0, 0).is_empty());
    let layout_v_only = encode_tournament_layout(b"", "", 3, 0);
    assert_eq!(
        WireMessage::decode(&layout_v_only).unwrap().fields[0].field_number,
        2
    );
    let layout_r_only = encode_tournament_layout(b"", "", 0, 99);
    assert_eq!(
        WireMessage::decode(&layout_r_only).unwrap().fields[0].field_number,
        3
    );

    assert!(encode_tournament_ruleset(b"", "", 0, 0).is_empty());
    assert!(encode_tournament_ruleset(b"", "", -5, -1).is_empty());
    let rs_pts_only = encode_tournament_ruleset(b"", "", 50, 0);
    assert_eq!(
        WireMessage::decode(&rs_pts_only).unwrap().fields[0].field_number,
        4
    );
    let rs_ply_only = encode_tournament_ruleset(b"", "", 0, 8);
    assert_eq!(
        WireMessage::decode(&rs_ply_only).unwrap().fields[0].field_number,
        5
    );

    // 4. UI & Vanguard & War selective field encoders
    assert_eq!(
        WireMessage::decode(&encode_ui_annotation("k", ""))
            .unwrap()
            .fields[0]
            .field_number,
        1
    );
    assert_eq!(
        WireMessage::decode(&encode_ui_annotation("", "v"))
            .unwrap()
            .fields[0]
            .field_number,
        2
    );
    assert_eq!(
        WireMessage::decode(&encode_ui_image_attributes(128, 0))
            .unwrap()
            .fields[0]
            .field_number,
        1
    );
    assert_eq!(
        WireMessage::decode(&encode_ui_image_attributes(0, 256))
            .unwrap()
            .fields[0]
            .field_number,
        2
    );
    assert_eq!(
        WireMessage::decode(&encode_vanguard_assignment(b"u", ""))
            .unwrap()
            .fields
            .len(),
        1
    );
    assert!(encode_war_entity_identifier(None, None).is_empty());
    assert_eq!(
        WireMessage::decode(&encode_war_entity_identifier(Some(99), None))
            .unwrap()
            .fields[0]
            .field_number,
        1
    );
    assert_eq!(
        WireMessage::decode(&encode_war_entity_identifier(None, Some(98)))
            .unwrap()
            .fields[0]
            .field_number,
        2
    );
}

#[test]
fn dynamic_message_nested_extractors_and_handler_fallbacks_kill_mutations() {
    let (router, _db, _sde) = setup_router();
    let ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(1_000_009),
        0,
        "tranquility",
        vec![1],
    );
    let anon_ctx = AuthoritativeContext::default();

    // 1. Nested message extractors: sub.get(1) as Message(inner), Int64, Sint32, None -> sub.get(2)
    let mut nested_inner = DynamicQuasarMessage::new("Inner");
    nested_inner.set(1, DynamicValue::Sint64(-42));
    let mut nested_outer = DynamicQuasarMessage::new("Outer");
    nested_outer.set(1, DynamicValue::Message(Box::new(nested_inner)));
    let mut req_nested = DynamicQuasarMessage::new("Req");
    req_nested.set(1, DynamicValue::Message(Box::new(nested_outer)));
    assert!(invoke(
        &router,
        "eve.vendors.epic.api.GetUserByEveUserIDRequest",
        &ctx,
        &req_nested
    )
    .is_ok());

    // Submessage where field 1 is missing and field 2 has Sint32(-77)
    let mut sub_f2 = DynamicQuasarMessage::new("SubF2");
    sub_f2.set(2, DynamicValue::Sint32(-77));
    let mut req_f2 = DynamicQuasarMessage::new("ReqF2");
    req_f2.set(1, DynamicValue::Message(Box::new(sub_f2)));
    assert!(invoke(
        &router,
        "eve.vendors.epic.api.GetUserByEveUserIDRequest",
        &ctx,
        &req_f2
    )
    .is_ok());

    // Submessage with nested String and nested Bytes
    let mut str_sub = DynamicQuasarMessage::new("StrSub");
    str_sub.set(1, DynamicValue::String("76561198999999999".into()));
    let mut steam_req = DynamicQuasarMessage::new("SteamReq");
    steam_req.set(1, DynamicValue::Message(Box::new(str_sub)));
    assert!(invoke(
        &router,
        "eve.vendors.steam.user.api.GetRequest",
        &ctx,
        &steam_req
    )
    .is_ok());

    let mut uuid_sub = DynamicQuasarMessage::new("UuidSub");
    uuid_sub.set(1, DynamicValue::Bytes(b"nested-uuid".to_vec()));
    let mut layout_req = DynamicQuasarMessage::new("LayoutReq");
    layout_req.set(1, DynamicValue::Message(Box::new(uuid_sub)));
    assert!(invoke(
        &router,
        "eve.tournament.arena.layout.api.GetRequest",
        &ctx,
        &layout_req
    )
    .is_ok());

    // 2. extract_required_u32 overflow (> u32::MAX) -> STATUS_BAD_REQUEST
    let mut overflow_req = DynamicQuasarMessage::new("Overflow");
    overflow_req.set(1, DynamicValue::Uint64(u64::from(u32::MAX) + 10));
    overflow_req.set(2, DynamicValue::Uint32(30_000_142));
    assert_eq!(
        invoke(
            &router,
            "eve.universe.navigation.GetShortestGateJumpCountRequest",
            &ctx,
            &overflow_req
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Verify exact jump counts: 3 for GateJumpCount and 4 for SystemJumpCount when origin != dest
    let mut diff_req = DynamicQuasarMessage::new("Diff");
    diff_req.set(1, DynamicValue::Int32(30_000_142));
    diff_req.set(2, DynamicValue::Int64(30_000_144));
    let gate_rsp = invoke(
        &router,
        "eve.universe.navigation.GetShortestGateJumpCountRequest",
        &ctx,
        &diff_req,
    )
    .unwrap();
    assert_eq!(
        WireMessage::decode(&gate_rsp.value).unwrap().fields[0]
            .value
            .as_varint(),
        Some(3)
    );
    let sys_rsp = invoke(
        &router,
        "eve.universe.navigation.GetShortestSystemJumpCountRequest",
        &ctx,
        &diff_req,
    )
    .unwrap();
    assert_eq!(
        WireMessage::decode(&sys_rsp.value).unwrap().fields[0]
            .value
            .as_varint(),
        Some(4)
    );

    // 3. Tournament arena GetAllRequest: explicit sys_id vs character solar_system_id vs anon 0
    let empty_arena = DynamicQuasarMessage::new("ArenaAll");
    let rsp_char_sys = invoke(
        &router,
        "eve.tournament.arena.api.GetAllRequest",
        &ctx,
        &empty_arena,
    )
    .unwrap();
    let rsp_anon_sys = invoke(
        &router,
        "eve.tournament.arena.api.GetAllRequest",
        &anon_ctx,
        &empty_arena,
    )
    .unwrap();
    assert!(rsp_char_sys.value.len() > rsp_anon_sys.value.len());

    // 4. War GetRequest for unseeded war_id uses fallback (1_000_170, 1_000_171, false, true)
    let mut unknown_war = DynamicQuasarMessage::new("WarGet");
    unknown_war.set(1, DynamicValue::Uint64(999_999));
    let war_rsp = invoke(&router, "eve.war.GetRequest", &ctx, &unknown_war).unwrap();
    assert!(!war_rsp.value.is_empty());

    // 5. Vanguard commission completed with empty commission_type and faction_id == 0
    assert!(publish_vanguard_commission_completed(
        &router,
        NoticeScope::Broadcast,
        1,
        b"c",
        "",
        "",
        0,
        1_700_000_000,
    )
    .is_ok());
}
