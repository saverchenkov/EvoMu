//! Edge-case, corrupt-wire, synthetic-DB manifest verification, and boundary unit tests
//! for `eve_proto::quasar::registry`.

use super::*;
use crate::MAX_RECURSION_DEPTH;

#[test]
fn registry_open_error_paths_and_synthetic_full_manifest_verification() {
    let base = std::env::temp_dir().join(format!("evomu_quasar_reg_edges_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let db_path = base.join("descs.sqlite");
    let manifest_path = base.join("manifest.json");

    // 1. Missing manifest file when sqlite exists
    {
        let _conn = Connection::open(&db_path).unwrap();
    }
    let err_missing_manifest = QuasarDescriptorRegistry::open(&db_path, &manifest_path).unwrap_err();
    assert!(matches!(err_missing_manifest, QuasarError::ManifestMismatch(_)));

    // 2. Invalid JSON in manifest
    std::fs::write(&manifest_path, "{not-valid-json").unwrap();
    let err_bad_json = QuasarDescriptorRegistry::open(&db_path, &manifest_path).unwrap_err();
    assert!(matches!(err_bad_json, QuasarError::ManifestMismatch(_)));

    // Build a valid minimal manifest template
    let base_manifest = QuasarManifest {
        schema_version: 1,
        domain_count: EXPECTED_QUASAR_DOMAIN_COUNT,
        descriptor_file_count: EXPECTED_QUASAR_FILE_COUNT,
        unique_package_count: 1,
        top_level_message_count: 1,
        total_message_count: 1,
        total_enum_count: 0,
        total_service_count: 0,
        total_field_count: 0,
        total_serialized_pb_bytes: 0,
        combined_serialized_pb_sha256: String::new(),
        combined_serialized_pb_md5: String::new(),
        per_domain_file_counts: BTreeMap::new(),
    };
    std::fs::write(&manifest_path, serde_json::to_string(&base_manifest).unwrap()).unwrap();

    // 3. Empty SQLite DB missing `quasar_file_descriptors` table -> DatabaseError
    let err_no_table = QuasarDescriptorRegistry::open(&db_path, &manifest_path).unwrap_err();
    assert!(matches!(err_no_table, QuasarError::DatabaseError(_)));

    // 4. Wrong column type (NULL in pyj_path) -> DatabaseError
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE quasar_file_descriptors (
                pyj_path TEXT,
                domain TEXT,
                sha256 TEXT,
                md5 TEXT,
                serialized_pb BLOB
            );
            INSERT INTO quasar_file_descriptors VALUES (NULL, 'd', '', '', X'');",
        )
        .unwrap();
    }
    let err_null_col = QuasarDescriptorRegistry::open(&db_path, &manifest_path).unwrap_err();
    assert!(matches!(err_null_col, QuasarError::DatabaseError(_)));

    // 5. Corrupt serialized_pb wire bytes -> UnexpectedEof
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("DELETE FROM quasar_file_descriptors", []).unwrap();
        conn.execute(
            "INSERT INTO quasar_file_descriptors VALUES ('a.pyj', 'd0', '0', '0', ?1)",
            rusqlite::params![vec![0x0Au8, 0x10, 0x01]],
        )
        .unwrap();
    }
    assert_eq!(
        QuasarDescriptorRegistry::open(&db_path, &manifest_path).unwrap_err(),
        QuasarError::UnexpectedEof
    );

    // 6. Checksum mismatch on a single valid FileDescriptorProto row
    let sample_fd = FileDescriptorProto {
        pyj_path: "d0/f0.pyj".into(),
        domain: "d0".into(),
        name: "d0/f0.proto".into(),
        package: "eve_public.d0".into(),
        syntax: "proto3".into(),
        dependencies: vec![],
        messages: vec![DescriptorProto {
            name: "Outer".into(),
            full_name: "eve_public.d0.Outer".into(),
            fields: vec![],
            nested_types: vec![DescriptorProto {
                name: "Inner".into(),
                full_name: "eve_public.d0.Outer.Inner".into(),
                fields: vec![],
                nested_types: vec![],
                enum_types: vec![],
                oneofs: vec![],
                is_map_entry: false,
            }],
            enum_types: vec![EnumDescriptorProto {
                name: "Kind".into(),
                full_name: "eve_public.d0.Outer.Kind".into(),
                values: vec![EnumValueDescriptorProto {
                    name: "KIND_UNSPECIFIED".into(),
                    number: 0,
                }],
            }],
            oneofs: vec![],
            is_map_entry: false,
        }],
        enums: vec![EnumDescriptorProto {
            name: "TopE".into(),
            full_name: "eve_public.d0.TopE".into(),
            values: vec![],
        }],
        services: vec![ServiceDescriptorProto {
            name: "TopS".into(),
            full_name: "eve_public.d0.TopS".into(),
            methods: vec![],
        }],
        sha256: String::new(),
        md5: String::new(),
        raw_bytes: Vec::new(),
    };
    let sample_wire = sample_fd.encode_wire();
    let sample_sha = sha256_hex(&sample_wire);
    let sample_md5 = md5_hex(&sample_wire);

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("DELETE FROM quasar_file_descriptors", []).unwrap();
        conn.execute(
            "INSERT INTO quasar_file_descriptors VALUES ('d0/f0.pyj', 'd0', 'bad_sha', ?1, ?2)",
            rusqlite::params![&sample_md5, &sample_wire],
        )
        .unwrap();
    }
    let err_csum = QuasarDescriptorRegistry::open(&db_path, &manifest_path).unwrap_err();
    assert!(matches!(err_csum, QuasarError::ManifestMismatch(ref m) if m.contains("checksum")));

    // 7. Valid single row but count != EXPECTED_QUASAR_FILE_COUNT
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("DELETE FROM quasar_file_descriptors", []).unwrap();
        conn.execute(
            "INSERT INTO quasar_file_descriptors VALUES ('d0/f0.pyj', 'd0', ?1, ?2, ?3)",
            rusqlite::params![&sample_sha, &sample_md5, &sample_wire],
        )
        .unwrap();
    }
    let err_cnt = QuasarDescriptorRegistry::open(&db_path, &manifest_path).unwrap_err();
    assert!(matches!(err_cnt, QuasarError::ManifestMismatch(ref m) if m.contains("Expected 1455")));

    // 8. Synthetic 1455-file / 135-domain SQLite DB to test domain-mismatch, combined sha/md5 mismatch, and full Ok load!
    let mut rows_meta: Vec<(String, String, String, String, Vec<u8>)> =
        Vec::with_capacity(EXPECTED_QUASAR_FILE_COUNT);
    let mut per_domain_counts: BTreeMap<String, usize> = BTreeMap::new();
    for i in 0..EXPECTED_QUASAR_FILE_COUNT {
        let dom_idx = i % EXPECTED_QUASAR_DOMAIN_COUNT;
        let domain = format!("dom_{dom_idx:03}");
        let pyj = format!("{domain}/file_{i:04}.pyj");
        let (wire, sha, md5) = if i == 0 {
            let mut f = sample_fd.clone();
            f.pyj_path = pyj.clone();
            f.domain = domain.clone();
            let w = f.encode_wire();
            (w.clone(), sha256_hex(&w), md5_hex(&w))
        } else {
            let mut w = Vec::new();
            encode_string_field(1, &format!("{domain}/file_{i:04}.proto"), &mut w);
            encode_string_field(2, &format!("eve_public.{domain}"), &mut w);
            (w.clone(), sha256_hex(&w), md5_hex(&w))
        };
        *per_domain_counts.entry(domain.clone()).or_insert(0) += 1;
        rows_meta.push((pyj, domain, sha, md5, wire));
    }
    rows_meta.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

    let mut combined = Vec::new();
    for (pyj, _, _, _, wire) in &rows_meta {
        let pb = pyj.as_bytes();
        combined.extend_from_slice(&(pb.len() as u32).to_le_bytes());
        combined.extend_from_slice(pb);
        combined.extend_from_slice(&(wire.len() as u32).to_le_bytes());
        combined.extend_from_slice(wire);
    }
    let combined_sha = sha256_hex(&combined);
    let combined_md5 = md5_hex(&combined);

    {
        let _ = std::fs::remove_file(&db_path);
        let mut conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE quasar_file_descriptors (
                pyj_path TEXT PRIMARY KEY,
                domain TEXT NOT NULL,
                sha256 TEXT NOT NULL,
                md5 TEXT NOT NULL,
                serialized_pb BLOB NOT NULL
            );",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let mut stmt = tx
            .prepare("INSERT INTO quasar_file_descriptors VALUES (?1, ?2, ?3, ?4, ?5)")
            .unwrap();
        for (pyj, dom, sha, md5, wire) in &rows_meta {
            stmt.execute(rusqlite::params![pyj, dom, sha, md5, wire])
                .unwrap();
        }
        drop(stmt);
        tx.commit().unwrap();
    }

    // 8a. Domain counts mismatch
    let mut bad_dom_manifest = base_manifest.clone();
    bad_dom_manifest.per_domain_file_counts = per_domain_counts.clone();
    *bad_dom_manifest
        .per_domain_file_counts
        .get_mut("dom_000")
        .unwrap() += 1;
    bad_dom_manifest.combined_serialized_pb_sha256 = combined_sha.clone();
    bad_dom_manifest.combined_serialized_pb_md5 = combined_md5.clone();
    std::fs::write(
        &manifest_path,
        serde_json::to_string(&bad_dom_manifest).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        QuasarDescriptorRegistry::open(&db_path, &manifest_path),
        Err(QuasarError::ManifestMismatch(ref m)) if m.contains("Domain counts mismatch")
    ));

    // 8b. Combined SHA-256 mismatch
    let mut bad_sha_manifest = bad_dom_manifest.clone();
    bad_sha_manifest.per_domain_file_counts = per_domain_counts.clone();
    bad_sha_manifest.combined_serialized_pb_sha256 = "0".repeat(64);
    std::fs::write(
        &manifest_path,
        serde_json::to_string(&bad_sha_manifest).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        QuasarDescriptorRegistry::open(&db_path, &manifest_path),
        Err(QuasarError::ManifestMismatch(ref m)) if m.contains("Combined SHA-256 mismatch")
    ));

    // 8c. Combined MD5 mismatch
    let mut bad_md5_manifest = bad_sha_manifest.clone();
    bad_md5_manifest.combined_serialized_pb_sha256 = combined_sha.clone();
    bad_md5_manifest.combined_serialized_pb_md5 = "0".repeat(32);
    std::fs::write(
        &manifest_path,
        serde_json::to_string(&bad_md5_manifest).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        QuasarDescriptorRegistry::open(&db_path, &manifest_path),
        Err(QuasarError::ManifestMismatch(ref m)) if m.contains("Combined MD5 mismatch")
    ));

    // 8d. Valid full open
    let mut good_manifest = bad_md5_manifest;
    good_manifest.combined_serialized_pb_md5 = combined_md5;
    std::fs::write(&manifest_path, serde_json::to_string(&good_manifest).unwrap()).unwrap();
    let reg = QuasarDescriptorRegistry::open(&db_path, &manifest_path).unwrap();
    assert_eq!(reg.files.len(), EXPECTED_QUASAR_FILE_COUNT);
    assert!(reg.get_file_by_pyj("dom_000/file_0000.pyj").is_some());
    assert!(reg.get_file_by_pyj("missing.pyj").is_none());
    assert!(reg.get_message("eve_public.d0.Outer").is_some());
    assert!(reg.get_message("eve_public.d0.Outer.Inner").is_some());
    assert!(reg.enums_by_full_name.contains_key("eve_public.d0.Outer.Kind"));
    assert!(reg.enums_by_full_name.contains_key("eve_public.d0.TopE"));
    assert!(reg.services_by_full_name.contains_key("eve_public.d0.TopS"));

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn instantiate_sample_depth_cutoff_enums_groups_and_unknown_refs() {
    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();

    // Enum with only value 0, and enum with empty values
    reg.enums_by_full_name.insert(
        "test.ZeroOnlyEnum".into(),
        EnumDescriptorProto {
            name: "ZeroOnlyEnum".into(),
            full_name: "test.ZeroOnlyEnum".into(),
            values: vec![EnumValueDescriptorProto {
                name: "ONLY_ZERO".into(),
                number: 0,
            }],
        },
    );
    reg.enums_by_full_name.insert(
        "test.EmptyEnum".into(),
        EnumDescriptorProto {
            name: "EmptyEnum".into(),
            full_name: "test.EmptyEnum".into(),
            values: vec![],
        },
    );

    // Non-cyclic chain of 9 messages (Chain0 -> ... -> Chain8) to hit `depth > 6` cutoff
    for i in 0..=8usize {
        let mut fields = Vec::new();
        if i < 8 {
            fields.push(FieldDescriptorProto {
                name: "next".into(),
                number: 1,
                label: FieldLabel::Optional,
                field_type: if i == 0 {
                    FieldType::Group
                } else {
                    FieldType::Message
                },
                type_name: format!("test.Chain{}", i + 1),
                oneof_index: None,
                json_name: "next".into(),
            });
        } else {
            fields.push(FieldDescriptorProto {
                name: "leaf_val".into(),
                number: 1,
                label: FieldLabel::Optional,
                field_type: FieldType::Int32,
                type_name: String::new(),
                oneof_index: None,
                json_name: "leafVal".into(),
            });
        }
        reg.insert_message_for_test(DescriptorProto {
            name: format!("Chain{i}"),
            full_name: format!("test.Chain{i}"),
            fields,
            nested_types: vec![],
            enum_types: vec![],
            oneofs: vec![],
            is_map_entry: false,
        });
    }

    let c0 = reg.instantiate_populated_sample("test.Chain0", 10).unwrap();
    // Walk 7 levels deep -> level 7 (depth=7 > 6) has empty fields
    let mut cur = &c0;
    for _ in 0..7 {
        let Some(DynamicValue::Message(next)) = cur.get(1) else {
            panic!("expected nested message");
        };
        cur = next;
    }
    assert!(cur.fields.is_empty(), "depth > 6 must return empty message");

    // Enum fallbacks: missing enum -> 1, zero-only -> 0, empty -> 1
    reg.insert_message_for_test(DescriptorProto {
        name: "EnumVariants".into(),
        full_name: "test.EnumVariants".into(),
        fields: vec![
            FieldDescriptorProto {
                name: "missing_enum".into(),
                number: 1,
                label: FieldLabel::Optional,
                field_type: FieldType::Enum,
                type_name: "test.DoesNotExist".into(),
                oneof_index: None,
                json_name: "missingEnum".into(),
            },
            FieldDescriptorProto {
                name: "zero_enum".into(),
                number: 2,
                label: FieldLabel::Optional,
                field_type: FieldType::Enum,
                type_name: "test.ZeroOnlyEnum".into(),
                oneof_index: None,
                json_name: "zeroEnum".into(),
            },
            FieldDescriptorProto {
                name: "empty_enum".into(),
                number: 3,
                label: FieldLabel::Optional,
                field_type: FieldType::Enum,
                type_name: "test.EmptyEnum".into(),
                oneof_index: None,
                json_name: "emptyEnum".into(),
            },
        ],
        nested_types: vec![],
        enum_types: vec![],
        oneofs: vec![],
        is_map_entry: false,
    });
    let ev_msg = reg
        .instantiate_populated_sample("test.EnumVariants", 5)
        .unwrap();
    assert_eq!(ev_msg.get(1), Some(&DynamicValue::Enum(1)));
    assert_eq!(ev_msg.get(2), Some(&DynamicValue::Enum(0)));
    assert_eq!(ev_msg.get(3), Some(&DynamicValue::Enum(1)));

    // Missing child message type fails with UnknownMessageType
    reg.insert_message_for_test(DescriptorProto {
        name: "BrokenChild".into(),
        full_name: "test.BrokenChild".into(),
        fields: vec![FieldDescriptorProto {
            name: "bad".into(),
            number: 1,
            label: FieldLabel::Optional,
            field_type: FieldType::Message,
            type_name: "test.MissingChild".into(),
            oneof_index: None,
            json_name: "bad".into(),
        }],
        nested_types: vec![],
        enum_types: vec![],
        oneofs: vec![],
        is_map_entry: false,
    });
    assert_eq!(
        reg.instantiate_populated_sample("test.BrokenChild", 1),
        Err(QuasarError::UnknownMessageType("test.MissingChild".into()))
    );
}

