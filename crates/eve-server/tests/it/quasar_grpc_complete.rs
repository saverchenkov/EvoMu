//! Quasar core/shared protobuf types vs. the V24.01 client's own protobuf serializer.
//!
//! Golden vectors are produced locally by `tools/quasar-import/core_golden.py`, which runs the EVE
//! client's embedded Python 2.7 runtime and its own `google.protobuf` package over the client's
//! generated `eveProto/generated/*_pb2` modules. Values are DERIVED (invented, deterministic) and the
//! bytes are what the client's `SerializeToString()` emits. The output file is client-derived
//! (message names / field numbers) and therefore lives only in gitignored
//! `data/quasar_core_golden.json` (override with `EVOMU_QUASAR_CORE_GOLDEN_PATH`). The test fails
//! loudly when it is missing; it never skips.

use eve_proto::quasar::{
    ActiveIdentity, AuthoritativeContext, ColorRgb, DescriptorProto, DeviceUserIdentifier,
    DynamicQuasarMessage, DynamicValue, EveIdempotencyToken, EveInternalRequest,
    EveInternalResponse, EveIpAddress, EvePublicEvent, EvePublicNotice, EvePublicRequest,
    EvePublicResponse, EveResponseCacheInfo, EveResponsePagingInfo, FieldLabel,
    InternalGatewayConsumerInfo, InternalGatewayRequest, InternalGatewayRequestsSubscriptionInfo,
    InternalGatewayResponse, MathFraction, MathVector3, MathVector4, NetworkRttSample,
    NoticeTargetGroup, QuasarDescriptorRegistry, QuasarMessage, SemanticVersionSpec, TraceContext,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const CORE_TYPES_ROW: &str = "feat:quasar.core-types";

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn load_json(path: &PathBuf, what: &str) -> Value {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "{what} missing at {} ({e}). Generate it with \
             `/usr/bin/python3 tools/quasar-import/core_golden.py` against the local V24 client.",
            path.display()
        )
    });
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {what} {}: {e}", path.display()))
}

fn load_golden() -> Value {
    let path = std::env::var_os("EVOMU_QUASAR_CORE_GOLDEN_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            workspace_root()
                .join("data")
                .join("quasar_core_golden.json")
        });
    load_json(&path, "Quasar core-type golden vectors")
}

/// Domains named by the ledger row's `quasar:<domain>` sources (read-only input).
fn ledger_core_domains() -> Vec<String> {
    let path = workspace_root()
        .join(".agents/skills/evomu-adversarial-review/references/parity/ledger.json");
    let ledger = load_json(&path, "parity ledger");
    let rows = ledger
        .get("rows")
        .and_then(Value::as_array)
        .or_else(|| ledger.as_array())
        .expect("ledger rows array");
    let row = rows
        .iter()
        .find(|r| r["id"] == CORE_TYPES_ROW)
        .expect("core-types ledger row");
    row["sources"]
        .as_array()
        .expect("sources")
        .iter()
        .filter_map(|s| s.as_str()?.strip_prefix("quasar:").map(str::to_string))
        .collect()
}

fn hex_decode(s: &str) -> Vec<u8> {
    assert!(s.len() % 2 == 0, "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex byte"))
        .collect()
}

fn collect_messages<'a>(m: &'a DescriptorProto, out: &mut Vec<&'a DescriptorProto>) {
    out.push(m);
    for n in &m.nested_types {
        if !n.is_map_entry {
            collect_messages(n, out);
        }
    }
}

fn int_of(v: &DynamicValue) -> Option<i128> {
    Some(match v {
        DynamicValue::Int64(x) | DynamicValue::Sint64(x) | DynamicValue::Sfixed64(x) => *x as i128,
        DynamicValue::Uint64(x) | DynamicValue::Fixed64(x) => *x as i128,
        DynamicValue::Int32(x) | DynamicValue::Sint32(x) | DynamicValue::Sfixed32(x) => *x as i128,
        DynamicValue::Uint32(x) | DynamicValue::Fixed32(x) => *x as i128,
        DynamicValue::Enum(x) => *x as i128,
        _ => return None,
    })
}

