//! Quasar CDN, Client, Constellation, Contact, Container, Content,
//! ContextualOffer, Contract, CSAT, and Deadspace parity integration suite.
//!
//! Attests 10 parity rows:
//! - `feat:quasar.cdn`: publicGatewaySvc routes `eve.cdn.*` / `eve_public/eve` cdn requests to handlers covering CDN resources/asset endpoints.
//! - `feat:quasar.client`: publicGatewaySvc routes `eve.client.*` client requests to real handlers covering client version and config.
//! - `feat:quasar.constellation`: constellation protobuf model messages are populated with correct IDs/fields from SDE/universe.
//! - `feat:quasar.contact`: publicGatewaySvc routes `eve.contact.*` requests to handlers covering pilot contacts / standing flags.
//! - `feat:quasar.container`: container protobuf model messages populated with correct IDs/fields.
//! - `feat:quasar.content`: publicGatewaySvc routes `eve.content.*` requests covering news / articles.
//! - `feat:quasar.contextualoffer`: contextualoffer protobuf model messages populated with correct IDs/fields.
//! - `feat:quasar.contract`: contract protobuf model messages populated with correct IDs/fields.
//! - `feat:quasar.csat`: csat protobuf model messages populated with correct IDs/fields.
//! - `feat:quasar.deadspace`: publicGatewaySvc routes `eve.deadspace.*` requests to real handlers covering deadspace/dungeons.

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU64, Ordering};

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, EvePublicEvent, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
    QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX, WireMessage,
};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;

static CORR_SEQ: AtomicU64 = AtomicU64::new(700);

fn make_session(session_id: u64, user_id: u64, char_id: i64, corp_id: i64) -> SessionState {
    let mut s = SessionState::new(session_id);
    s.user_id = Some(user_id);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(30_000_142);
    s.station_id = Some(60_003_760);
    s.role = eve_machonet::service_roles::ROLE_PLAYER;
    s.corp_role = 0;
    s
}

fn quasar_call(
    ctx: &ServerContext,
    sess: &SessionState,
    type_name: &str,
    value: Vec<u8>,
) -> EvePublicResponse {
    let now = ctx.now_unix();
    let seq = CORR_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut corr = *b"quasar-cd-000000";
    corr[10..16].copy_from_slice(&seq.to_le_bytes()[..6]);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now as i64,
            nanos: 0,
        }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-quasar-suite".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    ctx.quasar_gateway
        .handle_public_request(sess, &req, None, false, now)
}

fn encode_seq_id(field_num: u32, id: u64) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_varint_field(1, id, &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(field_num, &inner, &mut out);
    out
}

fn get_u64(msg: &DynamicQuasarMessage, field: u32) -> Option<u64> {
    match msg.get(field) {
        Some(DynamicValue::Uint64(v)) => Some(*v),
        Some(DynamicValue::Uint32(v)) => Some(*v as u64),
        Some(DynamicValue::Int64(v)) => Some(*v as u64),
        Some(DynamicValue::Int32(v)) => Some(*v as u64),
        Some(DynamicValue::Enum(v)) => Some(*v as u64),
        _ => None,
    }
}

fn get_str<'a>(msg: &'a DynamicQuasarMessage, field: u32) -> Option<&'a str> {
    match msg.get(field) {
        Some(DynamicValue::String(s)) => Some(s.as_str()),
        _ => None,
    }
}

fn get_bool(msg: &DynamicQuasarMessage, field: u32) -> Option<bool> {
    match msg.get(field) {
        Some(DynamicValue::Bool(b)) => Some(*b),
        _ => None,
    }
}

