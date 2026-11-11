//! Comprehensive edge, error, and notice tests for `quasar_cosmetics_skinr_wire`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{NoticeScope, STATUS_BAD_REQUEST, STATUS_FORBIDDEN};
use crate::services::quasar_social_chat::drain_local_notices;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, EvePublicCharacterId, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};

#[test]
fn wire_primitives_tier_tables_and_component_validation() {
    // 1. require_char
    let no_char = AuthoritativeContext::default();
    assert_eq!(require_char(&no_char).unwrap_err().0, STATUS_FORBIDDEN);
    let mut with_char = AuthoritativeContext::default();
    with_char.active_character = Some(EvePublicCharacterId { sequential: 90_000_123 });
    assert_eq!(require_char(&with_char), Ok(90_000_123));

    // 2. as_u64, sub, seq_of, str_of edge cases
    let mut m = DynamicQuasarMessage::new("Edge");
    m.set(1, DynamicValue::Bool(true)); // non-message for sub
    m.set(2, DynamicValue::Bytes(vec![0xff, 0xfe])); // invalid UTF-8 for str_of
    m.set(3, DynamicValue::Uint64(5)); // non-string/bytes for str_of
    let mut inner = DynamicQuasarMessage::new("Inner");
    inner.set(1, DynamicValue::Int32(0));
    m.set(4, DynamicValue::Message(Box::new(inner)));
    assert!(sub(&m, 1).is_none());
    assert!(seq_of(&m, 1).is_none());
    assert_eq!(seq_of(&m, 4), Some(0));
    assert!(str_of(&m, 2).is_none());
    assert!(str_of(&m, 3).is_none());

    // 3. parse_component_id_wire rejects id==0, wrong field numbers, and truncated wire bytes
    let zero_coat = encode_component_id(0, false);
    assert_eq!(parse_component_id_wire(&zero_coat), None);
    let mut bad_field = Vec::new();
    encode_varint_field(3, 42, &mut bad_field);
    assert_eq!(parse_component_id_wire(&bad_field), None);
    assert_eq!(parse_component_id_wire(&[0xff, 0xff]), None);

    // 4. put_seq / put_timestamp / put_duration roundtrips via WireMessage
    let mut ts_buf = Vec::new();
    put_seq(1, 777, &mut ts_buf);
    put_timestamp(2, 1_800_000_000, &mut ts_buf);
    put_duration(3, 3_600, &mut ts_buf);
    let wm = WireMessage::decode(&ts_buf).expect("valid wire");
    assert_eq!(wm.fields.len(), 3);

    // 5. Complete tier_base_plex_cents and tier_base_duration_secs tables
    for (tier, expected_cents) in [
        (0, 2_500),
        (1, 2_500),
        (2, 3_000),
        (3, 3_500),
        (4, 4_000),
        (5, 5_000),
        (6, 6_000),
        (7, 7_500),
        (8, 9_000),
        (9, 11_000),
        (12, 17_000),
    ] {
        assert_eq!(tier_base_plex_cents(tier), expected_cents);
    }
    for (tier, expected_hours) in [
        (0, 3),
        (1, 3),
        (2, 7),
        (3, 12),
        (4, 19),
        (5, 28),
        (6, 40),
        (7, 56),
        (8, 75),
        (9, 97),
        (10, 121),
        (11, 150),
        (12, 184),
        (13, 224),
        (14, 271),
        (15, 325),
        (16, 386),
        (17, 455),
        (18, 532),
        (20, 692),
    ] {
        assert_eq!(tier_base_duration_secs(tier), expected_hours * 3_600);
    }

    // 6. validate_component_in_sde: unknown, mismatch, valid
    let state_arc = SkinrCosmeticsState::load_unregistered(
        Arc::new(eve_db::WorldRepository::new_empty()),
        shared_sde(),
    );
    let mut catalog = state_arc.catalog.clone();
    catalog.components.insert(
        501,
        SdeSkinComponent {
            component_id: 501,
            category: COMP_CAT_MATERIAL,
            rarity: 2,
            disallowed_slots: Vec::new(),
            associated_types: Vec::new(),
            binder_type_id: 0,
            binder_count: 0,
        },
    );
    catalog.components.insert(
        502,
        SdeSkinComponent {
            component_id: 502,
            category: COMP_CAT_PATTERN,
            rarity: 3,
            disallowed_slots: Vec::new(),
            associated_types: Vec::new(),
            binder_type_id: 0,
            binder_count: 0,
        },
    );
    let state = SkinrCosmeticsState {
        db: Arc::clone(&state_arc.db),
        sde: Arc::clone(&state_arc.sde),
        catalog,
        data: std::sync::RwLock::new(SkinrCosmeticsData::default()),
        last_redeemed_gen: std::sync::atomic::AtomicU64::new(0),
    };
    assert_eq!(validate_component_in_sde(&state, 999, false).unwrap_err().0, STATUS_BAD_REQUEST);
    assert_eq!(validate_component_in_sde(&state, 501, true).unwrap_err().0, STATUS_BAD_REQUEST);
    assert_eq!(validate_component_in_sde(&state, 502, false).unwrap_err().0, STATUS_BAD_REQUEST);
    assert_eq!(validate_component_in_sde(&state, 501, false).unwrap().component_id, 501);
    assert_eq!(validate_component_in_sde(&state, 502, true).unwrap().component_id, 502);
}