/// Asserts that a dynamically decoded value equals the client-side typed value tree.
fn assert_value(path: &str, got: &DynamicValue, want: &Value) {
    let obj = want.as_object().expect("value node object");
    let (kind, w) = obj.iter().next().expect("value node kind");
    match kind.as_str() {
        "i" => {
            let expect: i128 = w.as_str().unwrap().parse().unwrap();
            assert_eq!(
                int_of(got),
                Some(expect),
                "{path}: integer mismatch ({got:?})"
            );
        }
        "f" => {
            let expect = w.as_f64().unwrap();
            let actual = match got {
                DynamicValue::Double(x) => *x,
                DynamicValue::Float(x) => *x as f64,
                other => panic!("{path}: expected float, got {other:?}"),
            };
            assert_eq!(actual, expect, "{path}: float mismatch");
        }
        "b" => assert_eq!(got, &DynamicValue::Bool(w.as_bool().unwrap()), "{path}"),
        "s" => assert_eq!(
            got,
            &DynamicValue::String(w.as_str().unwrap().into()),
            "{path}"
        ),
        "x" => assert_eq!(
            got,
            &DynamicValue::Bytes(hex_decode(w.as_str().unwrap())),
            "{path}"
        ),
        "m" => match got {
            DynamicValue::Message(sub) => assert_message(path, sub, w),
            other => panic!("{path}: expected message, got {other:?}"),
        },
        "r" => match got {
            DynamicValue::Repeated(items) => {
                let want_items = w.as_array().unwrap();
                assert_eq!(items.len(), want_items.len(), "{path}: repeated length");
                for (i, (g, wv)) in items.iter().zip(want_items).enumerate() {
                    assert_value(&format!("{path}[{i}]"), g, wv);
                }
            }
            other => panic!("{path}: expected repeated, got {other:?}"),
        },
        other => panic!("{path}: unknown golden kind {other}"),
    }
}

fn assert_message(path: &str, got: &DynamicQuasarMessage, want: &Value) {
    let want = want.as_object().expect("message tree");
    let want_keys: BTreeSet<u32> = want.keys().map(|k| k.parse().unwrap()).collect();
    let got_keys: BTreeSet<u32> = got.fields.keys().copied().collect();
    assert_eq!(
        got_keys, want_keys,
        "{path} ({}): decoded field set",
        got.full_name
    );
    for (k, wv) in want {
        let num: u32 = k.parse().unwrap();
        assert_value(&format!("{path}.{num}"), got.get(num).unwrap(), wv);
    }
}

/// Typed server-side structs (used by the gateway to build real wire envelopes) must re-emit the
/// client's bytes exactly, for every golden variant (each oneof alternative).
fn typed_roundtrip<T: QuasarMessage>(golden: &BTreeMap<String, Vec<Vec<u8>>>) -> usize {
    let variants = golden
        .get(T::FULL_NAME)
        .unwrap_or_else(|| panic!("golden vector for {} missing", T::FULL_NAME));
    for bytes in variants {
        let decoded = T::decode_proto(bytes)
            .unwrap_or_else(|e| panic!("typed decode {} from client bytes: {e}", T::FULL_NAME));
        assert_eq!(
            &decoded.encode_proto(),
            bytes,
            "typed {} must serialize identically to the V24 client",
            T::FULL_NAME
        );
    }
    1
}

