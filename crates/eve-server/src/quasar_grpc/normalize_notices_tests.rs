//! Comprehensive unit tests for `quasar_grpc::normalize_notices`.

use super::normalize_notices::{
    normalize_macho_notification, normalize_quasar_any_payload, normalize_quasar_client_response,
};
use eve_proto::quasar::{
    DescriptorProto, DynamicValue, EvePublicResponse, FieldDescriptorProto, FieldLabel, FieldType,
    GoogleAny, QuasarDescriptorRegistry, WireField, WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;

fn field(number: i32, name: &str, field_type: FieldType, type_name: &str) -> FieldDescriptorProto {
    FieldDescriptorProto {
        name: name.into(),
        number: number as u32,
        label: FieldLabel::Optional,
        field_type,
        type_name: type_name.into(),
        oneof_index: None,
        json_name: name.into(),
    }
}

fn msg(full_name: &str, fields: Vec<FieldDescriptorProto>) -> DescriptorProto {
    let name = full_name.rsplit('.').next().unwrap_or(full_name).to_string();
    DescriptorProto {
        name,
        full_name: full_name.into(),
        fields,
        nested_types: Vec::new(),
        enum_types: Vec::new(),
        oneofs: Vec::new(),
        is_map_entry: false,
    }
}

fn build_test_descriptors() -> QuasarDescriptorRegistry {
    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();
    for m in [
        msg(
            "test.VarintPrefer1",
            vec![
                field(2, "second", FieldType::Int64, ""),
                field(1, "first", FieldType::Sint32, ""),
            ],
        ),
        msg(
            "test.Sint64Box",
            vec![field(3, "val", FieldType::Sint64, "")],
        ),
        msg(
            "test.StringBox",
            vec![field(1, "text", FieldType::String, "")],
        ),
        msg(
            "test.BytesBox",
            vec![field(1, "raw", FieldType::Bytes, "")],
        ),
        msg(
            "test.Fixed64Box",
            vec![field(1, "f64", FieldType::Fixed64, "")],
        ),
        msg(
            "test.Fixed32Box",
            vec![field(1, "f32", FieldType::Fixed32, "")],
        ),
        msg(
            "test.MsgWrap",
            vec![field(1, "inner", FieldType::Message, "test.StringBox")],
        ),
        msg(
            "test.DeepCycle",
            vec![field(1, "next", FieldType::Message, "test.DeepCycle")],
        ),
        msg(
            "eve.semanticversion.Specification",
            vec![
                field(1, "major", FieldType::Int32, ""),
                field(2, "minor", FieldType::Int32, ""),
                field(3, "patch", FieldType::Int32, ""),
            ],
        ),
        msg(
            "test.AllFields",
            vec![
                field(1, "m_sub", FieldType::Message, "test.VarintPrefer1"),
                field(2, "s_val", FieldType::String, ""),
                field(3, "b_val", FieldType::Bytes, ""),
                field(4, "v_val", FieldType::Int64, ""),
                field(5, "f64_val", FieldType::Fixed64, ""),
                field(6, "f32_val", FieldType::Fixed32, ""),
                field(7, "m_str", FieldType::Message, "test.StringBox"),
                field(8, "m_bytes", FieldType::Message, "test.BytesBox"),
                field(9, "m_f64", FieldType::Message, "test.Fixed64Box"),
                field(10, "m_f32", FieldType::Message, "test.Fixed32Box"),
                field(11, "m_s64", FieldType::Message, "test.Sint64Box"),
                field(12, "m_wrap", FieldType::Message, "test.MsgWrap"),
                field(13, "m_cycle", FieldType::Message, "test.DeepCycle"),
            ],
        ),
        msg(
            "eve.cosmetic.character.title.definition.api.ListDefinitionsResponse",
            vec![field(1, "title", FieldType::String, "")],
        ),
    ] {
        reg.messages_by_full_name.insert(m.full_name.clone(), m);
    }
    reg
}

#[test]
fn macho_notifications_docking_destiny_channel_bookmarks_and_beans() {
    // OnDockingAccepted: >=3 extracts index 2; <3 passes through
    assert_eq!(
        normalize_macho_notification(
            "OnDockingAccepted",
            vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(60_003_760)],
        ),
        vec![PyRep::Integer(60_003_760)]
    );
    assert_eq!(
        normalize_macho_notification("OnDockingAccepted", vec![PyRep::Integer(1)]),
        vec![PyRep::Integer(1)]
    );

    // DoDestinyUpdate: rewrites nested OnDockingAccepted inside List/Tuple, preserves other entries
    let destiny = normalize_macho_notification(
        "DoDestinyUpdate",
        vec![PyRep::List(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnDockingAccepted".into()),
                PyRep::Tuple(vec![
                    PyRep::Integer(100),
                    PyRep::Integer(200),
                    PyRep::Integer(60_003_760),
                ]),
            ]),
            PyRep::Tuple(vec![
                PyRep::String("OnDockingAccepted".into()),
                PyRep::Tuple(vec![PyRep::Integer(1)]),
            ]),
            PyRep::Tuple(vec![
                PyRep::String("AddBalls2".into()),
                PyRep::Integer(42),
                PyRep::None,
            ]),
            PyRep::Integer(7),
        ])],
    );
    assert_eq!(
        destiny,
        vec![PyRep::List(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnDockingAccepted".into()),
                PyRep::Tuple(vec![PyRep::Integer(60_003_760)]),
            ]),
            PyRep::Tuple(vec![
                PyRep::String("OnDockingAccepted".into()),
                PyRep::Tuple(vec![PyRep::Integer(1)]),
            ]),
            PyRep::Tuple(vec![
                PyRep::String("AddBalls2".into()),
                PyRep::Integer(42),
                PyRep::None,
            ]),
            PyRep::Integer(7),
        ])]
    );

    // OnChannelJoined: empty vs non-empty
    assert!(normalize_macho_notification("OnChannelJoined", vec![]).is_empty());
    assert_eq!(
        normalize_macho_notification(
            "OnChannelJoined",
            vec![PyRep::String("chan".into()), PyRep::Integer(2)],
        ),
        vec![PyRep::String("chan".into())]
    );

    // OnSharedBookmarksFolderUpdated: wraps list/non-tuple element 2, leaves existing tuple untouched
    let existing_tuple = PyRep::Tuple(vec![PyRep::List(vec![]), PyRep::List(vec![])]);
    let bms = normalize_macho_notification(
        "OnSharedBookmarksFolderUpdated",
        vec![PyRep::List(vec![
            PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(2),
                PyRep::List(vec![
                    PyRep::Integer(55),
                    PyRep::keyval([("bookmarkID", PyRep::Integer(66))]),
                ]),
            ]),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::None]),
            PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(2),
                existing_tuple.clone(),
            ]),
        ])],
    );
    let PyRep::List(entries) = &bms[0] else {
        panic!("expected list");
    };
    let PyRep::Tuple(p0) = &entries[0] else {
        panic!("expected tuple");
    };
    let PyRep::Tuple(wrapped0) = &p0[2] else {
        panic!("expected tuple");
    };
    let PyRep::List(bm_list0) = &wrapped0[0] else {
        panic!("expected list");
    };
    assert_eq!(bm_list0[0].dict_get("bookmarkID"), Some(&PyRep::Integer(55)));
    assert_eq!(bm_list0[1].dict_get("bookmarkID"), Some(&PyRep::Integer(66)));
    let PyRep::Tuple(p1) = &entries[1] else {
        panic!("expected tuple");
    };
    assert_eq!(p1[2], existing_tuple);
    let PyRep::Tuple(p2) = &entries[2] else {
        panic!("expected tuple");
    };
    assert_eq!(p2[2], existing_tuple);

    // OnBeanPrime: single list, single scalar, multiple args, Dict/KeyVal/Integer/ignored
    let bp_list = normalize_macho_notification(
        "OnBeanPrime",
        vec![PyRep::List(vec![
            PyRep::Integer(404),
            PyRep::Dict(vec![
                (PyRep::String("errorID".into()), PyRep::Integer(500)),
                (PyRep::Integer(1), PyRep::String("skip".into())),
            ]),
            PyRep::keyval([("logMode", PyRep::Integer(2))]),
            PyRep::None,
        ])],
    );
    let PyRep::List(beans) = &bp_list[0] else {
        panic!("expected list");
    };
    assert_eq!(beans.len(), 3);
    assert_eq!(beans[0].dict_get("errorID"), Some(&PyRep::Integer(404)));
    assert_eq!(beans[1].dict_get("errorID"), Some(&PyRep::Integer(500)));
    assert_eq!(beans[1].dict_get("logMode"), Some(&PyRep::Integer(0)));
    assert_eq!(beans[2].dict_get("errorID"), Some(&PyRep::Integer(0)));
    assert_eq!(beans[2].dict_get("logMode"), Some(&PyRep::Integer(2)));

    let bp_single = normalize_macho_notification("OnBeanPrime", vec![PyRep::Integer(7)]);
    assert_eq!(bp_single.len(), 1);
    let bp_multi = normalize_macho_notification(
        "OnBeanPrime",
        vec![PyRep::Integer(1), PyRep::Integer(2)],
    );
    let PyRep::List(bm) = &bp_multi[0] else {
        panic!("expected list");
    };
    assert_eq!(bm.len(), 2);
}

