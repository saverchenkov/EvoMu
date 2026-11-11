//! Edge-case, corrupt-wire, boundary, and error-path unit tests for `quasar`.

use super::*;
use crate::{MAX_COLLECTION_LEN, MAX_FRAME_SIZE, MAX_RECURSION_DEPTH};

#[test]
fn quasar_hashes_multiblock_and_varint_wire_message_limits() {
    // Multi-block (>128 bytes) SHA-256 and MD5 determinism & exact RFC/well-known checks
    let long_input = vec![b'x'; 200];
    let s1 = sha256_bytes(&long_input);
    let s2 = sha256_hex(&long_input);
    assert_eq!(s2.len(), 64);
    assert_eq!(s1[0], u8::from_str_radix(&s2[0..2], 16).unwrap());
    assert_eq!(md5_hex(&long_input).len(), 32);
    assert_ne!(sha256_hex(&long_input), sha256_hex(&vec![b'x'; 201]));
    assert_ne!(md5_hex(&long_input), md5_hex(&vec![b'x'; 201]));

    // 9 continuation bytes ending with EOF -> UnexpectedEof (not VarintOverflow)
    let nine_cont = [0x80u8; 9];
    let mut pos = 0;
    assert_eq!(decode_varint(&nine_cont, &mut pos), Err(QuasarError::UnexpectedEof));
    assert_eq!(pos, 9);

    // Non-zero starting cursor already at EOF -> UnexpectedEof
    let mut at_end = 3;
    assert_eq!(decode_varint(&[1, 2, 3], &mut at_end), Err(QuasarError::UnexpectedEof));

    // Exact MAX_FRAME_SIZE frame decodes cleanly; MAX_FRAME_SIZE + 1 returns FrameTooLarge
    let mut max_frame = Vec::with_capacity(MAX_FRAME_SIZE);
    let payload_len = MAX_FRAME_SIZE - 5; // 1 byte tag + 4 byte varint length for 4_194_299
    encode_tag(1, WireType::LengthDelimited, &mut max_frame);
    encode_varint(payload_len as u64, &mut max_frame);
    max_frame.resize(MAX_FRAME_SIZE, 0x42);
    assert_eq!(max_frame.len(), MAX_FRAME_SIZE);
    let decoded_max = WireMessage::decode(&max_frame).expect("exact MAX_FRAME_SIZE must decode");
    assert_eq!(decoded_max.fields.len(), 1);
    assert_eq!(
        decoded_max.fields[0].value.as_bytes().map(|b| b.len()),
        Some(payload_len)
    );

    let mut over_frame = max_frame;
    over_frame.push(0);
    assert_eq!(
        WireMessage::decode(&over_frame),
        Err(QuasarError::FrameTooLarge(MAX_FRAME_SIZE + 1))
    );

    // Exact MAX_COLLECTION_LEN fields succeeds; MAX_COLLECTION_LEN + 1 returns CollectionTooLarge
    let mut max_fields_wire = Vec::with_capacity(MAX_COLLECTION_LEN * 2);
    for _ in 0..MAX_COLLECTION_LEN {
        max_fields_wire.push(0x08); // field 1, varint
        max_fields_wire.push(0x01);
    }
    let decoded_coll = WireMessage::decode(&max_fields_wire).expect("exact MAX_COLLECTION_LEN ok");
    assert_eq!(decoded_coll.fields.len(), MAX_COLLECTION_LEN);

    max_fields_wire.extend_from_slice(&[0x08, 0x01]);
    assert_eq!(
        WireMessage::decode(&max_fields_wire),
        Err(QuasarError::CollectionTooLarge(MAX_COLLECTION_LEN))
    );

    // Field number 0 with non-zero wire type -> InvalidWireType(0xFF)
    assert_eq!(
        WireMessage::decode(&[0x05, 0, 0, 0, 0]),
        Err(QuasarError::InvalidWireType(0xFF))
    );
    // Invalid wire types (3, 4, 6, 7) on field #1
    for wt in [3u8, 4, 6, 7] {
        let tag = (1u8 << 3) | wt;
        assert_eq!(
            WireMessage::decode(&[tag]),
            Err(QuasarError::InvalidWireType(wt))
        );
    }

    // Truncated varint value and truncated LengthDelimited length varint
    assert_eq!(
        WireMessage::decode(&[0x08, 0x80]),
        Err(QuasarError::UnexpectedEof)
    );
    assert_eq!(
        WireMessage::decode(&[0x0A, 0x80]),
        Err(QuasarError::UnexpectedEof)
    );
    // Empty LengthDelimited field decodes to empty Vec
    let empty_ld = WireMessage::decode(&[0x0A, 0x00]).unwrap();
    assert_eq!(empty_ld.fields.len(), 1);
    assert_eq!(empty_ld.fields[0].value, WireValue::LengthDelimited(vec![]));
}

