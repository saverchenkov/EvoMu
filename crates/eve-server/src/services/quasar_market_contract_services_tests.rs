//! Unit and mutation-killing tests for `quasar_market_contract_services` (`feat:quasar.cosmetic`, `feat:quasar.cdn`, `feat:quasar.client`, `feat:quasar.contact`, `feat:quasar.content`).

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use crate::services::quasar_cosmetics_drafts::{id_to_uuid_bytes, SkinrDraftRecord};
use eve_db::WorldRepository;
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, GoogleAny, QuasarDescriptorRegistry, WireField, WireMessage, WireValue,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

const ROLE_PROGRAMMER: u64 = eve_machonet::service_roles::ROLE_GM_MASK;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

trait TestRouterExt {
    fn dispatch(
        &self,
        ctx: &AuthoritativeContext,
        req: &GoogleAny,
    ) -> Result<GoogleAny, (u32, String)>;
}
impl TestRouterExt for PublicGatewayRouter {
    fn dispatch(
        &self,
        ctx: &AuthoritativeContext,
        req: &GoogleAny,
    ) -> Result<GoogleAny, (u32, String)> {
        let norm = req.normalized_type_name();
        let handler = self
            .handlers
            .read()
            .unwrap()
            .get(norm)
            .cloned()
            .expect("registered handler");
        let dyn_msg = self
            .descriptors
            .decode_dynamic_or_wire(norm, &req.value)
            .unwrap_or_else(|_| {
                crate::services::quasar_char_skill_services::decode_wire_dynamic(norm, &req.value)
            });
        handler(ctx, req, &dyn_msg)
    }
}

fn make_router_and_db() -> (Arc<PublicGatewayRouter>, Arc<WorldRepository>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let router = Arc::new(PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    ));
    (router, db)
}

fn make_ctx(char_id: Option<u32>, role: u64) -> AuthoritativeContext {
    AuthoritativeContext::for_session_at(
        Some(1),
        char_id,
        None,
        0,
        "tranquility".to_string(),
        b"evomu-quasar-gw1".to_vec(),
        1_760_000_000,
    )
    .with_service_roles(role)
}