#[test]
fn skin_attributes_parsers_and_notice_emitters() {
    // 1. parse_license_identifier: dynamic path + raw wire fallback + missing
    let mut lic_sub = DynamicQuasarMessage::new("LicId");
    let mut ch_seq = DynamicQuasarMessage::new("CharSeq");
    ch_seq.set(1, DynamicValue::Uint32(90_001));
    lic_sub.set(1, DynamicValue::Message(Box::new(ch_seq)));
    let mut comp_sub = DynamicQuasarMessage::new("CompId");
    let mut pat_seq = DynamicQuasarMessage::new("PatSeq");
    pat_seq.set(1, DynamicValue::Uint32(77));
    comp_sub.set(2, DynamicValue::Message(Box::new(pat_seq)));
    lic_sub.set(2, DynamicValue::Message(Box::new(comp_sub)));
    let mut outer_dyn = DynamicQuasarMessage::new("Req");
    outer_dyn.set(1, DynamicValue::Message(Box::new(lic_sub)));
    assert_eq!(parse_license_identifier(&outer_dyn, &[]), Some((Some(90_001), 77, true)));

    // Raw wire fallback (empty DynamicQuasarMessage)
    let mut raw_req = Vec::new();
    encode_bytes_field(1, &encode_comp_license_id(90_002, 88, false), &mut raw_req);
    assert_eq!(
        parse_license_identifier(&DynamicQuasarMessage::new("Empty"), &raw_req),
        Some((Some(90_002), 88, false))
    );
    assert_eq!(parse_license_identifier(&DynamicQuasarMessage::new("Empty"), &[]), None);

    // 2. parse_skin_hex: all 5 resolution paths + None
    let empty_dyn = DynamicQuasarMessage::new("Empty");
    // Path 1: sub(dyn_msg, 1).str_of(1)
    let mut s_sub = DynamicQuasarMessage::new("SkinId");
    s_sub.set(1, DynamicValue::String("deadbeef".into()));
    let mut d1 = DynamicQuasarMessage::new("Req");
    d1.set(1, DynamicValue::Message(Box::new(s_sub)));
    assert_eq!(parse_skin_hex(&d1, &[]), Some("deadbeef".into()));

    // Path 2: direct str_of(dyn_msg, 1)
    let mut d2 = DynamicQuasarMessage::new("Req");
    d2.set(1, DynamicValue::String("cafebabe".into()));
    assert_eq!(parse_skin_hex(&d2, &[]), Some("cafebabe".into()));

    // Path 3: numeric sub(dyn_msg, 1).get(1) -> "skin_{id:08x}"
    let mut n_sub = DynamicQuasarMessage::new("SkinNum");
    n_sub.set(1, DynamicValue::Uint64(0x12ab));
    let mut d3 = DynamicQuasarMessage::new("Req");
    d3.set(1, DynamicValue::Message(Box::new(n_sub)));
    assert_eq!(parse_skin_hex(&d3, &[]), Some("skin_000012ab".into()));

    // Path 4a: raw wire nested string
    let mut raw_nested_str = Vec::new();
    encode_bytes_field(1, &encode_skin_id("hex_from_wire"), &mut raw_nested_str);
    assert_eq!(parse_skin_hex(&empty_dyn, &raw_nested_str), Some("hex_from_wire".into()));

    // Path 4b: raw wire nested varint
    let mut inner_var = Vec::new();
    encode_varint_field(1, 0x00ff, &mut inner_var);
    let mut raw_nested_var = Vec::new();
    encode_bytes_field(1, &inner_var, &mut raw_nested_var);
    assert_eq!(parse_skin_hex(&empty_dyn, &raw_nested_var), Some("skin_000000ff".into()));

    // Path 5: raw wire direct string bytes in field 1
    let mut raw_direct_str = Vec::new();
    encode_string_field(1, "direct_hex", &mut raw_direct_str);
    assert_eq!(parse_skin_hex(&empty_dyn, &raw_direct_str), Some("direct_hex".into()));
    assert_eq!(parse_skin_hex(&empty_dyn, &[]), None);

    // 3. parse_skin_license_id: dynamic path & raw wire fallback
    let mut slic = DynamicQuasarMessage::new("SkinLic");
    let mut cseq = DynamicQuasarMessage::new("Char");
    cseq.set(1, DynamicValue::Uint32(90_005));
    slic.set(1, DynamicValue::Message(Box::new(cseq)));
    slic.set(2, DynamicValue::String("direct_skin_hex".into()));
    let mut sreq = DynamicQuasarMessage::new("Req");
    sreq.set(1, DynamicValue::Message(Box::new(slic)));
    assert_eq!(parse_skin_license_id(&sreq, &[]), Some((Some(90_005), "direct_skin_hex".into())));

    let mut raw_slic = Vec::new();
    encode_bytes_field(1, &encode_skin_license_id(90_006, "wire_skin_hex"), &mut raw_slic);
    assert_eq!(parse_skin_license_id(&empty_dyn, &raw_slic), Some((Some(90_006), "wire_skin_hex".into())));
    assert_eq!(parse_skin_license_id(&empty_dyn, &[]), None);

    // 4. encode_skin_attributes (unsequenced None -> field 6 varint; sequenced Some -> field 7 timestamp)
    let mut rec = SkinDesignRecord {
        skin_hex: "abc".into(),
        name: "TestSkin".into(),
        line_name: "Line".into(),
        creator_id: 90_001,
        ship_type_id: 0, // clamped to 1
        pattern_blend_mode: 0, // clamped to 1
        tier_level: 0, // clamped to 1
        slots: vec![
            SkinLayoutSlot {
                slot_id: 1,
                component_id: 101,
                is_pattern: false,
                primary_coating_id: None,
                secondary_coating_id: None,
            },
            SkinLayoutSlot {
                slot_id: 5,
                component_id: 202,
                is_pattern: true,
                primary_coating_id: Some(101),
                secondary_coating_id: Some(102),
            },
        ],
        first_sequenced_unix: None,
    };
    let unseq_bytes = encode_skin_attributes(&rec);
    let wm_unseq = WireMessage::decode(&unseq_bytes).unwrap();
    assert!(wm_unseq.fields.iter().any(|f| f.field_number == 6));
    assert!(!wm_unseq.fields.iter().any(|f| f.field_number == 7));

    rec.first_sequenced_unix = Some(1_800_000_123);
    let seq_bytes = encode_skin_attributes(&rec);
    let wm_seq = WireMessage::decode(&seq_bytes).unwrap();
    assert!(wm_seq.fields.iter().any(|f| f.field_number == 7));
    assert!(!wm_seq.fields.iter().any(|f| f.field_number == 6));

    // 5. Notice emitters queue exact expected type URLs & scopes
    let _ = drain_local_notices();
    let lic = ThirdPartySkinLicense {
        character_id: 90_010,
        skin_hex: "hex10".into(),
        activated: true,
        unactivated: 2,
        escrowed: 1,
        first_acquired_unix: 1_800_000_000,
    };
    let prev = ThirdPartySkinLicense {
        unactivated: 3,
        ..lic.clone()
    };
    queue_component_license_granted_notice(90_010, 44, false, Some(5), 100);
    queue_component_license_granted_notice(90_010, 55, true, None, 101);
    queue_skin_license_added_notice(&lic, 102);
    queue_skin_license_updated_notice(&lic, &prev, 103);
    queue_skin_license_deleted_notice(90_010, "hex10", 104);
    queue_ship_skin_set_notice(90_010, 1_000_001, Some("hex10"), 105);
    queue_ship_skin_set_notice(90_010, 1_000_001, None, 106);

    let notices = drain_local_notices();
    assert_eq!(notices.len(), 7);
    assert!(notices.iter().all(|(s, _, _)| *s == NoticeScope::Character(90_010)));
    let urls: Vec<&str> = notices.iter().map(|(_, a, _)| a.type_url.as_str()).collect();
    assert_eq!(
        urls,
        vec![
            format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GrantedNotice"),
            format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GrantedNotice"),
            format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.license.api.AddedNotice"),
            format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.license.api.UpdatedNotice"),
            format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.license.api.DeletedNotice"),
            format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.api.SetNotice"),
            format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.api.SetNotice"),
        ]
    );
}