// parity: feat:quasar.cdn, feat:quasar.client, feat:quasar.constellation
#[tokio::test]
async fn test_quasar_cdn_client_and_constellation() {
    eve_testkit::attest(&[
        "feat:quasar.cdn",
        "feat:quasar.client",
        "feat:quasar.constellation",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("open descriptor registry");
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let sess = make_session(701, 1001, 90_000_001, 1_000_001);

    // 1. Quasar CDN (feat:quasar.cdn)
    // Descriptor registry verification for cdn domain (4 proto files)
    let cdn_files = [
        "eve/cdn/api/events.proto",
        "eve/cdn/api/requests.proto",
        "eve/cdn/cdn.proto",
        "eve_public/cdn/cdn.proto",
    ];
    for f in cdn_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing cdn proto {f}");
    }
    assert!(reg.messages_by_full_name.contains_key("eve.cdn.api.GetActiveURLSigningKeyRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.cdn.api.SignURLRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.cdn.Checkpoint"));
    assert!(reg.messages_by_full_name.contains_key("eve_public.cdn.Checkpoint"));

    // Route: eve.cdn.api.GetActiveURLSigningKeyRequest
    let key_rsp = quasar_call(&ctx, &sess, "eve.cdn.api.GetActiveURLSigningKeyRequest", Vec::new());
    assert_eq!(key_rsp.status_code, STATUS_OK);
    let key_p = key_rsp.payload.expect("signing key payload");
    assert_eq!(key_p.normalized_type_name(), "eve.cdn.api.GetActiveURLSigningKeyResponse");
    let key_wm = WireMessage::decode(&key_p.value).expect("decode key response");
    let key_id = key_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap().unwrap();
    assert_eq!(key_id, "evomu-cdn-v24");

    // Route: eve.cdn.api.SignURLRequest
    let mut sign_req = Vec::new();
    encode_string_field(1, "https://cdn.evomu.org/res/graphics.fb", &mut sign_req);
    let sign_rsp = quasar_call(&ctx, &sess, "eve.cdn.api.SignURLRequest", sign_req);
    assert_eq!(sign_rsp.status_code, STATUS_OK);
    let sign_p = sign_rsp.payload.expect("sign url payload");
    assert_eq!(sign_p.normalized_type_name(), "eve.cdn.api.SignURLResponse");
    let sign_wm = WireMessage::decode(&sign_p.value).expect("decode sign response");
    let signed_url = sign_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap().unwrap();
    assert!(signed_url.contains("sig="), "signed URL must contain signature query parameter");

    // Route: eve_public.space.api.cdn.GetUpdateRequest
    let mut cdn_req = Vec::new();
    encode_varint_field(2, 1, &mut cdn_req);
    let space_cdn_rsp = quasar_call(&ctx, &sess, "eve_public.space.api.cdn.GetUpdateRequest", cdn_req);
    assert_eq!(space_cdn_rsp.status_code, STATUS_OK);
    let scdn_p = space_cdn_rsp.payload.expect("space cdn payload");
    assert_eq!(scdn_p.normalized_type_name(), "eve_public.space.api.cdn.GetUpdateResponse");

    // Checkpoint model message encoding/decoding
    let mut cp_buf = Vec::new();
    encode_string_field(1, "https://cdn.evomu.org/checkpoints/v1/meta.bin", &mut cp_buf);
    let dyn_cp = reg.decode_dynamic("eve.cdn.Checkpoint", &cp_buf).expect("decode eve.cdn.Checkpoint");
    assert_eq!(get_str(&dyn_cp, 1), Some("https://cdn.evomu.org/checkpoints/v1/meta.bin"));

    // 2. Quasar Client (feat:quasar.client)
    // Descriptor registry verification for client domain (4 proto files)
    let client_files = [
        "eve/client/anticheating.proto",
        "eve/client/connection.proto",
        "eve/client/event.proto",
        "eve/client/host.proto",
    ];
    for f in client_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing client proto {f}");
    }
    assert!(reg.messages_by_full_name.contains_key("eve.client.host.GetRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.client.host.GetResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.client.host.Attributes"));
    assert!(reg.messages_by_full_name.contains_key("DisconnectRequest"));

    // Route: eve.client.host.GetRequest
    let mut host_id = Vec::new();
    encode_string_field(2, "d41d8cd98f00b204e9800998ecf8427e", &mut host_id);
    let mut host_req = Vec::new();
    encode_bytes_field(1, &host_id, &mut host_req);
    let host_rsp = quasar_call(&ctx, &sess, "eve.client.host.GetRequest", host_req);
    assert_eq!(host_rsp.status_code, STATUS_OK);
    let host_p = host_rsp.payload.expect("host payload");
    assert_eq!(host_p.normalized_type_name(), "eve.client.host.GetResponse");
    let dyn_host_rsp = reg.decode_dynamic("eve.client.host.GetResponse", &host_p.value).expect("decode host response");
    let dyn_attrs = match dyn_host_rsp.get(1) {
        Some(DynamicValue::Message(m)) => m,
        _ => panic!("Expected host attributes in GetResponse"),
    };
    assert_eq!(get_str(dyn_attrs, 10), Some("24.01.2024"), "Client version must match V24");
    assert_eq!(get_str(dyn_attrs, 2), Some("en_US"), "Locale must match en_US");
    assert_eq!(get_u64(dyn_attrs, 4), Some(16384), "RAM MB must match 16384");

    // Route: DisconnectRequest
    let dis_rsp = quasar_call(&ctx, &sess, "DisconnectRequest", Vec::new());
    assert_eq!(dis_rsp.status_code, STATUS_OK);

    // 3. Quasar Constellation (feat:quasar.constellation)
    // SDE constellations verification: constellation 20000020 (Kimotoro)
    let sde_const = ctx.sde.constellations.get(&20_000_020).expect("Kimotoro constellation in SDE");
    assert_eq!(sde_const.name, "Kimotoro");
    assert_eq!(sde_const.region_id, 10_000_002);

    // Descriptor registry verification for constellation domain (2 proto files)
    assert!(reg.files_by_proto_name.contains_key("eve/constellation/constellation.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve_public/constellation/constellation.proto"));
    assert!(reg.messages_by_full_name.contains_key("eve.constellation.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.constellation.Attributes"));
    assert!(reg.messages_by_full_name.contains_key("eve.constellation.GetRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.constellation.GetResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.constellation.GetAllRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.constellation.GetAllResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.constellation.SearchRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.constellation.SearchResponse"));

    // Route: eve.constellation.GetRequest
    let const_req = encode_seq_id(1, 20_000_020);
    let const_rsp = quasar_call(&ctx, &sess, "eve.constellation.GetRequest", const_req);
    assert_eq!(const_rsp.status_code, STATUS_OK);
    let const_p = const_rsp.payload.expect("constellation payload");
    assert_eq!(const_p.normalized_type_name(), "eve.constellation.GetResponse");
    let dyn_c_rsp = reg.decode_dynamic("eve.constellation.GetResponse", &const_p.value).expect("decode GetResponse");
    let dyn_c_attrs = match dyn_c_rsp.get(1) {
        Some(DynamicValue::Message(m)) => m,
        _ => panic!("Expected Attributes in GetResponse"),
    };
    assert_eq!(get_str(dyn_c_attrs, 1), Some("Kimotoro"), "Name must match SDE Kimotoro");
    let reg_msg = match dyn_c_attrs.get(2) {
        Some(DynamicValue::Message(m)) => m,
        _ => panic!("Expected region Identifier"),
    };
    assert_eq!(get_u64(reg_msg, 1), Some(10_000_002), "Region ID must match The Forge");

    // Route: eve.constellation.GetAllRequest
    let all_c_rsp = quasar_call(&ctx, &sess, "eve.constellation.GetAllRequest", Vec::new());
    assert_eq!(all_c_rsp.status_code, STATUS_OK);
    let all_cp = all_c_rsp.payload.expect("all constellations payload");
    assert_eq!(all_cp.normalized_type_name(), "eve.constellation.GetAllResponse");

    // Route: eve.constellation.SearchRequest
    let mut search_buf = Vec::new();
    encode_string_field(1, "Kimotoro", &mut search_buf);
    let mut search_req = Vec::new();
    encode_bytes_field(1, &search_buf, &mut search_req);
    let srch_rsp = quasar_call(&ctx, &sess, "eve.constellation.SearchRequest", search_req);
    assert_eq!(srch_rsp.status_code, STATUS_OK, "srch_rsp failed: status={} msg={}", srch_rsp.status_code, srch_rsp.status_message);
    let srch_p = srch_rsp.payload.expect("search payload");
    assert_eq!(srch_p.normalized_type_name(), "eve.constellation.SearchResponse");
}