#[test]
fn draft_get_shared_and_cosmetic_market_transactions() {
    // parity: feat:quasar.cosmetic
    assert_eq!(hex_uuid(&[0x01, 0xab, 0xff]), "01abff");

    for det in [
        CosmeticTransactionDetails::PurchasingComponent,
        CosmeticTransactionDetails::PurchasingSkin,
        CosmeticTransactionDetails::CreatingSkinListing,
    ] {
        let tx = CosmeticTransactionRecord {
            uuid: [7u8; 16],
            buyer_id: Some(100),
            seller_id: Some(200),
            last_updated_unix: 1_760_000_000,
            status: 1,
            details: det,
        };
        assert!(!encode_transaction_attributes(&tx).is_empty());
    }

    let (router, db) = make_router_and_db();
    let owner_cid = 211200500u32;
    let buyer_cid = 211200501u32;
    let stranger_cid = 211200502u32;

    // Seed a saved draft for owner_cid
    let draft_id = 42u64;
    let draft_uuid = id_to_uuid_bytes(draft_id);
    {
        let state = QuasarCosmeticsState::from_db(Arc::clone(&db));
        let mut data = state.data.write().unwrap();
        data.saved_drafts.entry(owner_cid).or_default().insert(
            draft_id,
            SkinrDraftRecord {
                draft_id,
                uuid: draft_uuid.to_vec(),
                name: "SharedHalcyon".to_string(),
                line_name: "Halcyon".to_string(),
                design_slot_id: 1,
                pattern_id: 2,
                material_ids: vec![3, 4],
                updated_at_unix: 1_760_000_000,
                editor_cfg: vec![],
                ship_type_id: 587,
            },
        );
        drop(data);
        state.sync_to_db();
    }

    // Seed a cosmetic market transaction
    let tx_uuid = [0x55u8; 16];
    let tx_hex = hex_uuid(&tx_uuid);
    db.try_with_mut_snapshot(|snap| {
        let mut mkt = CosmeticsMarketData::default();
        mkt.transactions.insert(
            tx_hex.clone(),
            CosmeticTransactionRecord {
                uuid: tx_uuid,
                buyer_id: Some(buyer_cid),
                seller_id: Some(owner_cid),
                last_updated_unix: 1_760_000_000,
                status: 2,
                details: CosmeticTransactionDetails::PurchasingSkin,
            },
        );
        snap.quasar_state_blobs.insert(
            "quasar_cosmetics_market".to_string(),
            serde_json::to_string(&mkt).unwrap(),
        );
        Ok::<(), String>(())
    })
    .unwrap();

    let anon = make_ctx(None, 0);
    let owner_ctx = make_ctx(Some(owner_cid), 0);
    let buyer_ctx = make_ctx(Some(buyer_cid), 0);
    let stranger_ctx = make_ctx(Some(stranger_cid), 0);
    let admin_ctx = make_ctx(Some(stranger_cid), ROLE_PROGRAMMER);

    // 1. GetSharedRequest
    let shared_type = format!(
        "{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest"
    );
    assert_eq!(
        router
            .dispatch(&anon, &GoogleAny { type_url: shared_type.clone(), value: vec![] })
            .unwrap_err()
            .0,
        STATUS_UNAUTHORIZED
    );
    // Missing char id -> BAD_REQUEST
    assert_eq!(
        router
            .dispatch(&buyer_ctx, &GoogleAny { type_url: shared_type.clone(), value: vec![] })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    // Char ID present, missing UUID -> BAD_REQUEST
    let only_char = pb::message("eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest")
        .set("character.sequential", owner_cid)
        .must_encode();
    assert_eq!(
        router
            .dispatch(&buyer_ctx, &GoogleAny { type_url: shared_type.clone(), value: only_char })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    // Short UUID (not 16 bytes) -> BAD_REQUEST
    let short_uuid = pb::message("eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest")
        .set("character.sequential", owner_cid)
        .set("draft.uuid", vec![1u8, 2, 3])
        .must_encode();
    assert_eq!(
        router
            .dispatch(&buyer_ctx, &GoogleAny { type_url: shared_type.clone(), value: short_uuid })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    // Unknown draft UUID -> NOT_FOUND
    let nf_req = pb::message("eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest")
        .set("character.sequential", owner_cid)
        .set("draft.uuid", vec![0x99u8; 16])
        .must_encode();
    assert_eq!(
        router
            .dispatch(&buyer_ctx, &GoogleAny { type_url: shared_type.clone(), value: nf_req })
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    // Valid draft UUID -> GetSharedResponse
    let ok_shared = pb::message("eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest")
        .set("character.sequential", owner_cid)
        .set("draft.uuid", draft_uuid.to_vec())
        .must_encode();
    let shared_resp = router
        .dispatch(&stranger_ctx, &GoogleAny { type_url: shared_type, value: ok_shared })
        .expect("GetSharedRequest");
    assert!(!shared_resp.value.is_empty());

    // 2. transaction.api.GetRequest & 3. GetAllRequest
    let tx_get_type =
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.market.transaction.api.GetRequest");
    let tx_all_type =
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.market.transaction.api.GetAllRequest");
    assert_eq!(
        router
            .dispatch(&anon, &GoogleAny { type_url: tx_get_type.clone(), value: vec![] })
            .unwrap_err()
            .0,
        STATUS_UNAUTHORIZED
    );
    assert_eq!(
        router
            .dispatch(&buyer_ctx, &GoogleAny { type_url: tx_get_type.clone(), value: vec![] })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    let tx_req = pb::message("eve_public.cosmetic.market.transaction.api.GetRequest")
        .set("transaction.uuid", tx_uuid.to_vec())
        .must_encode();

    // Stranger -> FORBIDDEN, Admin -> OK, Buyer -> OK, Seller -> OK
    assert_eq!(
        router
            .dispatch(&stranger_ctx, &GoogleAny { type_url: tx_get_type.clone(), value: tx_req.clone() })
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert!(router
        .dispatch(&admin_ctx, &GoogleAny { type_url: tx_get_type.clone(), value: tx_req.clone() })
        .is_ok());
    assert!(router
        .dispatch(&buyer_ctx, &GoogleAny { type_url: tx_get_type.clone(), value: tx_req.clone() })
        .is_ok());
    assert!(router
        .dispatch(&owner_ctx, &GoogleAny { type_url: tx_get_type, value: tx_req })
        .is_ok());

    let all_buyer = router
        .dispatch(&buyer_ctx, &GoogleAny { type_url: tx_all_type.clone(), value: vec![] })
        .expect("GetAll buyer");
    assert!(!all_buyer.value.is_empty());
    let all_stranger = router
        .dispatch(&stranger_ctx, &GoogleAny { type_url: tx_all_type, value: vec![] })
        .expect("GetAll stranger");
    assert!(all_stranger.value.is_empty());
}

#[test]
fn cdn_client_host_contacts_and_content_handlers() {
    // parity: feat:quasar.cdn
    let (router, db) = make_router_and_db();
    let cid = 211200510i64;
    db.with_mut_snapshot(|s| {
        let mut ch = s.characters[&90_000_001].clone();
        ch.character_id = cid;
        ch.standings.insert(211200599, 5.0);
        s.characters.insert(cid, ch);
    });

    let user = make_ctx(Some(cid as u32), 0);

    // 4. CDN GetActiveURLSigningKeyRequest & SignURLRequest
    let key_resp = router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.cdn.api.GetActiveURLSigningKeyRequest"),
                value: vec![],
            },
        )
        .unwrap();
    assert_eq!(
        key_resp.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve.cdn.api.GetActiveURLSigningKeyResponse")
    );
    assert!(!key_resp.value.is_empty());

    let sign_type = format!("{TYPE_GOOGLEAPIS_PREFIX}eve.cdn.api.SignURLRequest");
    // Default URL fallback
    let def_signed = router
        .dispatch(&user, &GoogleAny { type_url: sign_type.clone(), value: vec![] })
        .unwrap();
    assert_eq!(
        def_signed.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve.cdn.api.SignURLResponse")
    );
    assert!(String::from_utf8_lossy(&def_signed.value).contains("?sig="));
    // URL without '?' and URL with '?'
    for (u, expected_sep) in [
        ("https://cdn.evomu.org/a", "?sig="),
        ("https://cdn.evomu.org/a?v=1", "&sig="),
    ] {
        let b = pb::message("eve.cdn.api.SignURLRequest")
            .set("url", u)
            .must_encode();
        let r = router
            .dispatch(&user, &GoogleAny { type_url: sign_type.clone(), value: b })
            .unwrap();
        let text = String::from_utf8_lossy(&r.value);
        assert!(text.contains(expected_sep));
    }

    // 5. Client host & DisconnectRequest
    let host_resp = router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.client.host.GetRequest"),
                value: vec![],
            },
        )
        .unwrap();
    assert_eq!(
        host_resp.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve.client.host.GetResponse")
    );
    let disc_resp = router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}DisconnectRequest"),
                value: vec![],
            },
        )
        .unwrap();
    assert_eq!(disc_resp.value, Vec::<u8>::new());

    // 6. Contacts & 7. Content
    for rpc in [
        "eve.contacts.GetAllForCharacterRequest",
        "eve.contact.label.GetForAllianceRequest",
        "eve.content.GetAllRequest",
        "eve.content.GetRequest",
    ] {
        let resp = router
            .dispatch(
                &user,
                &GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rpc}"),
                    value: vec![],
                },
            )
            .expect(rpc);
        assert!(!resp.value.is_empty(), "{rpc} should return non-empty payload");
    }
}

