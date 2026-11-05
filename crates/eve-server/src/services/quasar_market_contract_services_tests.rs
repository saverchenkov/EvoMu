//! Unit and mutation-killing tests for `quasar_market_contract_services` (`feat:cosmetics.skinr`, `feat:quasar.cdn`, `feat:quasar.client`, `feat:quasar.contact`, `feat:quasar.content`).

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use crate::services::quasar_cosmetics_drafts::{id_to_uuid_bytes, SkinrDraftRecord};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext, GoogleAny,
    QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
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
    // parity: feat:cosmetics.skinr
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
    let mut only_char = Vec::new();
    encode_varint_field(1, owner_cid as u64, &mut only_char);
    assert_eq!(
        router
            .dispatch(&buyer_ctx, &GoogleAny { type_url: shared_type.clone(), value: only_char.clone() })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    // Short UUID (not 16 bytes) -> BAD_REQUEST
    let mut short_uuid = only_char.clone();
    let mut bad_id = Vec::new();
    encode_bytes_field(1, &[1, 2, 3], &mut bad_id);
    encode_bytes_field(2, &bad_id, &mut short_uuid);
    assert_eq!(
        router
            .dispatch(&buyer_ctx, &GoogleAny { type_url: shared_type.clone(), value: short_uuid })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    // Unknown draft UUID -> NOT_FOUND
    let mut nf_req = only_char.clone();
    let mut nf_id = Vec::new();
    encode_bytes_field(1, &[0x99u8; 16], &mut nf_id);
    encode_bytes_field(2, &nf_id, &mut nf_req);
    assert_eq!(
        router
            .dispatch(&buyer_ctx, &GoogleAny { type_url: shared_type.clone(), value: nf_req })
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    // Valid draft UUID -> GetSharedResponse
    let mut ok_shared = only_char;
    let mut ok_id = Vec::new();
    encode_bytes_field(1, &draft_uuid, &mut ok_id);
    encode_bytes_field(2, &ok_id, &mut ok_shared);
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

    let mut tx_id_msg = Vec::new();
    encode_bytes_field(1, &tx_uuid, &mut tx_id_msg);
    let mut tx_req = Vec::new();
    encode_bytes_field(1, &tx_id_msg, &mut tx_req);

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
        let mut b = Vec::new();
        encode_string_field(1, u, &mut b);
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