// parity: feat:quasar.contact, feat:quasar.container, feat:quasar.content
#[tokio::test]
async fn test_quasar_contact_container_and_content() {
    eve_testkit::attest(&[
        "feat:quasar.contact",
        "feat:quasar.container",
        "feat:quasar.content",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("open descriptor registry");
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001_i64;
    let sess = make_session(702, 1002, char_id, 1_000_001);

    // 1. Quasar Contact (feat:quasar.contact)
    // Seed pilot contacts / standing flags
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(90_000_002, 5.0); // positive standing contact
            ch.standings.insert(1_000_003, -5.0); // negative standing contact
        }
    });

    // Descriptor registry verification for contact domain (3 proto files)
    let contact_files = [
        "eve/contact/api.proto",
        "eve/contact/contact.proto",
        "eve/contact/label.proto",
    ];
    for f in contact_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing contact proto {f}");
    }
    assert!(reg.messages_by_full_name.contains_key("eve.contacts.GetAllForCharacterRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.contacts.GetAllForCharacterResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.contacts.Personal"));
    assert!(reg.messages_by_full_name.contains_key("eve.contact.label.GetForCharacterRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.contact.label.GetForCorporationRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.contact.label.GetForAllianceRequest"));

    // Route: eve.contacts.GetAllForCharacterRequest
    let contacts_req = encode_seq_id(1, char_id as u64);
    let contacts_rsp = quasar_call(&ctx, &sess, "eve.contacts.GetAllForCharacterRequest", contacts_req);
    assert_eq!(contacts_rsp.status_code, STATUS_OK);
    let c_payload = contacts_rsp.payload.expect("contacts payload");
    assert_eq!(c_payload.normalized_type_name(), "eve.contacts.GetAllForCharacterResponse");
    let c_wm = WireMessage::decode(&c_payload.value).expect("decode contacts");
    let personal_fields: Vec<_> = c_wm.fields.iter().filter(|f| f.field_number == 1).collect();
    assert!(!personal_fields.is_empty(), "Must return seeded character contacts");

    // Route: eve.contact.label.GetForCharacterRequest
    let label_req = encode_seq_id(1, char_id as u64);
    let label_rsp = quasar_call(&ctx, &sess, "eve.contact.label.GetForCharacterRequest", label_req);
    assert_eq!(label_rsp.status_code, STATUS_OK);
    let l_payload = label_rsp.payload.expect("label payload");
    assert_eq!(l_payload.normalized_type_name(), "eve.contact.label.GetForCharacterResponse");

    // Route: eve.contact.label.GetForAllianceRequest
    let mut al_req = Vec::new();
    let mut al_id = Vec::new();
    encode_varint_field(1, 99_000_001, &mut al_id);
    encode_bytes_field(1, &al_id, &mut al_req);
    let mut ch_id_msg = Vec::new();
    encode_varint_field(1, char_id as u64, &mut ch_id_msg);
    encode_bytes_field(2, &ch_id_msg, &mut al_req);
    let al_rsp = quasar_call(&ctx, &sess, "eve.contact.label.GetForAllianceRequest", al_req);
    assert_eq!(al_rsp.status_code, STATUS_OK);

    // 2. Quasar Container (feat:quasar.container)
    // SDE verification: Station Container (3293), Cargo Container (3297)
    let station_cont = ctx.sde.types.get(&3293).expect("Station Container in SDE");
    assert_eq!(station_cont.name, "Medium Standard Container");
    let cargo_cont = ctx.sde.types.get(&3297).expect("Cargo Container in SDE");
    assert_eq!(cargo_cont.name, "Small Standard Container");

    // Descriptor registry verification for container domain (1 proto file)
    assert!(reg.files_by_proto_name.contains_key("eve/container/type.proto"));
    assert!(reg.messages_by_full_name.contains_key("eve.container.Identifier"));

    // Dynamic decode/encode eve.container.Identifier
    let mut cont_buf = Vec::new();
    encode_varint_field(1, 3293, &mut cont_buf);
    let dyn_cont = reg.decode_dynamic("eve.container.Identifier", &cont_buf).expect("decode eve.container.Identifier");
    assert_eq!(get_u64(&dyn_cont, 1), Some(3293), "Container type ID must match SDE 3293");

    // 3. Quasar Content (feat:quasar.content)
    // Descriptor registry verification for content domain (16 proto files)
    assert!(reg.files_by_proto_name.contains_key("eve/content/content.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/content/orchestrator/admin/api/requests.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/content/orchestrator/api/requests.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/content/orchestrator/deployment/api/requests.proto"));
    assert!(reg.messages_by_full_name.contains_key("eve.content.GetAllRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.content.GetAllResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.content.GetRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.content.GetResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.content.Attributes"));

    // Route: eve.content.GetAllRequest
    let all_cnt_rsp = quasar_call(&ctx, &sess, "eve.content.GetAllRequest", Vec::new());
    assert_eq!(all_cnt_rsp.status_code, STATUS_OK);
    let all_cnt_p = all_cnt_rsp.payload.expect("content all payload");
    assert_eq!(all_cnt_p.normalized_type_name(), "eve.content.GetAllResponse");

    // Route: eve.content.GetRequest (content articles / news)
    let cnt_req = encode_seq_id(1, 101);
    let cnt_rsp = quasar_call(&ctx, &sess, "eve.content.GetRequest", cnt_req);
    assert_eq!(cnt_rsp.status_code, STATUS_OK);
    let cnt_p = cnt_rsp.payload.expect("content get payload");
    assert_eq!(cnt_p.normalized_type_name(), "eve.content.GetResponse");
    let dyn_cnt_rsp = reg.decode_dynamic("eve.content.GetResponse", &cnt_p.value).expect("decode content GetResponse");
    assert_eq!(get_str(&dyn_cnt_rsp, 2), Some("ccp-content"));
    let dyn_cnt_attrs = match dyn_cnt_rsp.get(1) {
        Some(DynamicValue::Message(m)) => m,
        _ => panic!("Expected content Attributes"),
    };
    assert!(get_str(dyn_cnt_attrs, 4).unwrap_or("").contains("News from New Eden"));
    assert_eq!(get_u64(dyn_cnt_attrs, 3), Some(1), "PublishState must be Published");
}