#[test]
fn market_contract_services_edge_branches_decoding_and_mutants() {
    let (router, db) = make_router_and_db();
    register_handlers(&router, Arc::clone(&db), shared_sde());
    let anon = make_ctx(None, 0);
    let zero_char = make_ctx(Some(0), 0);
    let buyer_ctx = make_ctx(Some(900101), 0);
    let seller_ctx = make_ctx(Some(900102), 0);
    let outsider_ctx = make_ctx(Some(900103), 0);
    let admin_ctx = make_ctx(Some(900199), ROLE_PROGRAMMER);

    let dispatch_raw = |ctx: &AuthoritativeContext, rpc: &str, value: Vec<u8>, dyn_msg: DynamicQuasarMessage| {
        let handler = router.handlers.read().unwrap().get(rpc).cloned().expect(rpc);
        let req = GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rpc}"),
            value,
        };
        handler(ctx, &req, &dyn_msg)
    };

    // --- Helper decoders & hex_uuid ---
    assert_eq!(hex_uuid(&[0x00, 0xab, 0xff]), "00abff");
    assert_eq!(
        require_character(&anon).unwrap_err(),
        (STATUS_UNAUTHORIZED, "Authenticated character required".to_string())
    );
    assert_eq!(require_character(&buyer_ctx).unwrap(), 900101);

    let u64_wrap = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(
                WireMessage {
                    fields: vec![WireField {
                        field_number: 1,
                        value: WireValue::Varint(777),
                    }],
                }
                .encode(),
            ),
        }],
    }
    .encode();
    assert_eq!(extract_uint64_field(&u64_wrap, 1), Some(777));
    assert_eq!(extract_uint64_field(&[0xff, 0xff], 1), None);

    let u32_overflow = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::Varint(u64::from(u32::MAX) + 1),
        }],
    }
    .encode();
    assert_eq!(extract_uint32_field(&DynamicQuasarMessage::new("test"), &u32_overflow, 1), None);

    // extract_uuid_field across Message, 16B Bytes, wrapped Bytes, and WireMessage fallbacks
    let raw_uuid = vec![0x11; 16];
    let mut sub_dyn = DynamicQuasarMessage::new("test");
    sub_dyn.fields.insert(1, DynamicValue::Bytes(raw_uuid.clone()));
    let mut dyn_msg_obj = DynamicQuasarMessage::new("test");
    dyn_msg_obj.fields.insert(2, DynamicValue::Message(Box::new(sub_dyn)));
    assert_eq!(extract_uuid_field(&dyn_msg_obj, &[], 2), Some(raw_uuid.clone()));

    let mut dyn_bytes_direct = DynamicQuasarMessage::new("test");
    dyn_bytes_direct.fields.insert(2, DynamicValue::Bytes(raw_uuid.clone()));
    assert_eq!(extract_uuid_field(&dyn_bytes_direct, &[], 2), Some(raw_uuid.clone()));

    let wrapped_short = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(b"short".to_vec()),
        }],
    }
    .encode();
    let mut dyn_bytes_wrapped = DynamicQuasarMessage::new("test");
    dyn_bytes_wrapped.fields.insert(2, DynamicValue::Bytes(wrapped_short.clone()));
    assert_eq!(extract_uuid_field(&dyn_bytes_wrapped, &[], 2), Some(b"short".to_vec()));

    let wire_direct_16 = WireMessage {
        fields: vec![WireField {
            field_number: 2,
            value: WireValue::LengthDelimited(raw_uuid.clone()),
        }],
    }
    .encode();
    assert_eq!(extract_uuid_field(&DynamicQuasarMessage::new("test"), &wire_direct_16, 2), Some(raw_uuid.clone()));

    let wire_wrapped_16 = WireMessage {
        fields: vec![WireField {
            field_number: 2,
            value: WireValue::LengthDelimited(
                WireMessage {
                    fields: vec![WireField {
                        field_number: 1,
                        value: WireValue::LengthDelimited(raw_uuid.clone()),
                    }],
                }
                .encode(),
            ),
        }],
    }
    .encode();
    assert_eq!(extract_uuid_field(&DynamicQuasarMessage::new("test"), &wire_wrapped_16, 2), Some(raw_uuid.clone()));
    assert_eq!(extract_uuid_field(&DynamicQuasarMessage::new("test"), &[0xff, 0xff], 2), None);

    // --- 1. GetSharedRequest: auth, char_id == 0, uuid len != 16, UUID linear scan fallback, missing ---
    let rpc_draft = "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest";
    assert_eq!(
        dispatch_raw(&anon, rpc_draft, vec![], DynamicQuasarMessage::new("test")).unwrap_err().0,
        STATUS_UNAUTHORIZED
    );
    let bad_zero_char = pb::message(rpc_draft)
        .set("character.sequential", 0u32)
        .set("draft.uuid", raw_uuid)
        .must_encode();
    assert_eq!(
        dispatch_raw(&buyer_ctx, rpc_draft, bad_zero_char, DynamicQuasarMessage::new("test")).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    let valid_char_only = pb::message(rpc_draft)
        .set("character.sequential", 900101u32)
        .must_encode();
    assert_eq!(
        dispatch_raw(&buyer_ctx, rpc_draft, valid_char_only, DynamicQuasarMessage::new("test")).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    let bad_uuid_len = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::Varint(900101) },
            WireField { field_number: 2, value: WireValue::LengthDelimited(wrapped_short.clone()) },
        ],
    }
    .encode();
    let err_len = dispatch_raw(&buyer_ctx, rpc_draft, bad_uuid_len, DynamicQuasarMessage::new("test")).unwrap_err();
    assert_eq!(err_len.0, STATUS_BAD_REQUEST);
    assert!(err_len.1.contains("16 bytes"));

    // Seed a saved draft keyed under draft_id=999 with custom UUID whose extract_id != 999 to force scan fallback
    let custom_draft_uuid: [u8; 16] = [0xAA; 16];
    {
        let st = QuasarCosmeticsState::from_db(Arc::clone(&db));
        {
            let mut data = st.data.write().unwrap();
            let draft = SkinrDraftRecord {
                draft_id: 999,
                uuid: custom_draft_uuid.to_vec(),
                name: "SharedFallback".to_string(),
                line_name: "LineA".to_string(),
                design_slot_id: 1,
                pattern_id: 2,
                material_ids: vec![3],
                updated_at_unix: 1_760_000_100,
                editor_cfg: Vec::new(),
                ship_type_id: 587,
            };
            data.saved_drafts.entry(900101).or_default().insert(999, draft);
        }
        st.sync_to_db();
    }
    let scan_req = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::Varint(900101) },
            WireField { field_number: 2, value: WireValue::LengthDelimited(custom_draft_uuid.to_vec()) },
        ],
    }
    .encode();
    let scan_resp = dispatch_raw(&outsider_ctx, rpc_draft, scan_req, DynamicQuasarMessage::new("test"))
        .expect("custom uuid scan fallback");
    assert_eq!(
        scan_resp.type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedResponse")
    );
    let wm_draft = WireMessage::decode(&scan_resp.value).unwrap();
    assert!(!wm_draft.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap().is_empty());

    let missing_draft_req = pb::message(rpc_draft)
        .set("character.sequential", 900101u32)
        .set("draft.uuid", vec![0xBBu8; 16])
        .must_encode();
    assert_eq!(
        dispatch_raw(&outsider_ctx, rpc_draft, missing_draft_req, DynamicQuasarMessage::new("test")).unwrap_err().0,
        STATUS_NOT_FOUND
    );

    // --- 2 & 3. Cosmetic Market Transactions: corrupt JSON resilience, buyer/seller/neither ACL, all 3 detail variants ---
    let rpc_tx_get = "eve_public.cosmetic.market.transaction.api.GetRequest";
    let rpc_tx_all = "eve_public.cosmetic.market.transaction.api.GetAllRequest";

    // Unauthenticated & malformed UUID checks
    assert_eq!(
        dispatch_raw(&anon, rpc_tx_get, vec![], DynamicQuasarMessage::new("test")).unwrap_err().0,
        STATUS_UNAUTHORIZED
    );
    assert_eq!(
        dispatch_raw(&anon, rpc_tx_all, vec![], DynamicQuasarMessage::new("test")).unwrap_err().0,
        STATUS_UNAUTHORIZED
    );
    assert_eq!(
        dispatch_raw(&buyer_ctx, rpc_tx_get, vec![], DynamicQuasarMessage::new("test")).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    let tx_bad_len = WireMessage {
        fields: vec![WireField { field_number: 1, value: WireValue::LengthDelimited(wrapped_short) }],
    }
    .encode();
    assert_eq!(
        dispatch_raw(&buyer_ctx, rpc_tx_get, tx_bad_len, DynamicQuasarMessage::new("test")).unwrap_err().0,
        STATUS_BAD_REQUEST
    );

    // Corrupt market JSON blob -> graceful empty fallback (NOT_FOUND / empty list)
    db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert("quasar_cosmetics_market".to_string(), "{not-valid-json".to_string());
    });
    let tx_req_missing = pb::message(rpc_tx_get)
        .set("transaction.uuid", vec![0x55u8; 16])
        .must_encode();
    assert_eq!(
        dispatch_raw(&buyer_ctx, rpc_tx_get, tx_req_missing, DynamicQuasarMessage::new("test")).unwrap_err().0,
        STATUS_NOT_FOUND
    );
    let empty_all = dispatch_raw(&buyer_ctx, rpc_tx_all, vec![], DynamicQuasarMessage::new("test")).unwrap();
    assert!(empty_all.value.is_empty());

    // Populate transactions covering all 3 CosmeticTransactionDetails variants & buyer/seller/none ACL
    let uuid_comp = id_to_uuid_bytes(701);
    let uuid_skin = id_to_uuid_bytes(702);
    let uuid_list = id_to_uuid_bytes(703);
    db.with_mut_snapshot(|snap| {
        let mut market = super::super::quasar_cosmetics_market_types::CosmeticsMarketData::default();
        market.transactions.insert(
            hex_uuid(&uuid_comp),
            super::super::quasar_cosmetics_market_types::CosmeticTransactionRecord {
                uuid: uuid_comp,
                buyer_id: Some(900101),
                seller_id: None,
                status: 2,
                last_updated_unix: 1_760_000_011,
                details: super::super::quasar_cosmetics_market_types::CosmeticTransactionDetails::PurchasingComponent,
            },
        );
        market.transactions.insert(
            hex_uuid(&uuid_skin),
            super::super::quasar_cosmetics_market_types::CosmeticTransactionRecord {
                uuid: uuid_skin,
                buyer_id: None,
                seller_id: Some(900102),
                status: 3,
                last_updated_unix: 1_760_000_022,
                details: super::super::quasar_cosmetics_market_types::CosmeticTransactionDetails::PurchasingSkin,
            },
        );
        market.transactions.insert(
            hex_uuid(&uuid_list),
            super::super::quasar_cosmetics_market_types::CosmeticTransactionRecord {
                uuid: uuid_list,
                buyer_id: None,
                seller_id: None,
                status: 4,
                last_updated_unix: 1_760_000_033,
                details: super::super::quasar_cosmetics_market_types::CosmeticTransactionDetails::CreatingSkinListing,
            },
        );
        snap.quasar_state_blobs.insert(
            "quasar_cosmetics_market".to_string(),
            serde_json::to_string(&market).unwrap(),
        );
    });

    for (ctx_ok, ctx_deny, uuid, expected_status, expected_ts, expected_detail_field) in [
        (&buyer_ctx, &outsider_ctx, uuid_comp, 2u64, 1_760_000_011u64, 1u32),
        (&seller_ctx, &outsider_ctx, uuid_skin, 3u64, 1_760_000_022u64, 2u32),
        (&admin_ctx, &buyer_ctx, uuid_list, 4u64, 1_760_000_033u64, 3u32),
    ] {
        let req_bytes = WireMessage {
            fields: vec![WireField {
                field_number: 1,
                value: WireValue::LengthDelimited(uuid.to_vec()),
            }],
        }
        .encode();
        assert_eq!(
            dispatch_raw(ctx_deny, rpc_tx_get, req_bytes.clone(), DynamicQuasarMessage::new("test")).unwrap_err().0,
            STATUS_FORBIDDEN
        );
        let ok_resp = dispatch_raw(ctx_ok, rpc_tx_get, req_bytes, DynamicQuasarMessage::new("test")).unwrap();
        let top = WireMessage::decode(&ok_resp.value).unwrap();
        let attrs_raw = top.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
        let attrs = WireMessage::decode(attrs_raw).unwrap();
        let ts_raw = attrs.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
        let ts_msg = WireMessage::decode(ts_raw).unwrap();
        assert_eq!(ts_msg.fields[0].value.as_varint(), Some(expected_ts));
        assert_eq!(
            attrs.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_varint(),
            Some(expected_status)
        );
        let det_raw = attrs.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_bytes().unwrap();
        let det_msg = WireMessage::decode(det_raw).unwrap();
        assert_eq!(det_msg.fields[0].field_number, expected_detail_field);
    }

    let seller_all = dispatch_raw(&seller_ctx, rpc_tx_all, vec![], DynamicQuasarMessage::new("test")).unwrap();
    let seller_wm = WireMessage::decode(&seller_all.value).unwrap();
    assert_eq!(seller_wm.fields.len(), 1);
    let inner_id_msg = WireMessage::decode(seller_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(inner_id_msg.fields[0].value.as_bytes(), Some(uuid_skin.as_slice()));

    // --- 4. CDN Signing: WireMessage fallback + query string branching + exact field assertions ---
    let key_resp = dispatch_raw(&buyer_ctx, "eve.cdn.api.GetActiveURLSigningKeyRequest", vec![], DynamicQuasarMessage::new("test")).unwrap();
    let key_wm = WireMessage::decode(&key_resp.value).unwrap();
    assert_eq!(key_wm.fields[0].value.as_str().unwrap(), Some("evomu-cdn-v24"));

    let sign_wire_req = pb::message("eve.cdn.api.SignURLRequest")
        .set("url", "https://cdn.example/skin?v=2")
        .must_encode();
    let signed_query = dispatch_raw(&buyer_ctx, "eve.cdn.api.SignURLRequest", sign_wire_req, DynamicQuasarMessage::new("test")).unwrap();
    let sq_wm = WireMessage::decode(&signed_query.value).unwrap();
    assert_eq!(sq_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap(), Some("https://cdn.example/skin?v=2&sig=evomu_v24_signed"));
    let sq_ts = WireMessage::decode(sq_wm.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(sq_ts.fields[0].value.as_varint(), Some(1_800_000_000));
    assert_eq!(sq_wm.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_str().unwrap(), Some("evomu-cdn-v24"));

    let signed_default = dispatch_raw(&buyer_ctx, "eve.cdn.api.SignURLRequest", vec![], DynamicQuasarMessage::new("test")).unwrap();
    let sd_wm = WireMessage::decode(&signed_default.value).unwrap();
    assert_eq!(sd_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap(), Some("https://cdn.evomu.org/res/asset.fb?sig=evomu_v24_signed"));

    // --- 5. Client Host & Disconnect: exact decoded fields ---
    let host_resp = dispatch_raw(&buyer_ctx, "eve.client.host.GetRequest", vec![], DynamicQuasarMessage::new("test")).unwrap();
    let host_wm = WireMessage::decode(&host_resp.value).unwrap();
    let host_attrs = WireMessage::decode(host_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(host_attrs.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_varint(), Some(12345678));
    assert_eq!(host_attrs.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_str().unwrap(), Some("en_US"));
    assert_eq!(host_attrs.fields.iter().find(|f| f.field_number == 4).unwrap().value.as_varint(), Some(16384));
    assert_eq!(host_attrs.fields.iter().find(|f| f.field_number == 10).unwrap().value.as_str().unwrap(), Some("24.01.2024"));

    let disc = dispatch_raw(&buyer_ctx, "DisconnectRequest", vec![], DynamicQuasarMessage::new("test")).unwrap();
    assert_eq!(disc.type_url, format!("{TYPE_GOOGLEAPIS_PREFIX}google.protobuf.Empty"));
    assert!(disc.value.is_empty());

    // --- 6. Contacts: unauth/0-char gate + negative/rounded standings math ---
    assert_eq!(
        dispatch_raw(&anon, "eve.contacts.GetAllForCharacterRequest", vec![], DynamicQuasarMessage::new("test")).unwrap_err().0,
        STATUS_UNAUTHORIZED
    );
    assert_eq!(
        dispatch_raw(&zero_char, "eve.contacts.GetAllForCharacterRequest", vec![], DynamicQuasarMessage::new("test")).unwrap_err().0,
        STATUS_UNAUTHORIZED
    );
    db.with_mut_snapshot(|snap| {
        if let Some(mut ch) = snap.characters.get(&900101).cloned().or_else(|| snap.characters.values().next().cloned()) {
            ch.character_id = 900101;
            ch.standings.clear();
            ch.standings.insert(4001, -10.0);
            ch.standings.insert(4002, 5.6);
            snap.characters.insert(900101, ch);
        }
    });
    let contacts_resp = dispatch_raw(&buyer_ctx, "eve.contacts.GetAllForCharacterRequest", vec![], DynamicQuasarMessage::new("test")).unwrap();
    let contacts_wm = WireMessage::decode(&contacts_resp.value).unwrap();
    let mut standing_pairs = Vec::new();
    for f in &contacts_wm.fields {
        let personal = WireMessage::decode(f.value.as_bytes().unwrap()).unwrap();
        let ind = WireMessage::decode(personal.fields.iter().find(|x| x.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
        let cid_msg = WireMessage::decode(ind.fields.iter().find(|x| x.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
        let contact_id = cid_msg.fields[0].value.as_varint().unwrap();
        let rel_val = personal.fields.iter().find(|x| x.field_number == 4).unwrap().value.as_varint().unwrap();
        standing_pairs.push((contact_id, rel_val));
    }
    standing_pairs.sort();
    assert_eq!(standing_pairs, vec![(4001, 0), (4002, 16)]);

    let missing_char_contacts = dispatch_raw(&outsider_ctx, "eve.contacts.GetAllForCharacterRequest", vec![], DynamicQuasarMessage::new("test")).unwrap();
    assert!(missing_char_contacts.value.is_empty());

    let label_resp = dispatch_raw(&buyer_ctx, "eve.contact.label.GetForAllianceRequest", vec![], DynamicQuasarMessage::new("test")).unwrap();
    let label_wm = WireMessage::decode(&label_resp.value).unwrap();
    let inst = WireMessage::decode(label_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    let id_msg = WireMessage::decode(inst.fields.iter().find(|x| x.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(id_msg.fields[0].value.as_varint(), Some(1));
    let attr = WireMessage::decode(inst.fields.iter().find(|x| x.field_number == 2).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(attr.fields[0].value.as_str().unwrap(), Some("Alliance Contacts"));

    // --- 7. Content: exact decoded IDs + submessage-wrapped cid ---
    let all_c = dispatch_raw(&buyer_ctx, "eve.content.GetAllRequest", vec![], DynamicQuasarMessage::new("test")).unwrap();
    let all_wm = WireMessage::decode(&all_c.value).unwrap();
    let ids: Vec<u64> = all_wm.fields.iter().map(|f| {
        WireMessage::decode(f.value.as_bytes().unwrap()).unwrap().fields[0].value.as_varint().unwrap()
    }).collect();
    assert_eq!(ids, vec![1, 2, 3]);

    let content_req_wrap = pb::message("eve.content.GetRequest")
        .set("id.sequential", 42u32)
        .must_encode();
    let c_resp = dispatch_raw(&buyer_ctx, "eve.content.GetRequest", content_req_wrap, DynamicQuasarMessage::new("test")).unwrap();
    let c_wm = WireMessage::decode(&c_resp.value).unwrap();
    assert_eq!(c_wm.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_str().unwrap(), Some("ccp-content"));
    let c_attrs = WireMessage::decode(c_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(c_attrs.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_varint(), Some(1));
    assert_eq!(c_attrs.fields.iter().find(|f| f.field_number == 4).unwrap().value.as_str().unwrap(), Some("Article #42: News from New Eden"));
    let c_details = WireMessage::decode(c_attrs.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let c_op = WireMessage::decode(c_details.fields[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(c_op.fields[0].value.as_varint(), Some(100));
    let c_tags = WireMessage::decode(c_attrs.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(c_tags.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str().unwrap(), Some("news"));
    assert_eq!(c_tags.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_str().unwrap(), Some("community"));
}

#[test]
fn mutant_kill_register_handlers_and_contact_char_filter() {
    use eve_proto::quasar::WireMessage;

    let (router, db) = make_router_and_db();
    register_handlers(&router, Arc::clone(&db), shared_sde());
    let handlers = router.handlers.read().unwrap();
    for expected in [
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest",
        "eve.contacts.GetAllForCharacterRequest",
        "eve.contact.label.GetForAllianceRequest",
        "eve.content.GetAllRequest",
        "eve.content.GetRequest",
    ] {
        assert!(handlers.contains_key(expected), "missing registered handler {expected}");
    }
    drop(handlers);

    let cid = 90_500_101_u32;
    db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = i64::from(cid);
        ch.standings.clear();
        ch.standings.insert(4011, -5.0);
        snap.characters.insert(i64::from(cid), ch);
    });

    let req = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.contacts.GetAllForCharacterRequest"),
        value: vec![],
    };
    assert_eq!(router.dispatch(&make_ctx(None, 0), &req).unwrap_err().0, STATUS_UNAUTHORIZED);
    assert_eq!(router.dispatch(&make_ctx(Some(0), 0), &req).unwrap_err().0, STATUS_UNAUTHORIZED);

    let resp = router.dispatch(&make_ctx(Some(cid), 0), &req).unwrap();
    let wm = WireMessage::decode(&resp.value).unwrap();
    assert_eq!(wm.fields.len(), 1);
    let personal = WireMessage::decode(wm.fields[0].value.as_bytes().unwrap()).unwrap();
    let ind = WireMessage::decode(personal.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let id_msg = WireMessage::decode(ind.fields[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(id_msg.fields[0].value.as_varint(), Some(4011));
    assert_eq!(
        personal.fields.iter().find(|f| f.field_number == 4).unwrap().value.as_varint(),
        Some(5)
    );
}
