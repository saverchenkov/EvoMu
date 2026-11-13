//! Edge-case and branch-coverage unit tests for `services::quasar_cosmetics_skinr_wire`.

use super::*;
use crate::services::quasar_cosmetics_skinr::{
    SkinrStaticCatalog, COMP_CAT_MATERIAL, COMP_CAT_PATTERN,
};
use eve_db::WorldRepository;
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, EvePublicCharacterId,
    GoogleTimestamp, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn primitive_helpers_and_require_char() {
    let any = any_msg("foo.Bar", vec![1, 2, 3]);
    assert_eq!(any.type_url, format!("{TYPE_GOOGLEAPIS_PREFIX}foo.Bar"));
    assert_eq!(any.value, vec![1, 2, 3]);

    let buf = pb::message("eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.Attributes")
        .set("sequencer.sequential", 42u32)
        .set("started", GoogleTimestamp::from_unix(1_700_000_000, 0))
        .set("planned_completion", GoogleTimestamp::from_unix(1_700_003_600, 0))
        .must_encode();
    let wm = WireMessage::decode(&buf).unwrap();
    assert_eq!(wm.fields.len(), 3);

    let mut ctx = AuthoritativeContext::default();
    assert_eq!(require_char(&ctx).unwrap_err().0, STATUS_FORBIDDEN);
    ctx.active_character = Some(EvePublicCharacterId { sequential: 90_000_001 });
    assert_eq!(require_char(&ctx).unwrap(), 90_000_001);

    assert_eq!(as_u64(&DynamicValue::Uint64(10)), Some(10));
    assert_eq!(as_u64(&DynamicValue::Uint32(11)), Some(11));
    assert_eq!(as_u64(&DynamicValue::Int64(12)), Some(12));
    assert_eq!(as_u64(&DynamicValue::Int64(-1)), None);
    assert_eq!(as_u64(&DynamicValue::Int32(13)), Some(13));
    assert_eq!(as_u64(&DynamicValue::Int32(-2)), None);
    assert_eq!(as_u64(&DynamicValue::Enum(4)), Some(4));
    assert_eq!(as_u64(&DynamicValue::Enum(-1)), None);
    assert_eq!(as_u64(&DynamicValue::String("x".into())), None);

    let mut msg = DynamicQuasarMessage::new("");
    msg.set(1, DynamicValue::String("hello".into()));
    msg.set(2, DynamicValue::Bytes(b"world".to_vec()));
    msg.set(3, DynamicValue::Bytes(vec![0xff, 0xfe]));
    msg.set(4, DynamicValue::Uint64(99));
    assert_eq!(str_of(&msg, 1).as_deref(), Some("hello"));
    assert_eq!(str_of(&msg, 2).as_deref(), Some("world"));
    assert_eq!(str_of(&msg, 3), None);
    assert_eq!(str_of(&msg, 4), None);
    assert_eq!(str_of(&msg, 99), None);
    assert!(sub(&msg, 1).is_none());
    assert!(seq_of(&msg, 1).is_none());
}