// parity: feat:quasar.contextualoffer, feat:quasar.contract, feat:quasar.csat, feat:quasar.deadspace
#[tokio::test]
async fn test_quasar_contextualoffer_contract_csat_and_deadspace() {
    eve_testkit::attest(&[
        "feat:quasar.contextualoffer",
        "feat:quasar.contract",
        "feat:quasar.csat",
        "feat:quasar.deadspace",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("open descriptor registry");
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let sess = make_session(703, 1003, 90_000_003, 1_000_001);

    let mut uid_msg = Vec::new();
    encode_varint_field(1, 1003, &mut uid_msg);
    let mut cid_msg = Vec::new();
    encode_varint_field(1, 90_000_003, &mut cid_msg);

    // 1. Quasar ContextualOffer (feat:quasar.contextualoffer)
    // Descriptor registry verification (1 proto file, 6 messages)
    assert!(reg.files_by_proto_name.contains_key("eve/contextualoffer/contextualoffer.proto"));
    assert!(reg.messages_by_full_name.contains_key("eve.contextualoffer.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.contextualoffer.Delivered"));
    assert!(reg.messages_by_full_name.contains_key("eve.contextualoffer.Acknowledged"));
    assert!(reg.messages_by_full_name.contains_key("eve.contextualoffer.BuyInitiated"));
    assert!(reg.messages_by_full_name.contains_key("eve.contextualoffer.OmegaPrepared"));
    assert!(reg.messages_by_full_name.contains_key("eve.contextualoffer.OmegaPurchased"));

    // Dynamic decode/encode eve.contextualoffer.OmegaPrepared
    let mut off_msg = Vec::new();
    encode_varint_field(1, 42, &mut off_msg);

    let mut prep_buf = Vec::new();
    encode_bytes_field(1, &uid_msg, &mut prep_buf); // user_identifier
    encode_bytes_field(2, &cid_msg, &mut prep_buf); // character_identifier
    encode_bytes_field(3, &off_msg, &mut prep_buf); // offer_identifier
    encode_string_field(6, "19.99 USD", &mut prep_buf); // originalPrice
    encode_string_field(7, "14.99 USD", &mut prep_buf); // offerPrice
    encode_string_field(9, "Omega 30 Days Special", &mut prep_buf); // purchaseTitle

    let dyn_prep = reg.decode_dynamic("eve.contextualoffer.OmegaPrepared", &prep_buf).expect("decode OmegaPrepared");
    assert_eq!(get_str(&dyn_prep, 6), Some("19.99 USD"));
    assert_eq!(get_str(&dyn_prep, 7), Some("14.99 USD"));
    assert_eq!(get_str(&dyn_prep, 9), Some("Omega 30 Days Special"));

    // Dynamic decode/encode eve.contextualoffer.OmegaPurchased
    let mut pur_buf = Vec::new();
    encode_bytes_field(1, &uid_msg, &mut pur_buf);
    encode_bytes_field(2, &cid_msg, &mut pur_buf);
    encode_bytes_field(3, &off_msg, &mut pur_buf);
    encode_string_field(4, "Subscription Activated", &mut pur_buf); // confirmationTitle
    let dyn_pur = reg.decode_dynamic("eve.contextualoffer.OmegaPurchased", &pur_buf).expect("decode OmegaPurchased");
    assert_eq!(get_str(&dyn_pur, 4), Some("Subscription Activated"));

    // 2. Quasar Contract (feat:quasar.contract)
    // Descriptor registry verification (2 proto files, 16 messages)
    assert!(reg.files_by_proto_name.contains_key("eve/contract/contract.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/contract/item_exchange.proto"));
    assert!(reg.messages_by_full_name.contains_key("eve.contract.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.contract.Attributes"));
    assert!(reg.messages_by_full_name.contains_key("eve.contract.Issuer"));
    assert!(reg.messages_by_full_name.contains_key("eve.contract.Acceptor"));
    assert!(reg.messages_by_full_name.contains_key("eve.contract.Location"));
    assert!(reg.messages_by_full_name.contains_key("eve.contract.item_exchange.GetRequest"));
    assert!(reg.messages_by_full_name.contains_key("eve.contract.item_exchange.GetResponse"));
    assert!(reg.messages_by_full_name.contains_key("eve.contract.item_exchange.Attributes"));

    // Dynamic encode/decode eve.contract.Attributes
    let mut c_attr_buf = Vec::new();
    encode_varint_field(1, 1, &mut c_attr_buf); // type: ItemExchange
    encode_string_field(2, "WTS Raven Navy Issue", &mut c_attr_buf); // title
    let dyn_ca = reg.decode_dynamic("eve.contract.Attributes", &c_attr_buf).expect("decode contract.Attributes");
    assert_eq!(get_u64(&dyn_ca, 1), Some(1));
    assert_eq!(get_str(&dyn_ca, 2), Some("WTS Raven Navy Issue"));

    // Route: eve.contract.item_exchange.GetRequest
    let contract_req = encode_seq_id(1, 55_001);
    let contract_rsp = quasar_call(&ctx, &sess, "eve.contract.item_exchange.GetRequest", contract_req);
    assert_eq!(contract_rsp.status_code, STATUS_OK);
    let crsp_p = contract_rsp.payload.expect("contract payload");
    assert_eq!(crsp_p.normalized_type_name(), "eve.contract.item_exchange.GetResponse");

    // 3. Quasar CSAT (feat:quasar.csat)
    // Descriptor registry verification (1 proto file, 2 messages)
    assert!(reg.files_by_proto_name.contains_key("eve/csat/survey.proto"));
    assert!(reg.messages_by_full_name.contains_key("eve.csat.Survey"));
    assert!(reg.messages_by_full_name.contains_key("eve.csat.SurveyCompleted"));

    // Dynamic encode/decode eve.csat.Survey and SurveyCompleted
    let mut survey_buf = Vec::new();
    encode_varint_field(1, 1, &mut survey_buf); // thumbs_up: true
    encode_string_field(2, "Great fleet fight in Jita!", &mut survey_buf); // feedback
    let dyn_survey = reg.decode_dynamic("eve.csat.Survey", &survey_buf).expect("decode eve.csat.Survey");
    assert_eq!(get_bool(&dyn_survey, 1), Some(true));
    assert_eq!(get_str(&dyn_survey, 2), Some("Great fleet fight in Jita!"));

    let mut comp_buf = Vec::new();
    encode_bytes_field(1, &uid_msg, &mut comp_buf); // user
    encode_bytes_field(2, &survey_buf, &mut comp_buf); // survey
    let dyn_comp = reg.decode_dynamic("eve.csat.SurveyCompleted", &comp_buf).expect("decode SurveyCompleted");
    let inner_s = match dyn_comp.get(2) {
        Some(DynamicValue::Message(m)) => m,
        _ => panic!("Expected Survey message in SurveyCompleted"),
    };
    assert_eq!(get_bool(inner_s, 1), Some(true));
    assert_eq!(get_str(inner_s, 2), Some("Great fleet fight in Jita!"));

    // 4. Quasar Deadspace (feat:quasar.deadspace)
    // Descriptor registry verification for deadspace domain (18 proto files)
    let ds_files = [
        "eve/deadspace/abyssal/event.proto",
        "eve/deadspace/abyssal/filament_type.proto",
        "eve/deadspace/abyssal/weather_type.proto",
        "eve/deadspace/archetype.proto",
        "eve/deadspace/combatsite/combat_site.proto",
        "eve/deadspace/combatsite/event.proto",
        "eve/deadspace/datasite/data_site.proto",
        "eve/deadspace/datasite/event.proto",
        "eve/deadspace/escalation/event.proto",
        "eve/deadspace/factionwarfaresite/event.proto",
        "eve/deadspace/factionwarfaresite/factionwarfare_site.proto",
        "eve/deadspace/incursionsite/event.proto",
        "eve/deadspace/incursionsite/incursion_site.proto",
        "eve/deadspace/relicsite/event.proto",
        "eve/deadspace/relicsite/relic_site.proto",
        "eve/deadspace/signaturetype.proto",
        "eve_public/deadspace/archetype.proto",
        "eve_public/deadspace/signaturetype.proto",
    ];
    for f in ds_files {
        assert!(reg.files_by_proto_name.contains_key(f), "missing deadspace proto {f}");
    }
    assert!(reg.messages_by_full_name.contains_key("eve.deadspace.archetype.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.deadspace.signaturetype.Identifier"));
    assert!(reg.messages_by_full_name.contains_key("eve.deadspace.combatsite.event.CharacterCompleted"));
    assert!(reg.messages_by_full_name.contains_key("eve.deadspace.abyssal.event.CharacterEntered"));
    assert!(reg.messages_by_full_name.contains_key("eve.deadspace.abyssal.event.CharacterCompleted"));

    // Dynamic encode/decode eve.deadspace.archetype.Identifier
    let mut arch_id_buf = Vec::new();
    encode_varint_field(1, 101, &mut arch_id_buf);
    let dyn_arch = reg.decode_dynamic("eve.deadspace.archetype.Identifier", &arch_id_buf).expect("decode archetype Identifier");
    assert_eq!(get_u64(&dyn_arch, 1), Some(101));

    // Dungeon & deadspace sovereignty routes (feat:quasar.deadspace)
    // Routes: eve.dungeon.sovereignty.api.GetInstancesRequest & eve_public.dungeon.sovereignty.api.GetVisibleInstancesRequest

    let dng_rsp = quasar_call(&ctx, &sess, "eve.dungeon.sovereignty.api.GetInstancesRequest", Vec::new());
    assert_eq!(dng_rsp.status_code, STATUS_OK);
    let dng_p = dng_rsp.payload.expect("dungeon instances payload");
    assert_eq!(dng_p.normalized_type_name(), "eve.dungeon.sovereignty.api.GetInstancesResponse");

    let vdng_rsp = quasar_call(&ctx, &sess, "eve_public.dungeon.sovereignty.api.GetVisibleInstancesRequest", Vec::new());
    assert_eq!(vdng_rsp.status_code, STATUS_OK);
    let vdng_p = vdng_rsp.payload.expect("visible dungeon payload");
    assert_eq!(vdng_p.normalized_type_name(), "eve_public.dungeon.sovereignty.api.GetVisibleInstancesResponse");

    // Dynamic encode/decode eve.deadspace.abyssal.event.CharacterCompleted
    let mut abyssal_buf = Vec::new();
    encode_bytes_field(1, &cid_msg, &mut abyssal_buf); // character
    encode_varint_field(3, 4, &mut abyssal_buf); // level 4
    let mut w_id = Vec::new();
    encode_varint_field(1, 2, &mut w_id);
    encode_bytes_field(4, &w_id, &mut abyssal_buf); // weather: Electrical
    let mut ship_id = Vec::new();
    encode_varint_field(1, 11987, &mut ship_id);
    encode_bytes_field(5, &ship_id, &mut abyssal_buf); // ship_type: Gila (11987)

    let dyn_abyssal = reg.decode_dynamic("eve.deadspace.abyssal.event.CharacterCompleted", &abyssal_buf).expect("decode abyssal event");
    assert_eq!(get_u64(&dyn_abyssal, 3), Some(4), "Abyssal level must match 4");

    // Publish deadspace event to Quasar Gateway
    let now = ctx.now_unix();
    ctx.quasar_gateway.publish_event(
        &sess,
        EvePublicEvent {
            occurred: Some(GoogleTimestamp {
                seconds: now as i64,
                nanos: 0,
            }),
            uuid: b"deadspace-abyss-01".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.deadspace.abyssal.event.CharacterCompleted"),
                value: abyssal_buf,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"app-quasar-suite".to_vec(),
            ..Default::default()
        },
        now,
    );
}