#[test]
fn dynamic_codec_packed_unpacked_fallback_errors_and_rust_codegen_all_types() {
    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();

    reg.insert_message_for_test(DescriptorProto {
        name: "Child".into(),
        full_name: "test.Child".into(),
        fields: vec![FieldDescriptorProto {
            name: "v".into(),
            number: 1,
            label: FieldLabel::Optional,
            field_type: FieldType::Int32,
            type_name: String::new(),
            oneof_index: None,
            json_name: "v".into(),
        }],
        nested_types: vec![],
        enum_types: vec![],
        oneofs: vec![],
        is_map_entry: false,
    });

    let all_fields = vec![
        (1, "f_double", FieldType::Double, FieldLabel::Repeated, ""),
        (2, "f_float", FieldType::Float, FieldLabel::Repeated, ""),
        (3, "f_int64", FieldType::Int64, FieldLabel::Repeated, ""),
        (4, "f_uint64", FieldType::Uint64, FieldLabel::Repeated, ""),
        (5, "f_int32", FieldType::Int32, FieldLabel::Repeated, ""),
        (6, "f_fixed64", FieldType::Fixed64, FieldLabel::Repeated, ""),
        (7, "f_fixed32", FieldType::Fixed32, FieldLabel::Repeated, ""),
        (8, "f_bool", FieldType::Bool, FieldLabel::Repeated, ""),
        (9, "f_string", FieldType::String, FieldLabel::Repeated, ""),
        (10, "f_group", FieldType::Group, FieldLabel::Optional, "test.Child"),
        (11, "f_msg", FieldType::Message, FieldLabel::Repeated, "test.Child"),
        (12, "f_bytes", FieldType::Bytes, FieldLabel::Repeated, ""),
        (13, "f_uint32", FieldType::Uint32, FieldLabel::Repeated, ""),
        (14, "f_enum", FieldType::Enum, FieldLabel::Repeated, ""),
        (15, "f_sfixed32", FieldType::Sfixed32, FieldLabel::Repeated, ""),
        (16, "f_sfixed64", FieldType::Sfixed64, FieldLabel::Repeated, ""),
        (17, "f_sint32", FieldType::Sint32, FieldLabel::Repeated, ""),
        (18, "f_sint64", FieldType::Sint64, FieldLabel::Repeated, ""),
        (19, "s_str", FieldType::String, FieldLabel::Optional, ""),
        (20, "s_bytes", FieldType::Bytes, FieldLabel::Optional, ""),
        (21, "s_missing", FieldType::Message, FieldLabel::Optional, "test.NoSuchChild"),
    ];
    let mut desc_fields = Vec::new();
    for (num, name, ft, lbl, tn) in &all_fields {
        desc_fields.push(FieldDescriptorProto {
            name: (*name).into(),
            number: *num,
            label: *lbl,
            field_type: *ft,
            type_name: (*tn).into(),
            oneof_index: None,
            json_name: (*name).into(),
        });
    }
    reg.insert_message_for_test(DescriptorProto {
        name: "FullCov".into(),
        full_name: "test.FullCov".into(),
        fields: desc_fields,
        nested_types: vec![],
        enum_types: vec![],
        oneofs: vec![],
        is_map_entry: false,
    });

    // Encode unknown message type (desc == None -> unpacked Repeated)
    let mut unknown_msg = DynamicQuasarMessage::new("test.UnknownForEncode");
    unknown_msg.set(
        1,
        DynamicValue::Repeated(vec![DynamicValue::Uint64(1), DynamicValue::Uint64(2)]),
    );
    let unk_wire = reg.encode_dynamic(&unknown_msg).unwrap();
    let unk_dec = reg
        .decode_dynamic_or_wire("test.UnknownForEncode", &unk_wire)
        .unwrap();
    assert_eq!(
        unk_dec.get(1),
        Some(&DynamicValue::Repeated(vec![
            DynamicValue::Uint64(1),
            DynamicValue::Uint64(2)
        ]))
    );

    // Encode FullCov with empty Repeated, mismatched item in packed Repeated (_ => {}), Group, Repeated Message/String
    let mut child = DynamicQuasarMessage::new("test.Child");
    child.set(1, DynamicValue::Int32(7));
    let mut full = DynamicQuasarMessage::new("test.FullCov");
    full.set(1, DynamicValue::Repeated(vec![])); // empty repeated
    full.set(
        5,
        DynamicValue::Repeated(vec![
            DynamicValue::Int32(-1),
            DynamicValue::String("ignored_in_packed".into()),
        ]),
    );
    full.set(10, DynamicValue::Message(Box::new(child.clone())));
    full.set(
        11,
        DynamicValue::Repeated(vec![
            DynamicValue::Message(Box::new(child.clone())),
            DynamicValue::Message(Box::new(child)),
        ]),
    );
    full.set(
        9,
        DynamicValue::Repeated(vec![
            DynamicValue::String("a".into()),
            DynamicValue::String("b".into()),
        ]),
    );
    full.set(20, DynamicValue::Bytes(vec![1, 2, 3]));
    let full_wire = reg.encode_dynamic(&full).unwrap();
    let full_dec = reg.decode_dynamic("test.FullCov", &full_wire).unwrap();
    assert_eq!(full_dec.get(5), Some(&DynamicValue::Repeated(vec![DynamicValue::Int32(-1)])));
    assert!(matches!(full_dec.get(10), Some(DynamicValue::Message(_))));
    assert!(matches!(full_dec.get(11), Some(DynamicValue::Repeated(v)) if v.len() == 2));
    assert!(matches!(full_dec.get(9), Some(DynamicValue::Repeated(v)) if v.len() == 2));

    // Unknown field number on wire is ignored cleanly
    let mut with_unknown_fnum = full_wire.clone();
    encode_varint_field(999, 123, &mut with_unknown_fnum);
    assert!(reg.decode_dynamic("test.FullCov", &with_unknown_fnum).is_ok());

    // Truncated packed varint for all varint-packable types (3,4,5,8,13,14,17,18)
    for fnum in [3u32, 4, 5, 8, 13, 14, 17, 18] {
        let mut bad_packed = Vec::new();
        encode_bytes_field(fnum, &[0x80], &mut bad_packed);
        assert_eq!(
            reg.decode_dynamic("test.FullCov", &bad_packed),
            Err(QuasarError::UnexpectedEof)
        );
    }

    // Invalid UTF-8 in String field -> InvalidUtf8
    let mut bad_str = Vec::new();
    encode_bytes_field(19, &[0xFF, 0xFE], &mut bad_str);
    assert!(matches!(
        reg.decode_dynamic("test.FullCov", &bad_str),
        Err(QuasarError::InvalidUtf8(_))
    ));

    // Missing child message descriptor on decode -> UnknownMessageType
    let mut bad_child = Vec::new();
    encode_bytes_field(21, &[], &mut bad_child);
    assert_eq!(
        reg.decode_dynamic("test.FullCov", &bad_child),
        Err(QuasarError::UnknownMessageType("test.NoSuchChild".into()))
    );

    // RecursionLimitExceeded in decode_dynamic
    let mut self_rec_bytes = Vec::new();
    reg.insert_message_for_test(DescriptorProto {
        name: "SelfNode".into(),
        full_name: "test.SelfNode".into(),
        fields: vec![FieldDescriptorProto {
            name: "child".into(),
            number: 1,
            label: FieldLabel::Optional,
            field_type: FieldType::Message,
            type_name: "test.SelfNode".into(),
            oneof_index: None,
            json_name: "child".into(),
        }],
        nested_types: vec![],
        enum_types: vec![],
        oneofs: vec![],
        is_map_entry: false,
    });
    for _ in 0..=(MAX_RECURSION_DEPTH + 1) {
        let mut next = Vec::new();
        encode_bytes_field(1, &self_rec_bytes, &mut next);
        self_rec_bytes = next;
    }
    assert_eq!(
        reg.decode_dynamic("test.SelfNode", &self_rec_bytes),
        Err(QuasarError::RecursionLimitExceeded)
    );

    // decode_wire_fallback: control-char UTF-8 that is valid wire msg vs invalid wire msg,
    // and non-UTF-8 that is valid wire msg vs invalid wire msg, plus recursion limit.
    let mut ctrl_valid_wire = Vec::new();
    let mut inner_valid = Vec::new();
    encode_varint_field(1, 5, &mut inner_valid); // [0x08, 0x05] -> 0x05 is ASCII control!
    encode_bytes_field(1, &inner_valid, &mut ctrl_valid_wire);

    // UTF-8 with control char 0x01 that is NOT valid wire (field 0: [0x00, 0x01])
    encode_bytes_field(2, &[0x00, 0x01], &mut ctrl_valid_wire);
    // Non-UTF-8 that IS valid wire (field 1 bytes [0xFF])
    let mut non_utf8_valid_wire = Vec::new();
    encode_bytes_field(1, &[0xFF], &mut non_utf8_valid_wire);
    encode_bytes_field(3, &non_utf8_valid_wire, &mut ctrl_valid_wire);
    // Non-UTF-8 that is NOT valid wire
    encode_bytes_field(4, &[0xFF, 0x00], &mut ctrl_valid_wire);

    let fb = reg
        .decode_dynamic_or_wire("test.NotInReg", &ctrl_valid_wire)
        .unwrap();
    assert!(matches!(fb.get(1), Some(DynamicValue::Message(_))));
    assert_eq!(fb.get(2), Some(&DynamicValue::Bytes(vec![0x00, 0x01])));
    assert!(matches!(fb.get(3), Some(DynamicValue::Message(_))));
    assert_eq!(fb.get(4), Some(&DynamicValue::Bytes(vec![0xFF, 0x00])));

    let fb_deep = reg
        .decode_dynamic_or_wire("test.NotInReg", &self_rec_bytes)
        .unwrap();
    assert!(matches!(fb_deep.get(1), Some(DynamicValue::Message(_))));

    // Rust codegen covering ALL 18 FieldTypes and ALL 9 reserved keywords
    let keywords = ["type", "match", "loop", "in", "ref", "mod", "self", "super", "crate"];
    let mut codegen_fields = Vec::new();
    let ft_list = [
        FieldType::Double,
        FieldType::Float,
        FieldType::Int64,
        FieldType::Uint64,
        FieldType::Int32,
        FieldType::Fixed64,
        FieldType::Fixed32,
        FieldType::Bool,
        FieldType::String,
        FieldType::Group,
        FieldType::Message,
        FieldType::Bytes,
        FieldType::Uint32,
        FieldType::Enum,
        FieldType::Sfixed32,
        FieldType::Sfixed64,
        FieldType::Sint32,
        FieldType::Sint64,
    ];
    for (idx, ft) in ft_list.iter().enumerate() {
        let name = if idx < keywords.len() {
            keywords[idx].to_string()
        } else {
            format!("f_{idx}")
        };
        codegen_fields.push(FieldDescriptorProto {
            name,
            number: (idx + 1) as u32,
            label: if *ft == FieldType::Group {
                FieldLabel::Repeated
            } else {
                FieldLabel::Optional
            },
            field_type: *ft,
            type_name: "pkg.SubItem".into(),
            oneof_index: None,
            json_name: String::new(),
        });
    }
    let cg_file = FileDescriptorProto {
        pyj_path: "cg.pyj".into(),
        domain: "cg".into(),
        name: "cg.proto".into(),
        package: "pkg".into(),
        syntax: "proto3".into(),
        dependencies: vec![],
        messages: vec![DescriptorProto {
            name: "AllKinds".into(),
            full_name: "pkg.AllKinds".into(),
            fields: codegen_fields,
            nested_types: vec![],
            enum_types: vec![],
            oneofs: vec![],
            is_map_entry: false,
        }],
        enums: vec![],
        services: vec![],
        sha256: String::new(),
        md5: String::new(),
        raw_bytes: vec![],
    };
    let generated = QuasarDescriptorRegistry::generate_rust_module_for_file(&cg_file);
    for kw in keywords {
        assert!(generated.contains(&format!("pub r#{kw}:")));
    }
    assert!(generated.contains("Vec<Box<SubItem>>"));
    assert!(generated.contains("Option<Box<SubItem>>"));
    assert!(generated.contains("f64"));
    assert!(generated.contains("f32"));
}
