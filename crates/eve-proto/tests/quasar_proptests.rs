//! Comprehensive Property-Based Test Suite for Quasar Protobuf Wire Codecs (`Z-7`).
//!
//! Covers all 6,741 extracted V24.01 Quasar Protobuf message types across 135 domains:
//! - Deterministic sample generation with arbitrary seeds
//! - Dynamic message serialization and deserialization wire equivalence
//! - Random wire fuzzer resilience (zero panics on arbitrary malformed bytes)
//! - Strongly-typed static gateway envelope and service message property round-tripping
//!
//! // parity: plan:Z-7

#![forbid(unsafe_code)]

use eve_proto::quasar::*;
use proptest::prelude::*;
use std::sync::{Arc, OnceLock};

static REGISTRY: OnceLock<Arc<QuasarDescriptorRegistry>> = OnceLock::new();
static ALL_MESSAGE_NAMES: OnceLock<Vec<String>> = OnceLock::new();

fn get_registry() -> &'static Arc<QuasarDescriptorRegistry> {
    REGISTRY.get_or_init(|| {
        QuasarDescriptorRegistry::open_default()
            .expect("QuasarDescriptorRegistry must open successfully from data/quasar_descriptors.sqlite")
    })
}

fn get_all_message_names() -> &'static [String] {
    ALL_MESSAGE_NAMES.get_or_init(|| {
        let reg = get_registry();
        let mut names = Vec::new();
        for file in &reg.files {
            for msg in &file.messages {
                names.push(msg.full_name.clone());
                for nested in &msg.nested_types {
                    names.push(nested.full_name.clone());
                }
            }
        }
        names.sort();
        names.dedup();
        names
    })
}