#[test]
fn component_id_license_id_and_sde_validation_cover_all_paths() {
    // Component ID encoding & parsing (coating and pattern)
    let coat_wire = encode_component_id(55, false);
    let pat_wire = encode_component_id(77, true);
    assert_eq!(parse_component_id_wire(&coat_wire), Some((55, false)));
    assert_eq!(parse_component_id_wire(&pat_wire), Some((77, true)));
    assert_eq!(parse_component_id_wire(&encode_component_id(0, false)), None);
    assert_eq!(parse_component_id_wire(&[0xff, 0xff]), None);

    let mut coat_inner = DynamicQuasarMessage::new("");
    coat_inner.set(1, DynamicValue::Uint64(55));
    let mut coat_msg = DynamicQuasarMessage::new("");
    coat_msg.set(1, DynamicValue::Message(Box::new(coat_inner)));
    assert_eq!(parse_component_id(&coat_msg), Some((55, false)));

    let mut pat_inner = DynamicQuasarMessage::new("");
    pat_inner.set(1, DynamicValue::Uint64(77));
    let mut pat_msg = DynamicQuasarMessage::new("");
    pat_msg.set(2, DynamicValue::Message(Box::new(pat_inner)));
    assert_eq!(parse_component_id(&pat_msg), Some((77, true)));

    let mut zero_inner = DynamicQuasarMessage::new("");
    zero_inner.set(1, DynamicValue::Uint64(0));
    let mut zero_msg = DynamicQuasarMessage::new("");
    zero_msg.set(1, DynamicValue::Message(Box::new(zero_inner)));
    assert_eq!(parse_component_id(&zero_msg), None);

    // parse_license_identifier via DynamicQuasarMessage and raw wire fallback
    let outer_wire = pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest",
    )
    .set(
        "license",
        build_comp_license_id(
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.Identifier",
            90_000_001,
            77,
            true,
        ),
    )
    .must_encode();
    let empty_dyn = DynamicQuasarMessage::new("");
    assert_eq!(
        parse_license_identifier(&empty_dyn, &outer_wire),
        Some((Some(90_000_001), 77, true))
    );

    let mut char_seq = DynamicQuasarMessage::new("");
    char_seq.set(1, DynamicValue::Uint64(90_000_002));
    let mut lic_dyn_inner = DynamicQuasarMessage::new("");
    lic_dyn_inner.set(1, DynamicValue::Message(Box::new(char_seq)));
    lic_dyn_inner.set(2, DynamicValue::Message(Box::new(coat_msg)));
    let mut lic_dyn = DynamicQuasarMessage::new("");
    lic_dyn.set(1, DynamicValue::Message(Box::new(lic_dyn_inner)));
    assert_eq!(
        parse_license_identifier(&lic_dyn, b""),
        Some((Some(90_000_002), 55, false))
    );
    assert_eq!(parse_license_identifier(&empty_dyn, b""), None);

    // encode_license_kind finite vs infinite + notice emission
    let fin = encode_license_kind(55, false, Some(3));
    let inf = encode_license_kind(77, true, None);
    assert!(!fin.is_empty());
    assert!(!inf.is_empty());
    queue_component_license_granted_notice(90_000_001, 55, false, Some(2), 1_700_000_000);

    // validate_component_in_sde
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let mut cat = SkinrStaticCatalog::default();
    cat.components.insert(
        55,
        SdeSkinComponent {
            component_id: 55,
            category: COMP_CAT_MATERIAL,
            rarity: 1,
            disallowed_slots: Vec::new(),
            associated_types: Vec::new(),
            binder_type_id: 0,
            binder_count: 0,
        },
    );
    cat.components.insert(
        77,
        SdeSkinComponent {
            component_id: 77,
            category: COMP_CAT_PATTERN,
            rarity: 2,
            disallowed_slots: Vec::new(),
            associated_types: Vec::new(),
            binder_type_id: 0,
            binder_count: 0,
        },
    );
    let state = SkinrCosmeticsState {
        db,
        sde: Arc::clone(&sde),
        catalog: cat,
        data: std::sync::RwLock::new(Default::default()),
        last_redeemed_gen: std::sync::atomic::AtomicU64::new(0),
    };
    assert_eq!(validate_component_in_sde(&state, 9999, false).unwrap_err().0, STATUS_BAD_REQUEST);
    assert_eq!(validate_component_in_sde(&state, 55, true).unwrap_err().0, STATUS_BAD_REQUEST);
    assert_eq!(validate_component_in_sde(&state, 55, false).unwrap().component_id, 55);
    assert_eq!(validate_component_in_sde(&state, 77, true).unwrap().component_id, 77);
}

