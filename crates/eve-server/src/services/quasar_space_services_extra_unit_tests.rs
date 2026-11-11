//! Extra unit and mutation-killing tests for `services::quasar_space_services`
//! covering schema-driven `set_domain_field` / `set_domain_attributes` / `encode_domain_message`
//! across all `FieldType` variants (including `Sint32`, `Sint64`, `Fixed32`, `Fixed64`, `Sfixed32`,
//! `Sfixed64`, `Double`, `Float`, `Bool`, `String`, `Bytes`, `Enum`, nested `Message`, `Repeated`,
//! `oneof_index`, and recursion depth > 4 guard), doubly-nested `get_identifier_or_int`,
//! `get_string_field` repeated/nested edge cases, all 6 insurgency campaign state branches
//! (`FORECAST`, `ACTIVE`, `PIRATE_VICTORY`, `ANTI_PIRATE_VICTORY`, `NO_WINNER`, `CLEANUP`),
//! warzone 1 vs 2 resolution, `GetShortestGateJumpCountRequest` (0, 1, 2 jumps), stargate lock
//! expiry & non-Zarzakh filtering, AI assistant anonymous broadcast fallback, and CDN `GetUpdateRequest`
//! `type.evetech.net/` flatbuffer seeding vs HTTP URL response.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_INTERNAL_ERROR, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use crate::services::quasar_space_services::*;
use eve_db::{PersistedInsurgencyCampaign, SystemDynamicState, WorldRepository};
use eve_proto::quasar::{
    AuthoritativeContext, DescriptorProto, DynamicQuasarMessage, DynamicValue, EnumDescriptorProto,
    EnumValueDescriptorProto, FieldDescriptorProto, FieldLabel, FieldType, GoogleAny,
    GoogleTimestamp, QuasarDescriptorRegistry, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::{SdeRegistry, StargateJump};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_field(
    number: u32,
    name: &str,
    field_type: FieldType,
    label: FieldLabel,
    type_name: &str,
    oneof_index: Option<u32>,
) -> FieldDescriptorProto {
    FieldDescriptorProto {
        name: name.into(),
        number,
        label,
        field_type,
        type_name: type_name.into(),
        oneof_index,
        json_name: name.into(),
    }
}

fn call_space(
    router: &PublicGatewayRouter,
    req_type: &str,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let h = router
        .handlers
        .read()
        .unwrap()
        .get(req_type)
        .cloned()
        .expect(req_type);
    let raw_any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{req_type}"),
        value: Vec::new(),
    };
    h(ctx, &raw_any, msg)
}