#[test]
fn macho_notifications_contacts_combat_heat_modules_inventory_and_targets() {
    // OnCorporationRecruitmentAdChanged -> always empty
    assert!(normalize_macho_notification(
        "OnCorporationRecruitmentAdChanged",
        vec![PyRep::Integer(1)],
    )
    .is_empty());

    // OnOrganizationContactsUpdated
    assert!(normalize_macho_notification("OnOrganizationContactsUpdated", vec![]).is_empty());
    let contacts = normalize_macho_notification(
        "OnOrganizationContactsUpdated",
        vec![
            PyRep::Integer(98_000_001),
            PyRep::List(vec![
                PyRep::keyval([
                    ("contactID", PyRep::Integer(90_000_002)),
                    ("standing", PyRep::Real(5.0)),
                    ("labelMask", PyRep::Integer(4)),
                ]),
                PyRep::keyval([
                    ("contactID", PyRep::Integer(90_000_003)),
                    ("relationshipID", PyRep::Real(-10.0)),
                ]),
                PyRep::keyval(Vec::<(&str, PyRep)>::new()),
            ]),
        ],
    );
    assert_eq!(
        contacts,
        vec![
            PyRep::Integer(98_000_001),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(90_000_002), PyRep::Real(5.0), PyRep::Integer(4)]),
                PyRep::Tuple(vec![PyRep::Integer(90_000_003), PyRep::Real(-10.0), PyRep::Integer(0)]),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Real(0.0), PyRep::Integer(0)]),
            ]),
        ]
    );

    // OnTargetClear
    assert_eq!(
        normalize_macho_notification("OnTargetClear", vec![]),
        vec![PyRep::List(vec![])]
    );
    assert_eq!(
        normalize_macho_notification("OnTargetClear", vec![PyRep::Integer(1)]),
        vec![PyRep::Integer(1)]
    );

    // OnGodmaShipEffect: 0, <11, ==11
    assert!(normalize_macho_notification("OnGodmaShipEffect", vec![]).is_empty());
    let eff11: Vec<PyRep> = (0..11).map(PyRep::Integer).collect();
    let eff11_out = normalize_macho_notification("OnGodmaShipEffect", eff11.clone());
    assert_eq!(eff11_out.len(), 11);
    assert!(matches!(&eff11_out[5], PyRep::Tuple(t) if t.len() == 7));
    let eff1 = normalize_macho_notification("OnGodmaShipEffect", vec![PyRep::Integer(555)]);
    assert_eq!(eff1.len(), 11);
    assert_eq!(eff1[0], PyRep::Integer(555));
    assert_eq!(eff1[1], PyRep::Integer(0));
    assert_eq!(eff1[3], PyRep::Integer(0));
    let eff3 = normalize_macho_notification(
        "OnGodmaShipEffect",
        vec![PyRep::Integer(555), PyRep::Integer(16), PyRep::Bool(true)],
    );
    assert_eq!(eff3.len(), 11);
    assert_eq!(eff3[1], PyRep::Integer(16));
    assert_eq!(eff3[3], PyRep::Integer(1));

    // OnHeatAdded / OnHeatRemoved
    assert_eq!(
        normalize_macho_notification("OnHeatAdded", vec![PyRep::Real(0.2)]),
        vec![PyRep::Integer(1211), PyRep::Real(0.2)]
    );
    assert_eq!(
        normalize_macho_notification("OnHeatRemoved", vec![PyRep::Integer(1), PyRep::Real(0.0)]),
        vec![PyRep::Integer(1), PyRep::Real(0.0)]
    );

    // OnModuleAttributeChanges: Tuple(8), Tuple(7), scalar, Tuple(list), multi-args
    let t8 = PyRep::Tuple((0..8).map(PyRep::Integer).collect());
    let t7 = PyRep::Tuple((0..7).map(PyRep::Integer).collect());
    let mod_res = normalize_macho_notification(
        "OnModuleAttributeChanges",
        vec![PyRep::Tuple(vec![t8.clone(), t7, PyRep::Integer(99)])],
    );
    let PyRep::List(mod_items) = &mod_res[0] else {
        panic!("expected list");
    };
    assert_eq!(mod_items[0], t8);
    assert_eq!(
        mod_items[1],
        PyRep::Tuple(
            (0..7)
                .map(PyRep::Integer)
                .chain(std::iter::once(PyRep::None))
                .collect()
        )
    );
    assert_eq!(
        mod_items[2],
        PyRep::Tuple(vec![
            PyRep::String("OnModuleAttributeChange".into()),
            PyRep::None,
            PyRep::Integer(99),
            PyRep::Integer(2),
            PyRep::Integer(0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::None,
        ])
    );
    let mod_single = normalize_macho_notification("OnModuleAttributeChanges", vec![PyRep::Integer(5)]);
    assert!(matches!(&mod_single[0], PyRep::List(v) if v.len() == 1));
    let mod_multi = normalize_macho_notification(
        "OnModuleAttributeChanges",
        vec![PyRep::Integer(5), PyRep::Integer(6)],
    );
    assert!(matches!(&mod_multi[0], PyRep::List(v) if v.len() == 2));

    // OnRemoteMessage
    assert_eq!(
        normalize_macho_notification("OnRemoteMessage", vec![PyRep::String("m".into()), PyRep::Dict(vec![])]),
        vec![PyRep::String("m".into()), PyRep::Dict(vec![]), PyRep::None]
    );
    assert_eq!(
        normalize_macho_notification("OnRemoteMessage", vec![PyRep::String("m".into())]),
        vec![PyRep::String("m".into())]
    );

    // OnItemChange: KeyVal (normalized to blue.DBRow) & Dict
    let ic_kv = normalize_macho_notification(
        "OnItemChange",
        vec![
            PyRep::keyval([
                ("quantity", PyRep::Integer(5)),
                ("locationID", PyRep::Integer(60_003_760)),
            ]),
            PyRep::Dict(vec![]),
        ],
    );
    assert_eq!(ic_kv.len(), 3);
    let PyRep::DBRow { columns, .. } = &ic_kv[0] else {
        panic!("expected DBRow from KeyVal OnItemChange, got {:?}", ic_kv[0]);
    };
    let col = |name: &str| columns.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    assert_eq!(col("quantity"), Some(&PyRep::Integer(5)));
    assert_eq!(col("categoryID"), Some(&PyRep::Integer(0)));
    assert_eq!(col("ownerID"), Some(&PyRep::Integer(0)));
    assert_eq!(col("locationID"), Some(&PyRep::Integer(60_003_760)));
    assert_eq!(ic_kv[1], PyRep::Dict(vec![]));
    assert_eq!(ic_kv[2], PyRep::None);

    let ic_dict = normalize_macho_notification(
        "OnItemChange",
        vec![
            PyRep::Dict(vec![(PyRep::String("categoryID".into()), PyRep::Integer(6))]),
            PyRep::Dict(vec![]),
            PyRep::Integer(123),
        ],
    );
    assert_eq!(ic_dict[0].dict_get("categoryID"), Some(&PyRep::Integer(6)));
    assert_eq!(ic_dict[0].dict_get("stacksize"), None);
    assert_eq!(ic_dict[2], PyRep::Integer(123));

    // ProcessInventoryChange
    let pic = normalize_macho_notification(
        "ProcessInventoryChange",
        vec![PyRep::Integer(100), PyRep::Integer(60_003_760), PyRep::Integer(4)],
    );
    assert_eq!(pic.len(), 4);
    assert_eq!(pic[2], PyRep::Bool(true));
    assert_eq!(pic[3], PyRep::None);
    let pic_passthru = normalize_macho_notification(
        "ProcessInventoryChange",
        vec![PyRep::List(vec![]), PyRep::Integer(1), PyRep::Integer(2)],
    );
    assert_eq!(pic_passthru.len(), 3);

    // OnNotificationReceived
    assert_eq!(
        normalize_macho_notification("OnNotificationReceived", vec![]).len(),
        5
    );
    assert_eq!(
        normalize_macho_notification("OnNotificationReceived", vec![PyRep::Integer(1)]),
        vec![PyRep::Integer(1)]
    );

    // OnTarget & fallback
    for (verb, expected) in [
        ("locking", "locking"),
        ("locked", "add"),
        ("add", "add"),
        ("unlock", "lost"),
        ("unlocked", "lost"),
        ("lost", "lost"),
        ("clear", "clear"),
        ("custom", "custom"),
    ] {
        assert_eq!(
            normalize_macho_notification("OnTarget", vec![PyRep::Integer(777), PyRep::String(verb.into())]),
            vec![PyRep::String(expected.into()), PyRep::Integer(777), PyRep::None]
        );
    }
    assert_eq!(
        normalize_macho_notification("OnTarget", vec![PyRep::String("add".into()), PyRep::Integer(777)]),
        vec![PyRep::String("add".into()), PyRep::Integer(777), PyRep::None]
    );
    assert_eq!(
        normalize_macho_notification("OnTarget", vec![PyRep::Integer(1)]),
        vec![PyRep::Integer(1)]
    );
    assert_eq!(
        normalize_macho_notification("UnknownEvent", vec![PyRep::Integer(9)]),
        vec![PyRep::Integer(9)]
    );
}