#[test]
fn tier_pricing_duration_skin_encoders_notices_and_parsers() {
    for tier in 0..=12 {
        assert!(tier_base_plex_cents(tier) >= 2500);
    }
    assert_eq!(tier_base_plex_cents(10), 130 * 100);
    for tier in 0..=20 {
        assert!(tier_base_duration_secs(tier) >= 3 * 3_600);
    }
    assert_eq!(tier_base_duration_secs(19), (532 + 80) * 3_600);

    let rec = SkinDesignRecord {
        skin_hex: "deadbeef".to_string(),
        name: "MySkin".to_string(),
        line_name: "Line".to_string(),
        creator_id: 90_000_001,
        ship_type_id: 587,
        pattern_blend_mode: 2,
        slots: vec![
            SkinLayoutSlot {
                slot_id: 1,
                component_id: 10,
                is_pattern: false,
                primary_coating_id: None,
                secondary_coating_id: None,
            },
            SkinLayoutSlot {
                slot_id: 5,
                component_id: 20,
                is_pattern: true,
                primary_coating_id: Some(11),
                secondary_coating_id: Some(12),
            },
        ],
        tier_level: 3,
        first_sequenced_unix: None,
    };
    assert!(!encode_skin_attributes(&rec).is_empty());

    let mut rec_seq = rec.clone();
    rec_seq.first_sequenced_unix = Some(1_700_000_000);
    assert!(!encode_skin_attributes(&rec_seq).is_empty());

    let lic = ThirdPartySkinLicense {
        character_id: 90_000_001,
        skin_hex: "deadbeef".to_string(),
        activated: true,
        unactivated: 2,
        escrowed: 1,
        first_acquired_unix: 1_700_000_000,
    };
    let prev = ThirdPartySkinLicense {
        activated: false,
        unactivated: 3,
        ..lic.clone()
    };
    queue_skin_license_added_notice(&lic, 1_700_000_000);
    queue_skin_license_updated_notice(&lic, &prev, 1_700_000_000);
    queue_skin_license_deleted_notice(90_000_001, "deadbeef", 1_700_000_000);
    queue_ship_skin_set_notice(90_000_001, 100_000_001, Some("deadbeef"), 1_700_000_000);
    queue_ship_skin_set_notice(90_000_001, 100_000_001, None, 1_700_000_000);

    // parse_skin_hex branches:
    // 1. dyn_msg field 1 submessage with string field 1
    let mut sub_str = DynamicQuasarMessage::new("");
    sub_str.set(1, DynamicValue::String("hex1".into()));
    let mut m1 = DynamicQuasarMessage::new("");
    m1.set(1, DynamicValue::Message(Box::new(sub_str)));
    assert_eq!(parse_skin_hex(&m1, b"").as_deref(), Some("hex1"));

    // 2. dyn_msg direct string field 1
    let mut m2 = DynamicQuasarMessage::new("");
    m2.set(1, DynamicValue::String("hex2".into()));
    assert_eq!(parse_skin_hex(&m2, b"").as_deref(), Some("hex2"));

    // 3. dyn_msg submessage with u64 field 1 -> "skin_{id:08x}"
    let mut sub_num = DynamicQuasarMessage::new("");
    sub_num.set(1, DynamicValue::Uint64(0x1a2b));
    let mut m3 = DynamicQuasarMessage::new("");
    m3.set(1, DynamicValue::Message(Box::new(sub_num)));
    assert_eq!(parse_skin_hex(&m3, b"").as_deref(), Some("skin_00001a2b"));

    // 4. raw wire fallback: nested string, nested varint, direct string bytes, empty
    let empty_dyn = DynamicQuasarMessage::new("");
    let wire_nested_str = pb::message("eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest")
        .set("skin.hex", "hex_wire")
        .must_encode();
    assert_eq!(parse_skin_hex(&empty_dyn, &wire_nested_str).as_deref(), Some("hex_wire"));

    let inner_var = pb::message("eve_public.ship.Identifier")
        .set("sequential", 0x00ff_u64)
        .must_encode();
    let wire_nested_var = pb::message("eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest")
        .set_raw_message("skin", inner_var)
        .must_encode();
    assert_eq!(parse_skin_hex(&empty_dyn, &wire_nested_var).as_deref(), Some("skin_000000ff"));

    let wire_direct_str = pb::message("eve_public.cosmetic.ship.skin.thirdparty.Identifier")
        .set("hex", "direct_hex")
        .must_encode();
    assert_eq!(parse_skin_hex(&empty_dyn, &wire_direct_str).as_deref(), Some("direct_hex"));
    assert_eq!(parse_skin_hex(&empty_dyn, b""), None);

    // parse_skin_license_id branches:
    let mut char_seq = DynamicQuasarMessage::new("");
    char_seq.set(1, DynamicValue::Uint64(90_000_001));
    let mut skin_sub = DynamicQuasarMessage::new("");
    skin_sub.set(1, DynamicValue::String("abcd".into()));
    let mut lic_sub = DynamicQuasarMessage::new("");
    lic_sub.set(1, DynamicValue::Message(Box::new(char_seq)));
    lic_sub.set(2, DynamicValue::Message(Box::new(skin_sub)));
    let mut lic_dyn = DynamicQuasarMessage::new("");
    lic_dyn.set(1, DynamicValue::Message(Box::new(lic_sub)));
    assert_eq!(
        parse_skin_license_id(&lic_dyn, b""),
        Some((Some(90_000_001), "abcd".to_string()))
    );

    let wire_slic = pb::message("eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest")
        .set("license", build_skin_license_id("eve_public", 90_000_005, "ef01"))
        .must_encode();
    assert_eq!(
        parse_skin_license_id(&empty_dyn, &wire_slic),
        Some((Some(90_000_005), "ef01".to_string()))
    );
    assert_eq!(parse_skin_license_id(&empty_dyn, b""), None);
}