#[tokio::test]
async fn quasar_core_types_serialize_identically_to_v24_client_protobuf_runtime() {
    // parity: plan:Q-1, plan:Q-2, feat:quasar.core-types, quasar:eveProto/generated/eve_public/public_pb2.pyj, quasar:eveProto/generated/eve_public/internal_gateway_pb2.pyj, quasar:eveProto/generated/eve_public/gateway_pb2.pyj
    eve_testkit::attest(&[
        "plan:Q-1",
        "plan:Q-2",
        "feat:quasar.core-types",
        "quasar:eveProto/generated/eve_public/public_pb2.pyj",
        "quasar:eveProto/generated/eve_public/internal_gateway_pb2.pyj",
        "quasar:eveProto/generated/eve_public/gateway_pb2.pyj",
    ]);

    let reg = QuasarDescriptorRegistry::open_default()
        .expect("load data/quasar_descriptors.sqlite (V24 client FileDescriptorProtos)");
    let golden = load_golden();

    // 1. Golden vectors were produced by the client's own protobuf runtime for exactly the
    //    ledger row's domains.
    let domains = ledger_core_domains();
    assert_eq!(domains.len(), 13, "core-types row names 13 quasar domains");
    let golden_domains: Vec<String> = golden["domains"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap().to_string())
        .collect();
    assert_eq!(golden_domains, domains);
    assert!(golden["protobuf_impl"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));

    // 2. Every descriptor file and every (non map-entry) message of those domains is covered.
    let golden_files = golden["files"].as_array().unwrap();
    let mut by_file: BTreeMap<String, &Value> = BTreeMap::new();
    for f in golden_files {
        by_file.insert(f["pyj_path"].as_str().unwrap().to_string(), f);
    }
    let mut expected_files = 0usize;
    let mut expected_messages: BTreeSet<String> = BTreeSet::new();
    for d in &domains {
        let idxs = reg
            .files_by_domain
            .get(d)
            .unwrap_or_else(|| panic!("domain {d} missing from QuasarDescriptorRegistry"));
        assert!(!idxs.is_empty(), "domain {d} has descriptor files");
        for &i in idxs {
            let fd = &reg.files[i];
            expected_files += 1;
            let gf = by_file
                .get(&fd.pyj_path)
                .unwrap_or_else(|| panic!("no golden vectors for {}", fd.pyj_path));
            assert_eq!(gf["domain"].as_str(), Some(d.as_str()));
            assert_eq!(gf["proto_name"].as_str(), Some(fd.name.as_str()));
            let mut msgs = Vec::new();
            for m in &fd.messages {
                collect_messages(m, &mut msgs);
            }
            let reg_names: BTreeSet<String> = msgs.iter().map(|m| m.full_name.clone()).collect();
            let gold_names: BTreeSet<String> = gf["messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["full_name"].as_str().unwrap().to_string())
                .collect();
            assert_eq!(reg_names, gold_names, "message set of {}", fd.pyj_path);
            expected_messages.extend(reg_names);
        }
    }
    assert_eq!(
        by_file.len(),
        expected_files,
        "golden file count == registry file count"
    );

    // 3. Decode each client-serialized message, check every value, re-encode byte-identically.
    let mut bytes_by_name: BTreeMap<String, Vec<Vec<u8>>> = BTreeMap::new();
    let mut populated: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    let mut vectors = 0usize;
    let mut nonempty = 0usize;
    let mut type_coverage: BTreeSet<String> = BTreeSet::new();
    for f in golden_files {
        for gm in f["messages"].as_array().unwrap() {
            let name = gm["full_name"].as_str().unwrap();
            let wire = hex_decode(gm["wire_hex"].as_str().unwrap());
            let desc = reg
                .get_message(name)
                .unwrap_or_else(|| panic!("{name} not in registry"));
            let decoded = reg
                .decode_dynamic(name, &wire)
                .unwrap_or_else(|e| panic!("decode client bytes of {name}: {e}"));
            assert_message(name, &decoded, &gm["values"]);
            let reencoded = reg
                .encode_dynamic(&decoded)
                .unwrap_or_else(|e| panic!("encode {name}: {e}"));
            assert_eq!(
                reencoded, wire,
                "{name}: EvoMu serialization differs from the V24 client protobuf runtime"
            );
            for fdesc in &desc.fields {
                if decoded.fields.contains_key(&fdesc.number) {
                    let label = if fdesc.label == FieldLabel::Repeated {
                        "repeated "
                    } else {
                        ""
                    };
                    type_coverage.insert(format!("{label}{:?}", fdesc.field_type));
                }
            }
            populated
                .entry(name.to_string())
                .or_default()
                .extend(decoded.fields.keys().copied());
            if !wire.is_empty() {
                nonempty += 1;
            }
            vectors += 1;
            bytes_by_name
                .entry(name.to_string())
                .or_default()
                .push(wire);
        }
    }
    let populated_names: BTreeSet<String> = populated.keys().cloned().collect();
    assert_eq!(
        populated_names, expected_messages,
        "every core message has golden vectors"
    );
    // Across the variants (one per oneof alternative) every descriptor field of every core message
    // was populated by the client serializer, so every field / wire type / oneof arm is exercised.
    let mut fields_checked = 0usize;
    for name in &expected_messages {
        let desc = reg.get_message(name).unwrap();
        let all: BTreeSet<u32> = desc.fields.iter().map(|f| f.number).collect();
        assert_eq!(
            &populated[name], &all,
            "{name}: every field populated in some variant"
        );
        fields_checked += all.len();
    }
    assert!(nonempty > 0 && fields_checked > 0);
    for t in [
        "Int32", "Int64", "Uint32", "Uint64", "String", "Bytes", "Bool", "Enum", "Double",
    ] {
        assert!(
            type_coverage.contains(t),
            "core types exercise {t} fields: {type_coverage:?}"
        );
    }
    let checked = expected_messages.len();
    let scalar_fields_checked = fields_checked;

    // 4. The typed structs the gateway uses on the wire match the client byte-for-byte.
    let mut typed = 0usize;
    typed += typed_roundtrip::<EvePublicRequest>(&bytes_by_name);
    typed += typed_roundtrip::<EvePublicResponse>(&bytes_by_name);
    typed += typed_roundtrip::<EvePublicNotice>(&bytes_by_name);
    typed += typed_roundtrip::<NoticeTargetGroup>(&bytes_by_name);
    typed += typed_roundtrip::<EvePublicEvent>(&bytes_by_name);
    typed += typed_roundtrip::<AuthoritativeContext>(&bytes_by_name);
    typed += typed_roundtrip::<ActiveIdentity>(&bytes_by_name);
    typed += typed_roundtrip::<EveInternalRequest>(&bytes_by_name);
    typed += typed_roundtrip::<EveInternalResponse>(&bytes_by_name);
    typed += typed_roundtrip::<EveResponseCacheInfo>(&bytes_by_name);
    typed += typed_roundtrip::<EveResponsePagingInfo>(&bytes_by_name);
    typed += typed_roundtrip::<InternalGatewayRequest>(&bytes_by_name);
    typed += typed_roundtrip::<InternalGatewayResponse>(&bytes_by_name);
    typed += typed_roundtrip::<InternalGatewayRequestsSubscriptionInfo>(&bytes_by_name);
    typed += typed_roundtrip::<InternalGatewayConsumerInfo>(&bytes_by_name);
    typed += typed_roundtrip::<EveIdempotencyToken>(&bytes_by_name);
    typed += typed_roundtrip::<EveIpAddress>(&bytes_by_name);
    typed += typed_roundtrip::<TraceContext>(&bytes_by_name);
    typed += typed_roundtrip::<SemanticVersionSpec>(&bytes_by_name);
    typed += typed_roundtrip::<ColorRgb>(&bytes_by_name);
    typed += typed_roundtrip::<MathFraction>(&bytes_by_name);
    typed += typed_roundtrip::<MathVector3>(&bytes_by_name);
    typed += typed_roundtrip::<MathVector4>(&bytes_by_name);
    typed += typed_roundtrip::<DeviceUserIdentifier>(&bytes_by_name);
    typed += typed_roundtrip::<NetworkRttSample>(&bytes_by_name);
    assert_eq!(typed, 25);
    eprintln!(
        "core-types: {} files, {checked} messages ({nonempty} non-empty), \
         {vectors} client vectors, {scalar_fields_checked} fields, {typed} typed structs byte-identical",
        by_file.len()
    );
}

#[tokio::test]
async fn q1_q2_gateway_golden_wire_codec_and_descriptor_acceptance() {
    // parity: plan:Q-1, plan:Q-2
    eve_testkit::attest(&["plan:Q-1", "plan:Q-2"]);

    let reg = QuasarDescriptorRegistry::open_default()
        .expect("load data/quasar_descriptors.sqlite");

    // Q-1 Acceptance: All 1,455 descriptors extracted with recorded checksum
    assert_eq!(reg.files.len(), 1_455);
    assert_eq!(reg.manifest.descriptor_file_count, 1_455);
    assert_eq!(reg.files_by_domain.len(), 135);
    assert_eq!(reg.manifest.domain_count, 135);
    assert_eq!(reg.manifest.unique_package_count, 1_270);
    assert_eq!(reg.manifest.total_message_count, 6_741);
    assert_eq!(
        reg.manifest.combined_serialized_pb_sha256,
        "06a2d8bde8222e444dcb740932cc3ef5e208bb853693d33a6a48ec8664f068be"
    );
    assert_eq!(
        reg.manifest.combined_serialized_pb_md5,
        "318048feebdaee6d35565c0e2d5b284c"
    );

    // Q-2 Acceptance: Golden wire tests pass against client's own gateway codec
    let golden = load_golden();
    assert!(golden["domains"].as_array().unwrap().len() >= 13);
    assert!(golden["files"].as_array().unwrap().len() >= 25);
}