#[test]
fn quasar_response_and_any_payload_coercion_covers_wire_variants() {
    let reg = build_test_descriptors();

    // None payload in EvePublicResponse is a no-op
    let mut rsp_none = EvePublicResponse::default();
    normalize_quasar_client_response(&reg, &mut rsp_none);
    assert!(rsp_none.payload.is_none());

    // GetAllResponse type_url rewrite + valid payload preserved
    let valid_title_bytes = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(b"Admiral".to_vec()),
        }],
    }
    .encode();
    let mut rsp_titles = EvePublicResponse {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.cosmetic.character.title.definition.api.GetAllResponse"),
            value: valid_title_bytes.clone(),
        }),
        ..Default::default()
    };
    normalize_quasar_client_response(&reg, &mut rsp_titles);
    let p = rsp_titles.payload.unwrap();
    assert!(p.type_url.ends_with("ListDefinitionsResponse"));
    assert_eq!(p.value, valid_title_bytes);

    let mut any_pub = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.definition.api.GetAllResponse"),
        value: Vec::new(),
    };
    normalize_quasar_any_payload(&reg, &mut any_pub);
    assert!(any_pub.type_url.ends_with("ListDefinitionsResponse"));

    // eve.mail.cspa.SetResponse -> google.protobuf.Empty early return
    let mut any_cspa = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.mail.cspa.SetResponse"),
        value: vec![1, 2, 3],
    };
    normalize_quasar_any_payload(&reg, &mut any_cspa);
    assert_eq!(any_cspa.type_url, format!("{TYPE_GOOGLEAPIS_PREFIX}google.protobuf.Empty"));
    assert_eq!(any_cspa.value, vec![1, 2, 3]);

    // Unknown descriptor leaves payload intact
    let mut any_unknown = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}unknown.DoesNotExist"),
        value: vec![0xff, 0xff],
    };
    normalize_quasar_any_payload(&reg, &mut any_unknown);
    assert_eq!(any_unknown.value, vec![0xff, 0xff]);

    // semanticversion.Specification raw leaf coercion on invalid wire bytes
    let mut any_semver = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.semanticversion.Specification"),
        value: vec![0xff, 0xff, 0xff], // invalid varint wire
    };
    normalize_quasar_any_payload(&reg, &mut any_semver);
    let dec_sv = reg.decode_dynamic("eve.semanticversion.Specification", &any_semver.value).unwrap();
    assert_eq!(dec_sv.fields.get(&1), Some(&DynamicValue::Int32(24)));
    assert_eq!(dec_sv.fields.get(&2), Some(&DynamicValue::Int32(1)));
    assert_eq!(dec_sv.fields.get(&3), Some(&DynamicValue::Int32(0)));

    // StringBox raw leaf coercion: valid UTF-8 vs invalid UTF-8 hex fallback
    let mut any_str_valid = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.StringBox"),
        value: vec![0x80, 0x80], // invalid wire, invalid UTF-8
    };
    normalize_quasar_any_payload(&reg, &mut any_str_valid);
    let dec_str = reg.decode_dynamic("test.StringBox", &any_str_valid.value).unwrap();
    assert_eq!(dec_str.fields.get(&1), Some(&DynamicValue::String("8080".into())));

    let mut any_str_utf8 = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.StringBox"),
        value: b"hi\xff".to_vec(),
    };
    normalize_quasar_any_payload(&reg, &mut any_str_utf8);
    assert!(reg.decode_dynamic("test.StringBox", &any_str_utf8.value).is_ok());

    // MsgWrap raw leaf coercion (recurses into nested StringBox) + BytesBox raw leaf coercion
    let mut any_wrap = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.MsgWrap"),
        value: vec![0x80, 0x81],
    };
    normalize_quasar_any_payload(&reg, &mut any_wrap);
    assert!(reg.decode_dynamic("test.MsgWrap", &any_wrap.value).is_ok());

    let mut any_bytes = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.BytesBox"),
        value: vec![0x80, 0x81],
    };
    normalize_quasar_any_payload(&reg, &mut any_bytes);
    assert!(reg.decode_dynamic("test.BytesBox", &any_bytes.value).is_ok());

    // Unknown wire field number falls back to coerce_raw_leaf_bytes_to_message
    let unknown_only = WireMessage {
        fields: vec![WireField {
            field_number: 99,
            value: WireValue::Varint(123),
        }],
    }
    .encode();
    let mut any_unknown_field = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.BytesBox"),
        value: unknown_only,
    };
    normalize_quasar_any_payload(&reg, &mut any_unknown_field);
    assert!(reg.decode_dynamic("test.BytesBox", &any_unknown_field.value).is_ok());

    // Wire-type mismatch coercion across all field types in test.AllFields
    let sub_varint = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::Varint(77),
        }],
    }
    .encode();
    let bad_sub = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(vec![0xfe, 0xff]),
        }],
    }
    .encode();
    let mismatched = WireMessage {
        fields: vec![
            WireField { field_number: 999, value: WireValue::Varint(1) }, // unknown field skipped
            WireField { field_number: 1, value: WireValue::Varint(5) },   // Message <- Varint (VarintPrefer1 picks #1 Sint32)
            WireField { field_number: 2, value: WireValue::Varint(42) },  // String <- Varint ("42")
            WireField { field_number: 3, value: WireValue::Varint(7) },   // Bytes <- Varint (7u64.to_le_bytes())
            WireField { field_number: 4, value: WireValue::LengthDelimited(sub_varint) }, // Int64 <- LengthDelimited(sub_varint)
            WireField { field_number: 5, value: WireValue::Varint(88) },  // Fixed64 <- Varint
            WireField { field_number: 6, value: WireValue::Varint(99) },  // Fixed32 <- Varint
            WireField { field_number: 7, value: WireValue::Varint(11) },  // Msg(StringBox) <- Varint
            WireField { field_number: 8, value: WireValue::Varint(22) },  // Msg(BytesBox) <- Varint
            WireField { field_number: 9, value: WireValue::Varint(33) },  // Msg(Fixed64Box) <- Varint
            WireField { field_number: 10, value: WireValue::Varint(44) }, // Msg(Fixed32Box) <- Varint
            WireField { field_number: 11, value: WireValue::Varint(55) }, // Msg(Sint64Box) <- Varint
            WireField { field_number: 12, value: WireValue::Varint(66) }, // Msg(MsgWrap) <- Varint
            // DeepCycle (>8 levels) varint triggers depth > 8 guard -> empty submessage
            WireField { field_number: 13, value: WireValue::Varint(1) },
            // Nested StringBox inside MsgWrap with invalid UTF-8 bytes
            WireField { field_number: 12, value: WireValue::LengthDelimited(bad_sub) },
        ],
    }
    .encode();

    let mut any_all = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.AllFields"),
        value: mismatched,
    };
    normalize_quasar_any_payload(&reg, &mut any_all);
    let dec_all = reg.decode_dynamic("test.AllFields", &any_all.value).unwrap();
    assert_eq!(dec_all.fields.get(&2), Some(&DynamicValue::String("42".into())));
    assert_eq!(dec_all.fields.get(&4), Some(&DynamicValue::Int64(77)));

    // Also test Fixed64/Fixed32 -> Varint field coercion
    let fixed_to_varint = WireMessage {
        fields: vec![WireField {
            field_number: 4,
            value: WireValue::Fixed64(1234),
        }],
    }
    .encode();
    let mut any_f64 = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.AllFields"),
        value: fixed_to_varint,
    };
    normalize_quasar_any_payload(&reg, &mut any_f64);
    let dec_f64 = reg.decode_dynamic("test.AllFields", &any_f64.value).unwrap();
    assert_eq!(dec_f64.fields.get(&4), Some(&DynamicValue::Int64(1234)));

    let fixed32_to_varint = WireMessage {
        fields: vec![WireField {
            field_number: 4,
            value: WireValue::Fixed32(4321),
        }],
    }
    .encode();
    let mut any_f32 = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.AllFields"),
        value: fixed32_to_varint,
    };
    normalize_quasar_any_payload(&reg, &mut any_f32);
    let dec_f32 = reg.decode_dynamic("test.AllFields", &any_f32.value).unwrap();
    assert_eq!(dec_f32.fields.get(&4), Some(&DynamicValue::Int64(4321)));
}