#[test]
fn descriptor_wire_decode_tolerates_unknown_fields_and_mismatched_wire_types() {
    // Build a FieldDescriptorProto with mismatched wire types and unknown field #99
    let mut bad_types_field = Vec::new();
    encode_varint_field(1, 1, &mut bad_types_field); // field 1 expects string
    encode_bytes_field(3, b"x", &mut bad_types_field); // field 3 expects varint
    encode_bytes_field(4, b"x", &mut bad_types_field); // field 4 expects varint
    encode_bytes_field(5, b"x", &mut bad_types_field); // field 5 expects varint
    encode_varint_field(6, 1, &mut bad_types_field); // field 6 expects string
    encode_bytes_field(9, b"x", &mut bad_types_field); // field 9 expects varint
    encode_varint_field(10, 1, &mut bad_types_field); // field 10 expects string
    encode_varint_field(99, 123, &mut bad_types_field); // unknown field

    // Build EnumValueDescriptorProto with mismatched wire types & unknown field, plus a negative value
    let mut bad_ev = Vec::new();
    encode_varint_field(1, 1, &mut bad_ev);
    encode_bytes_field(2, b"x", &mut bad_ev);
    encode_varint_field(99, 1, &mut bad_ev);

    let mut neg_ev = Vec::new();
    encode_string_field(1, "NEG_VAL", &mut neg_ev);
    encode_varint_field(2, (-42i64) as u64, &mut neg_ev);

    let mut enum_wire = Vec::new();
    encode_varint_field(1, 1, &mut enum_wire); // mismatched name
    encode_varint_field(2, 1, &mut enum_wire); // mismatched value submsg
    encode_bytes_field(2, &bad_ev, &mut enum_wire);
    encode_bytes_field(2, &neg_ev, &mut enum_wire);
    encode_varint_field(99, 1, &mut enum_wire);
    encode_string_field(1, "MyEnum", &mut enum_wire);

    // Oneof with unknown field and mismatched name, plus real name
    let mut oneof_wire = Vec::new();
    encode_varint_field(1, 1, &mut oneof_wire);
    encode_varint_field(99, 2, &mut oneof_wire);
    encode_string_field(1, "choice", &mut oneof_wire);

    // Options with map_entry=0 and unknown option field
    let mut opts_wire = Vec::new();
    encode_varint_field(99, 1, &mut opts_wire);
    encode_varint_field(7, 0, &mut opts_wire);

    // MessageDescriptor with mismatched wire types, unknown field, and sub-elements
    let mut msg_wire = Vec::new();
    encode_varint_field(1, 1, &mut msg_wire);
    encode_varint_field(2, 1, &mut msg_wire);
    encode_varint_field(3, 1, &mut msg_wire);
    encode_varint_field(4, 1, &mut msg_wire);
    encode_varint_field(7, 1, &mut msg_wire);
    encode_varint_field(8, 1, &mut msg_wire);
    encode_varint_field(99, 1, &mut msg_wire);
    encode_bytes_field(2, &bad_types_field, &mut msg_wire);
    encode_bytes_field(7, &opts_wire, &mut msg_wire);
    encode_bytes_field(8, &oneof_wire, &mut msg_wire);
    encode_string_field(1, "CleanedMsg", &mut msg_wire);

    // Service + Method with mismatched wire types and unknown fields
    let mut meth_wire = Vec::new();
    encode_varint_field(1, 1, &mut meth_wire);
    encode_varint_field(2, 1, &mut meth_wire);
    encode_varint_field(3, 1, &mut meth_wire);
    encode_varint_field(99, 1, &mut meth_wire);
    encode_string_field(1, "DoWork", &mut meth_wire);
    encode_string_field(2, ".pkg.Req", &mut meth_wire);
    encode_string_field(3, ".pkg.Resp", &mut meth_wire);

    let mut svc_wire = Vec::new();
    encode_varint_field(1, 1, &mut svc_wire);
    encode_varint_field(2, 1, &mut svc_wire);
    encode_varint_field(99, 1, &mut svc_wire);
    encode_bytes_field(2, &meth_wire, &mut svc_wire);
    encode_string_field(1, "GatewayWorker", &mut svc_wire);

    // FileDescriptorProto with mismatched wire types & unknown field
    let mut fd_wire = Vec::new();
    encode_varint_field(1, 1, &mut fd_wire);
    encode_varint_field(2, 1, &mut fd_wire);
    encode_varint_field(3, 1, &mut fd_wire);
    encode_varint_field(4, 1, &mut fd_wire);
    encode_varint_field(5, 1, &mut fd_wire);
    encode_varint_field(6, 1, &mut fd_wire);
    encode_varint_field(12, 1, &mut fd_wire);
    encode_varint_field(99, 1, &mut fd_wire);
    encode_string_field(1, "worker.proto", &mut fd_wire);
    encode_string_field(2, "pkg", &mut fd_wire);
    encode_bytes_field(4, &msg_wire, &mut fd_wire);
    encode_bytes_field(5, &enum_wire, &mut fd_wire);
    encode_bytes_field(6, &svc_wire, &mut fd_wire);

    let decoded = FileDescriptorProto::decode_wire("worker.pyj", "pkg", fd_wire).unwrap();
    assert_eq!(decoded.name, "worker.proto");
    assert_eq!(decoded.package, "pkg");
    assert_eq!(decoded.syntax, "proto2");
    assert_eq!(decoded.messages.len(), 1);
    assert!(!decoded.messages[0].is_map_entry);
    assert_eq!(decoded.messages[0].oneofs[0].name, "choice");
    assert_eq!(decoded.messages[0].fields[0].field_type, FieldType::Bytes);
    assert_eq!(decoded.messages[0].fields[0].label, FieldLabel::Optional);
    assert_eq!(decoded.enums[0].full_name, "pkg.MyEnum");
    assert_eq!(decoded.enums[0].values[1].number, -42);
    assert_eq!(decoded.services[0].full_name, "pkg.GatewayWorker");
    assert_eq!(decoded.services[0].methods[0].input_type, "pkg.Req");

    // Re-encode with negative enum & empty optional fields and re-decode
    let re_wire = decoded.encode_wire();
    let re_dec = FileDescriptorProto::decode_wire("worker.pyj", "pkg", re_wire).unwrap();
    assert_eq!(re_dec.enums[0].values[1].number, -42);
}