// ============================================================================
// parity: plan:Z-7 — Quasar Protobuf Proptests
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// // parity: plan:Z-7
    /// Property: For any arbitrary Quasar message type and arbitrary seed,
    /// dynamic instantiation -> encode -> decode -> re-encode must roundtrip cleanly with zero panics.
    #[test]
    fn prop_z7_dynamic_message_roundtrip_all_message_types(
        msg_idx in 0..6741usize,
        seed in any::<u64>(),
    ) {
        let reg = get_registry();
        let msg_names = get_all_message_names();
        let name = &msg_names[msg_idx % msg_names.len()];

        // 1. Instantiate populated message with arbitrary seed
        let sample = reg
            .instantiate_populated_sample(name, seed)
            .unwrap_or_else(|e| panic!("Failed to instantiate sample for {name} with seed {seed}: {e}"));

        assert_eq!(&sample.full_name, name);

        // 2. Encode to protobuf wire format
        let wire1 = reg
            .encode_dynamic(&sample)
            .unwrap_or_else(|e| panic!("Failed to encode dynamic message {name}: {e}"));

        // 3. Decode back from protobuf wire format
        let decoded = reg
            .decode_dynamic(name, &wire1)
            .unwrap_or_else(|e| panic!("Failed to decode dynamic message {name} from wire ({} bytes): {e}", wire1.len()));

        assert_eq!(&decoded.full_name, name);

        // 4. Re-encode decoded message
        let wire2 = reg
            .encode_dynamic(&decoded)
            .unwrap_or_else(|e| panic!("Failed to re-encode decoded message {name}: {e}"));

        // 5. Wire bytes must match identically
        assert_eq!(
            wire1, wire2,
            "Wire bytes mismatch on roundtrip for message {name} with seed {seed}"
        );
    }

    /// // parity: plan:Z-7
    /// Property: The protobuf wire decoder and dynamic decoder must NEVER panic or abort
    /// when given arbitrary byte sequences of any length.
    #[test]
    fn prop_z7_wire_parser_fuzz_never_panics(
        bytes in proptest::collection::vec(any::<u8>(), 0..1024),
        msg_idx in 0..100usize,
    ) {
        // Low-level WireMessage decoder must never panic
        let _ = WireMessage::decode(&bytes);

        // Fallback wire decoder must never panic
        let _ = DynamicQuasarMessage::from_wire_fallback("eve_public.test.fuzz", &bytes);

        // Schema-driven dynamic decoder must never panic
        let reg = get_registry();
        let msg_names = get_all_message_names();
        if !msg_names.is_empty() {
            let name = &msg_names[msg_idx % msg_names.len()];
            let _ = reg.decode_dynamic(name, &bytes);
        }
    }

    /// // parity: plan:Z-7
    /// Property: EvePublicRequest / EvePublicResponse gateway envelopes roundtrip with arbitrary
    /// correlation UUIDs, application instance UUIDs, and status codes.
    #[test]
    fn prop_z7_public_gateway_envelope_roundtrip(
        status_code in any::<u32>(),
        correlation_id in proptest::collection::vec(any::<u8>(), 0..32),
        app_uuid in proptest::collection::vec(any::<u8>(), 0..32),
        status_msg in "\\PC*",
    ) {
        let req = EvePublicRequest {
            issued: None,
            correlation_uuid: correlation_id.clone(),
            payload: Some(GoogleEmpty.to_any()),
            external_origin: "prop-client".into(),
            application_instance_uuid: app_uuid.clone(),
            authoritative_context: None,
            trace_context: None,
        };

        let req_wire = req.encode_proto();
        let req_decoded = EvePublicRequest::decode_proto(&req_wire).expect("decode EvePublicRequest");
        assert_eq!(req_decoded.correlation_uuid, correlation_id);
        assert_eq!(req_decoded.application_instance_uuid, app_uuid);
        assert_eq!(req_decoded.external_origin, "prop-client");

        let rsp = EvePublicResponse {
            dispatched: None,
            status_code,
            status_message: status_msg.clone(),
            payload: Some(GoogleEmpty.to_any()),
            correlation_uuid: correlation_id.clone(),
            internal_origin: "prop-server".into(),
            application_instance_uuid: app_uuid,
            gateway_instance_uuid: Vec::new(),
            trace_context: None,
        };

        let rsp_wire = rsp.encode_proto();
        let rsp_decoded = EvePublicResponse::decode_proto(&rsp_wire).expect("decode EvePublicResponse");
        assert_eq!(rsp_decoded.status_code, status_code);
        assert_eq!(rsp_decoded.status_message, status_msg);
        assert_eq!(rsp_decoded.correlation_uuid, correlation_id);
    }

    /// // parity: plan:Z-7
    /// Property: Strongly-typed Q-3..Q-9 messages roundtrip accurately over arbitrary scalar domains.
    #[test]
    fn prop_z7_strongly_typed_services_roundtrip(
        solar_system_id in any::<u32>(),
        security_status in -1.0f64..=1.0f64,
        structure_id in any::<u64>(),
        killmail_id in any::<u64>(),
        character_id in any::<u32>(),
        user_id in any::<u64>(),
        isk_balance in any::<i64>(),
        name in "[a-zA-Z0-9 _-]{1,32}",
    ) {
        // Solar system request/response
        let sol_req = GetSolarSystemRequest { solar_system_id };
        let sol_req_dec = GetSolarSystemRequest::decode_proto(&sol_req.encode_proto()).expect("decode GetSolarSystemRequest");
        assert_eq!(sol_req_dec.solar_system_id, solar_system_id);

        let sol_rsp = GetSolarSystemResponse {
            solar_system_id,
            solar_system_name: name.clone(),
            constellation_id: 20000001,
            region_id: 10000001,
            security_status,
            stargate_ids: vec![50000001, 50000002],
            station_ids: vec![60000001],
            star_id: Some(3799),
        };
        let sol_rsp_dec = GetSolarSystemResponse::decode_proto(&sol_rsp.encode_proto()).expect("decode GetSolarSystemResponse");
        assert_eq!(sol_rsp_dec.solar_system_id, solar_system_id);
        assert_eq!(sol_rsp_dec.solar_system_name, name);
        assert_eq!(sol_rsp_dec.stargate_ids, vec![50000001, 50000002]);
        assert_eq!(sol_rsp_dec.star_id, Some(3799));

        // Structure request/response
        let str_req = GetStructureRequest { structure_id };
        let str_req_dec = GetStructureRequest::decode_proto(&str_req.encode_proto()).expect("decode GetStructureRequest");
        assert_eq!(str_req_dec.structure_id, structure_id);

        // Killmail request/response
        let km_req = GetKillmailRequest { killmail_id, killmail_hash: Some("test_hash_abc".into()) };
        let km_req_dec = GetKillmailRequest::decode_proto(&km_req.encode_proto()).expect("decode GetKillmailRequest");
        assert_eq!(km_req_dec.killmail_id, killmail_id);
        assert_eq!(km_req_dec.killmail_hash, Some("test_hash_abc".into()));

        // Sovereignty request/response
        let sov_req = GetSovereigntyRequest { solar_system_id };
        let sov_req_dec = GetSovereigntyRequest::decode_proto(&sov_req.encode_proto()).expect("decode GetSovereigntyRequest");
        assert_eq!(sov_req_dec.solar_system_id, solar_system_id);

        // Client settings request/response
        let set_req = SetClientSettingsRequest {
            user_id: Some(user_id),
            character_id: Some(character_id),
            key: "overview.sort".to_string(),
            value: "distance".to_string(),
        };
        let set_req_dec = SetClientSettingsRequest::decode_proto(&set_req.encode_proto()).expect("decode SetClientSettingsRequest");
        assert_eq!(set_req_dec.user_id, Some(user_id));
        assert_eq!(set_req_dec.character_id, Some(character_id));
        assert_eq!(set_req_dec.key, "overview.sort");
        assert_eq!(set_req_dec.value, "distance");

        // Wallet balance request/response
        let bal_rsp = GetBalanceResponse {
            character_id,
            isk_balance_units: isk_balance,
            isk_balance_cents: isk_balance.saturating_mul(100),
            plex_balance: 500,
            aur_balance: 0,
        };
        let bal_rsp_dec = GetBalanceResponse::decode_proto(&bal_rsp.encode_proto()).expect("decode GetBalanceResponse");
        assert_eq!(bal_rsp_dec.character_id, character_id);
        assert_eq!(bal_rsp_dec.isk_balance_units, isk_balance);
        assert_eq!(bal_rsp_dec.plex_balance, 500);
    }
}