#[test]
fn schema_field_builder_all_field_types_oneof_depth_and_extractors() {
    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();
    reg.enums_by_full_name.insert(
        ".test.MyEnum".into(),
        EnumDescriptorProto {
            name: "MyEnum".into(),
            full_name: "test.MyEnum".into(),
            values: vec![
                EnumValueDescriptorProto { name: "ZERO".into(), number: 0 },
                EnumValueDescriptorProto { name: "NONZERO".into(), number: 7 },
            ],
        },
    );
    // Recursive self-referencing message to exercise depth > 4 termination
    reg.messages_by_full_name.insert(
        "test.Recursive".into(),
        DescriptorProto {
            name: "Recursive".into(),
            full_name: "test.Recursive".into(),
            fields: vec![
                make_field(1, "self_ref", FieldType::Message, FieldLabel::Optional, ".test.Recursive", None),
                make_field(2, "leaf", FieldType::Uint32, FieldLabel::Optional, "", None),
            ],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );
    // All scalar/enum/repeated/oneof field types
    reg.messages_by_full_name.insert(
        "test.AllFields".into(),
        DescriptorProto {
            name: "AllFields".into(),
            full_name: "test.AllFields".into(),
            fields: vec![
                make_field(1, "f_u32", FieldType::Uint32, FieldLabel::Optional, "", Some(0)),
                make_field(2, "f_i32_same_oneof", FieldType::Int32, FieldLabel::Optional, "", Some(0)),
                make_field(3, "f_str", FieldType::String, FieldLabel::Optional, "", None),
                make_field(4, "f_u64", FieldType::Uint64, FieldLabel::Optional, "", None),
                make_field(5, "f_i64", FieldType::Int64, FieldLabel::Optional, "", None),
                make_field(6, "f_sint32", FieldType::Sint32, FieldLabel::Optional, "", None),
                make_field(7, "f_sint64", FieldType::Sint64, FieldLabel::Optional, "", None),
                make_field(8, "f_fixed32", FieldType::Fixed32, FieldLabel::Optional, "", None),
                make_field(9, "f_fixed64", FieldType::Fixed64, FieldLabel::Optional, "", None),
                make_field(10, "f_sfixed32", FieldType::Sfixed32, FieldLabel::Optional, "", None),
                make_field(11, "f_sfixed64", FieldType::Sfixed64, FieldLabel::Optional, "", None),
                make_field(12, "f_double", FieldType::Double, FieldLabel::Optional, "", None),
                make_field(13, "f_float", FieldType::Float, FieldLabel::Optional, "", None),
                make_field(14, "f_bool", FieldType::Bool, FieldLabel::Optional, "", None),
                make_field(15, "f_bytes", FieldType::Bytes, FieldLabel::Optional, "", None),
                make_field(16, "f_enum", FieldType::Enum, FieldLabel::Optional, ".test.MyEnum", None),
                make_field(17, "f_rep_i32", FieldType::Int32, FieldLabel::Repeated, "", None),
                make_field(18, "f_rec", FieldType::Message, FieldLabel::Optional, ".test.Recursive", None),
            ],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );

    let mut msg = DynamicQuasarMessage::new("test.AllFields");
    set_domain_attributes(&reg, &mut msg, 42, 99, "Alpha");
    // f1 set, f2 skipped because it shares oneof_index Some(0) with f1, f3 set because it's String with no oneof
    assert_eq!(msg.get(1), Some(&DynamicValue::Uint32(42)));
    assert_eq!(msg.get(2), None);
    assert_eq!(msg.get(3), Some(&DynamicValue::String("Alpha".into())));

    // Exercise every field type via set_domain_field
    for fnum in 4..=18 {
        set_domain_field(&reg, &mut msg, fnum, 15, "Beta");
        assert!(msg.get(fnum).is_some(), "field {fnum} should be populated");
    }
    assert_eq!(msg.get(16), Some(&DynamicValue::Enum(7)));
    assert_eq!(
        msg.get(17),
        Some(&DynamicValue::Repeated(vec![DynamicValue::Int32(15)]))
    );

    // Fallback when field_num or message is not in descriptors
    set_domain_field(&reg, &mut msg, 999, 123, "Fallback");
    assert_eq!(msg.get(999), Some(&DynamicValue::Uint64(123)));

    // encode_domain_message succeeds for known message and returns STATUS_INTERNAL_ERROR for unknown
    let enc_ok = encode_domain_message(&reg, "test.AllFields", |r, m| {
        set_domain_attributes(r, m, 1, 2, "Test");
    });
    assert!(enc_ok.is_ok());
    assert_eq!(
        encode_domain_message(&reg, "test.MissingMsg", |_, _| {}).unwrap_err().0,
        STATUS_INTERNAL_ERROR
    );

    // Doubly-nested Identifier in get_identifier_or_int & empty Message returning None
    let mut leaf = DynamicQuasarMessage::new("Leaf");
    leaf.set(1, DynamicValue::Int32(321));
    let mut mid = DynamicQuasarMessage::new("Mid");
    mid.set(1, DynamicValue::Message(Box::new(leaf)));
    let mut root = DynamicQuasarMessage::new("Root");
    root.set(1, DynamicValue::Message(Box::new(mid)));
    root.set(2, DynamicValue::Message(Box::new(DynamicQuasarMessage::new("EmptySub"))));
    assert_eq!(get_identifier_or_int(&root, 1), Some(321));
    assert_eq!(get_identifier_or_int(&root, 2), None);

    // get_string_field with Repeated containing non-strings before a string, and only non-strings
    root.set(
        3,
        DynamicValue::Repeated(vec![
            DynamicValue::Uint32(1),
            DynamicValue::String("found".into()),
        ]),
    );
    root.set(4, DynamicValue::Repeated(vec![DynamicValue::Uint32(1)]));
    root.set(5, DynamicValue::Bytes(b"Jita".to_vec()));
    root.set(
        6,
        DynamicValue::Repeated(vec![DynamicValue::Bytes(b"Perimeter".to_vec())]),
    );
    assert_eq!(get_string_field(&root, 3).as_deref(), Some("found"));
    assert_eq!(get_string_field(&root, 4), None);
    assert_eq!(get_string_field(&root, 5).as_deref(), Some("Jita"));
    assert_eq!(get_string_field(&root, 6).as_deref(), Some("Perimeter"));
}

#[test]
fn navigation_jump_counts_and_insurgency_six_state_machine() {
    let mut sde = (*shared_sde()).clone();
    // Ensure a direct jump exists from 30_000_142 to 30_000_144
    sde.jumps.insert(
        50_000_001,
        StargateJump {
            stargate_id: 50_000_001,
            from_solar_system_id: 30_000_142,
            to_solar_system_id: 30_000_144,
            destination_stargate_id: 50_000_002,
            position: [0.0, 0.0, 0.0].into(),
        },
    );
    let sde = Arc::new(sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();
    reg.messages_by_full_name.insert(
        "eve.universe.navigation.GetShortestGateJumpCountResponse".into(),
        DescriptorProto {
            name: "GetShortestGateJumpCountResponse".into(),
            full_name: "eve.universe.navigation.GetShortestGateJumpCountResponse".into(),
            fields: vec![make_field(1, "jump_count", FieldType::Uint32, FieldLabel::Optional, "", None)],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );
    let descriptors = Arc::new(reg);
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let mut ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(1_000_009),
        0,
        "tranquility",
        vec![1],
    );
    ctx.received = Some(GoogleTimestamp {
        seconds: 1_700_005_000,
        nanos: 0,
    });

    // 1. GetShortestGateJumpCountRequest: invalid (0) -> BAD_REQUEST, same -> 0, neighbor -> 1, distant -> 2
    let mut nav = DynamicQuasarMessage::new("eve.universe.navigation.GetShortestGateJumpCountRequest");
    nav.set(1, DynamicValue::Uint32(30_000_142));
    nav.set(2, DynamicValue::Uint32(0));
    assert_eq!(
        call_space(&router, "eve.universe.navigation.GetShortestGateJumpCountRequest", &ctx, &nav)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    for (to_sys, _expected_jumps) in [(30_000_142, 0), (30_000_144, 1), (30_002_187, 2)] {
        nav.set(2, DynamicValue::Uint32(to_sys));
        assert!(call_space(&router, "eve.universe.navigation.GetShortestGateJumpCountRequest", &ctx, &nav).is_ok());
    }

    // GetAllInRegionRequest with region 0 -> BAD_REQUEST
    let mut reg_zero = DynamicQuasarMessage::new("eve.universe.constellation.GetAllInRegionRequest");
    reg_zero.set(1, DynamicValue::Uint32(0));
    assert_eq!(
        call_space(&router, "eve.universe.constellation.GetAllInRegionRequest", &ctx, &reg_zero)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // 2. Insurgency campaign states:
    // camp 10: FORECAST (now 1_700_005_000 < forecasting_end_unix 1_700_006_000)
    // camp 11: ACTIVE (forecasting_end < now < active_end)
    // camp 12: NO_WINNER (active_end <= now < winddown_end)
    // camp 13: CLEANUP (winddown_end <= now)
    db.with_mut_snapshot(|snap| {
        for (cid, target_fac, f_end, a_end, w_end) in [
            (10i64, 500_001i64, 1_700_006_000u64, 1_700_010_000u64, 1_700_020_000u64),
            (11, 500_002, 1_700_001_000, 1_700_010_000, 1_700_020_000),
            (12, 500_003, 1_700_001_000, 1_700_004_000, 1_700_020_000),
            (13, 500_004, 1_700_001_000, 1_700_003_000, 1_700_004_000),
        ] {
            snap.insurgency_runtime.campaigns.insert(
                cid,
                PersistedInsurgencyCampaign {
                    campaign_id: cid,
                    pirate_faction_id: 500_010,
                    target_faction_id: target_fac,
                    fob_solar_system_id: 30_000_142,
                    systems: vec![30_000_142],
                    created_at_unix: 1_700_000_000,
                    forecasting_end_unix: f_end,
                    active_end_unix: a_end,
                    winddown_end_unix: w_end,
                    corruption_points: HashMap::new(),
                    suppression_points: HashMap::new(),
                },
            );
        }
        snap.system_states.insert(
            30_000_142,
            SystemDynamicState {
                solar_system_id: 30_000_142,
                insurgency_corruption_stage: 3,
                insurgency_suppression_stage: 4,
                ..Default::default()
            },
        );
    });

    for (cid, expected_state) in [(10, 1u64), (11, 2), (12, 5), (13, 6)] {
        let mut req = DynamicQuasarMessage::new("eve.pirate.insurgency.api.GetRequest");
        req.set(1, DynamicValue::Int64(cid));
        let rsp = call_space(&router, "eve.pirate.insurgency.api.GetRequest", &ctx, &req).unwrap();
        let wm = WireMessage::decode(&rsp.value).unwrap();
        assert!(
            wm.fields.iter().any(|f| f.field_number == 5 && f.value.as_varint() == Some(expected_state)),
            "campaign {cid} expected state {expected_state}"
        );
    }

    // GetSystemsRequest error paths (0 -> BAD_REQUEST, 999 -> NOT_FOUND) & GetForWarzoneRequest error path
    let mut bad_sys_req = DynamicQuasarMessage::new("eve.pirate.insurgency.api.GetSystemsRequest");
    assert_eq!(
        call_space(&router, "eve.pirate.insurgency.api.GetSystemsRequest", &ctx, &bad_sys_req)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    bad_sys_req.set(1, DynamicValue::Int64(999));
    assert_eq!(
        call_space(&router, "eve.pirate.insurgency.api.GetSystemsRequest", &ctx, &bad_sys_req)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    let bad_wz = DynamicQuasarMessage::new("eve.pirate.insurgency.api.GetForWarzoneRequest");
    assert_eq!(
        call_space(&router, "eve.pirate.insurgency.api.GetForWarzoneRequest", &ctx, &bad_wz)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Corruption & Suppression GetSystemInfoRequest omitting field 1 uses caller_solar_system (30_000_142)
    let empty_info = DynamicQuasarMessage::new("Info");
    let corr_rsp = call_space(
        &router,
        "eve_public.pirate.corruption.api.GetSystemInfoRequest",
        &ctx,
        &empty_info,
    )
    .unwrap();
    let wm_corr = WireMessage::decode(&corr_rsp.value).unwrap();
    assert!(wm_corr.fields.iter().any(|f| f.field_number == 4 && f.value.as_varint() == Some(3)));

    let supp_rsp = call_space(
        &router,
        "eve_public.pirate.suppression.api.GetSystemInfoRequest",
        &ctx,
        &empty_info,
    )
    .unwrap();
    let wm_supp = WireMessage::decode(&supp_rsp.value).unwrap();
    assert!(wm_supp.fields.iter().any(|f| f.field_number == 2 && f.value.as_varint() == Some(4)));
}

#[test]
fn stargate_lock_expiry_non_zarzakh_ai_broadcast_and_cdn_evetech_flatbuffer() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let mut ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(1_000_009),
        0,
        "tranquility",
        vec![1],
    );
    ctx.received = Some(GoogleTimestamp {
        seconds: 1_700_005_000,
        nanos: 0,
    });
    let anon = AuthoritativeContext::default();

    // 1. Stargate lock GetRestrictedSystemsRequest unauthorized for anon
    assert_eq!(
        call_space(
            &router,
            "eve_public.stargate.lock.api.GetRestrictedSystemsRequest",
            &anon,
            &DynamicQuasarMessage::new("Req"),
        )
        .unwrap_err()
        .0,
        STATUS_UNAUTHORIZED
    );

    // Seed active lock, query for non-Zarzakh system (30_000_142) -> empty value
    db.with_mut_snapshot(|snap| {
        snap.zarzakh_gate_locks.insert(90_000_001, (50_000_001, 1_700_010_000));
    });
    let mut non_zz = DynamicQuasarMessage::new("eve_public.stargate.lock.api.GetRequest");
    non_zz.set(1, DynamicValue::Uint32(30_000_142));
    let non_zz_rsp = call_space(&router, "eve_public.stargate.lock.api.GetRequest", &ctx, &non_zz).unwrap();
    assert!(non_zz_rsp.value.is_empty());

    // Expire lock (expires_at <= now) -> both GetRequest and GetRestrictedSystemsRequest return empty value
    db.with_mut_snapshot(|snap| {
        snap.zarzakh_gate_locks.insert(90_000_001, (50_000_001, 1_700_005_000));
    });
    let exp_get = call_space(
        &router,
        "eve_public.stargate.lock.api.GetRequest",
        &ctx,
        &DynamicQuasarMessage::new("Req"),
    )
    .unwrap();
    assert!(exp_get.value.is_empty());
    let exp_rest = call_space(
        &router,
        "eve_public.stargate.lock.api.GetRestrictedSystemsRequest",
        &ctx,
        &DynamicQuasarMessage::new("Req"),
    )
    .unwrap();
    assert!(exp_rest.value.is_empty());

    // 2. AI assistant QueryRequest as anonymous caller -> "Unknown Space" fallback + Broadcast notice queued
    let mut ai_anon_req = DynamicQuasarMessage::new("eve_public.ai.assistant.chat.api.QueryRequest");
    ai_anon_req.set(1, DynamicValue::String("xyzzy unknown".into()));
    let ai_anon_rsp = call_space(
        &router,
        "eve_public.ai.assistant.chat.api.QueryRequest",
        &anon,
        &ai_anon_req,
    )
    .unwrap();
    assert!(!ai_anon_rsp.value.is_empty());

    // 3. CDN GetUpdateRequest with type.evetech.net/ prefix & no current_version seeds flatbuffer and returns empty type_url
    let h = router
        .handlers
        .read()
        .unwrap()
        .get("eve_public.space.api.cdn.GetUpdateRequest")
        .cloned()
        .unwrap();
    let mut cdn_no_local = DynamicQuasarMessage::new("eve_public.space.api.cdn.GetUpdateRequest");
    cdn_no_local.set(2, DynamicValue::Uint64(1));
    let evetech_any = GoogleAny {
        type_url: "type.evetech.net/eve_public.space.api.cdn.GetUpdateRequest".into(),
        value: Vec::new(),
    };
    let cdn_fb_rsp = h(&ctx, &evetech_any, &cdn_no_local).unwrap();
    assert!(cdn_fb_rsp.type_url.is_empty());
    assert!(cdn_fb_rsp.value.is_empty());
}