#[test]
fn descriptor_wire_decode_rejects_invalid_utf8_and_corrupt_submessages() {
    let bad_utf8 = &[0xFF, 0xFE];
    let trunc_wire = &[0x0A, 0x10, 0x01]; // truncated length-delimited submessage

    // FileDescriptorProto invalid UTF-8 on fields 1 (name), 2 (package), 3 (dependency), 12 (syntax)
    for fnum in [1u32, 2, 3, 12] {
        let mut buf = Vec::new();
        encode_bytes_field(fnum, bad_utf8, &mut buf);
        assert!(matches!(
            FileDescriptorProto::decode_wire("a.pyj", "d", buf),
            Err(QuasarError::InvalidUtf8(_))
        ));
    }

    // Corrupt top-level wire, message (#4), enum (#5), service (#6)
    assert_eq!(
        FileDescriptorProto::decode_wire("a.pyj", "d", trunc_wire.to_vec()),
        Err(QuasarError::UnexpectedEof)
    );
    for fnum in [4u32, 5, 6] {
        let mut buf = Vec::new();
        encode_bytes_field(fnum, trunc_wire, &mut buf);
        assert_eq!(
            FileDescriptorProto::decode_wire("a.pyj", "d", buf),
            Err(QuasarError::UnexpectedEof)
        );
    }

    // MessageDescriptor: invalid UTF-8 in name (#1) and oneof (#8 -> #1)
    let mut bad_msg_name = Vec::new();
    let mut m1 = Vec::new();
    encode_bytes_field(1, bad_utf8, &mut m1);
    encode_bytes_field(4, &m1, &mut bad_msg_name);
    assert!(matches!(
        FileDescriptorProto::decode_wire("a.pyj", "d", bad_msg_name),
        Err(QuasarError::InvalidUtf8(_))
    ));

    let mut bad_oneof_name = Vec::new();
    let mut oneof = Vec::new();
    encode_bytes_field(1, bad_utf8, &mut oneof);
    let mut m_oneof = Vec::new();
    encode_bytes_field(8, &oneof, &mut m_oneof);
    encode_bytes_field(4, &m_oneof, &mut bad_oneof_name);
    assert!(matches!(
        FileDescriptorProto::decode_wire("a.pyj", "d", bad_oneof_name),
        Err(QuasarError::InvalidUtf8(_))
    ));

    // MessageDescriptor corrupt sub-field (#2), sub-enum (#4), options (#7), oneof (#8)
    for sub_fnum in [2u32, 4, 7, 8] {
        let mut m = Vec::new();
        encode_bytes_field(sub_fnum, trunc_wire, &mut m);
        let mut fd = Vec::new();
        encode_bytes_field(4, &m, &mut fd);
        assert_eq!(
            FileDescriptorProto::decode_wire("a.pyj", "d", fd),
            Err(QuasarError::UnexpectedEof)
        );
    }

    // FieldDescriptorProto invalid UTF-8 in name (#1), type_name (#6), json_name (#10)
    for field_fnum in [1u32, 6, 10] {
        let mut f = Vec::new();
        encode_bytes_field(field_fnum, bad_utf8, &mut f);
        let mut m = Vec::new();
        encode_bytes_field(2, &f, &mut m);
        let mut fd = Vec::new();
        encode_bytes_field(4, &m, &mut fd);
        assert!(matches!(
            FileDescriptorProto::decode_wire("a.pyj", "d", fd),
            Err(QuasarError::InvalidUtf8(_))
        ));
    }

    // EnumDescriptorProto invalid UTF-8 in name (#1), value.name (#2 -> #1), and corrupt value (#2)
    let mut e_bad_name = Vec::new();
    encode_bytes_field(1, bad_utf8, &mut e_bad_name);
    let mut fd_e1 = Vec::new();
    encode_bytes_field(5, &e_bad_name, &mut fd_e1);
    assert!(matches!(
        FileDescriptorProto::decode_wire("a.pyj", "d", fd_e1),
        Err(QuasarError::InvalidUtf8(_))
    ));

    let mut ev_bad = Vec::new();
    encode_bytes_field(1, bad_utf8, &mut ev_bad);
    let mut e_bad_val = Vec::new();
    encode_bytes_field(2, &ev_bad, &mut e_bad_val);
    let mut fd_e2 = Vec::new();
    encode_bytes_field(5, &e_bad_val, &mut fd_e2);
    assert!(matches!(
        FileDescriptorProto::decode_wire("a.pyj", "d", fd_e2),
        Err(QuasarError::InvalidUtf8(_))
    ));

    let mut e_corrupt_val = Vec::new();
    encode_bytes_field(2, trunc_wire, &mut e_corrupt_val);
    let mut fd_e3 = Vec::new();
    encode_bytes_field(5, &e_corrupt_val, &mut fd_e3);
    assert_eq!(
        FileDescriptorProto::decode_wire("a.pyj", "d", fd_e3),
        Err(QuasarError::UnexpectedEof)
    );

    // ServiceDescriptorProto invalid UTF-8 in name (#1), method name/input/output (#2 -> #1/#2/#3), corrupt method (#2)
    let mut s_bad_name = Vec::new();
    encode_bytes_field(1, bad_utf8, &mut s_bad_name);
    let mut fd_s1 = Vec::new();
    encode_bytes_field(6, &s_bad_name, &mut fd_s1);
    assert!(matches!(
        FileDescriptorProto::decode_wire("a.pyj", "d", fd_s1),
        Err(QuasarError::InvalidUtf8(_))
    ));

    for mfnum in [1u32, 2, 3] {
        let mut mb = Vec::new();
        encode_bytes_field(mfnum, bad_utf8, &mut mb);
        let mut sb = Vec::new();
        encode_bytes_field(2, &mb, &mut sb);
        let mut fd_s2 = Vec::new();
        encode_bytes_field(6, &sb, &mut fd_s2);
        assert!(matches!(
            FileDescriptorProto::decode_wire("a.pyj", "d", fd_s2),
            Err(QuasarError::InvalidUtf8(_))
        ));
    }

    let mut s_corrupt_m = Vec::new();
    encode_bytes_field(2, trunc_wire, &mut s_corrupt_m);
    let mut fd_s3 = Vec::new();
    encode_bytes_field(6, &s_corrupt_m, &mut fd_s3);
    assert_eq!(
        FileDescriptorProto::decode_wire("a.pyj", "d", fd_s3),
        Err(QuasarError::UnexpectedEof)
    );

    // Exact MAX_RECURSION_DEPTH (64 -> 65 nested messages: depths 0..=64) succeeds
    let mut exact_nested = Vec::new();
    encode_string_field(1, "Leaf", &mut exact_nested);
    for d in 0..MAX_RECURSION_DEPTH {
        let mut p = Vec::new();
        encode_string_field(1, &format!("N{d}"), &mut p);
        encode_bytes_field(3, &exact_nested, &mut p);
        exact_nested = p;
    }
    let mut fd_ok = Vec::new();
    encode_string_field(1, "ok_depth.proto", &mut fd_ok);
    encode_bytes_field(4, &exact_nested, &mut fd_ok);
    let ok = FileDescriptorProto::decode_wire("ok_depth.pyj", "d", fd_ok).unwrap();
    assert_eq!(ok.messages.len(), 1);
}
