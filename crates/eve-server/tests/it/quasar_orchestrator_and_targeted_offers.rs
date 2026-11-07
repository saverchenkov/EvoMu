//! Integration tests for Batch 29 Quasar Public Gateway services.

use eve_db::{AccountRecord, CharacterRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
};
use eve_server::router::ServerContext;
use eve_server::services::quasar_corp_structure_services::{
    ContentGroupRecord, ContentTypeRecord, DeploymentInstanceRecord, DeploymentRecord,
    FailedFulfillmentRecord, OrchestratorState, TargetedOfferAdminRecord,
};
use eve_server::services::quasar_cosmetics_market_types::{
    CosmeticsMarketData, ListingTargetRecord, SkinListingRecord,
};
use eve_server::services::quasar_payment_store_types::get_plex_transactions;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};

static CORR_SEQ: AtomicU64 = AtomicU64::new(1);

fn make_session(sid: u64, uid: u64, cid: i64, role: u64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(30_000_142);
    sess.role = role;
    sess
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload_bytes: Vec<u8>,
) -> EvePublicResponse {
    let seq = CORR_SEQ.fetch_add(1, Ordering::Relaxed);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: format!("b29-quasar-{seq:05}").into_bytes(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        ..Default::default()
    };
    let rep = ctx
        .dispatch_rpc(sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None)
        .await
        .expect("publicGatewaySvc.SendRequest");
    extract_response_proto(&rep)
}

fn decode_dyn(ctx: &ServerContext, type_name: &str, bytes: &[u8]) -> DynamicQuasarMessage {
    ctx.quasar_gateway
        .descriptors
        .decode_dynamic(type_name, bytes)
        .unwrap_or_else(|e| panic!("failed to decode {type_name}: {e:?}"))
}

fn seq_bytes(id: u64) -> Vec<u8> {
    let mut sub = Vec::new();
    encode_varint_field(1, id, &mut sub);
    let mut out = Vec::new();
    encode_bytes_field(1, &sub, &mut out);
    out
}

fn sint32_user_bytes(id: i32) -> Vec<u8> {
    let zz = ((id << 1) ^ (id >> 31)) as u32;
    let mut sub = Vec::new();
    encode_varint_field(1, zz as u64, &mut sub);
    let mut out = Vec::new();
    encode_bytes_field(1, &sub, &mut out);
    out
}

fn uuid_bytes(uuid: &[u8]) -> Vec<u8> {
    let mut sub = Vec::new();
    encode_bytes_field(1, uuid, &mut sub);
    let mut out = Vec::new();
    encode_bytes_field(1, &sub, &mut out);
    out
}

fn encode_semver(field_num: u32, major: u64, minor: u64, patch: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    if major > 0 { encode_varint_field(1, major, &mut sub); }
    if minor > 0 { encode_varint_field(2, minor, &mut sub); }
    if patch > 0 { encode_varint_field(3, patch, &mut sub); }
    encode_bytes_field(field_num, &sub, out);
}

fn seed_test_orchestrator(ctx: &ServerContext) {
    let mut st = OrchestratorState::default();
    st.deployments.insert(101, DeploymentRecord {
        id: 101,
        name: "pirate_insurgency_deployment".into(),
        short_description: "Pirate insurgency forward operating sites".into(),
        content_group_id: 201,
        enabled: true,
        current_content_count: 1,
        no_region_limit: true,
    });
    st.content_groups.insert(201, ContentGroupRecord {
        id: 201,
        name: "insurgency_sites".into(),
        content_type_id: 301,
        weight: 1.0,
    });
    st.content_types.insert(301, ContentTypeRecord {
        id: 301,
        name: "guristas_outpost".into(),
        short_description: "Guristas forward outpost".into(),
        dungeon_id: 8001,
    });
    st.instances.push(DeploymentInstanceRecord {
        uuid: (*b"TESTORCHINST0001").to_vec(),
        deployment_id: 101,
        content_type_id: 301,
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        created_unix: 1_700_000_000,
    });
    let json = serde_json::to_string(&st).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert("content_orchestrator_state".to_string(), json);
    });
}

#[tokio::test]
// parity: quasar:eveProto/generated/eve/content/orchestrator/admin/api/requests_pb2.pyj, quasar:eveProto/generated/eve/content/orchestrator/api/requests_pb2.pyj, quasar:eveProto/generated/eve/content/orchestrator/deployment/api/requests_pb2.pyj
async fn test_b29_orchestrator_version_and_deployment() {
    let ctx = ServerContext::new();
    seed_test_orchestrator(&ctx);
    let mut sess = make_session(1, 1001, 2112000001, eve_machonet::service_roles::ROLE_GM_DEFAULT);
    let mut player = make_session(10, 2001, 2112000099, eve_machonet::service_roles::ROLE_PLAYER_DEFAULT);

    // 1. SetDefaultTenantVersionRequest & GetDefaultTenantVersionRequest
    let mut set_def = Vec::new();
    encode_semver(1, 2, 3, 4, &mut set_def);
    assert_eq!(quasar_rpc(&ctx, &mut player, "eve.content.orchestrator.admin.api.SetDefaultTenantVersionRequest", set_def.clone()).await.status_code, STATUS_FORBIDDEN);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.admin.api.SetDefaultTenantVersionRequest", set_def).await.status_code, STATUS_OK);

    let r2 = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.api.GetDefaultTenantVersionRequest", Vec::new()).await;
    assert_eq!(r2.status_code, STATUS_OK);
    let dyn_ver = decode_dyn(&ctx, "eve.content.orchestrator.api.GetDefaultTenantVersionResponse", &r2.payload.unwrap().value);
    if let Some(DynamicValue::Message(v)) = dyn_ver.get(1) {
        assert_eq!((v.get(1), v.get(2), v.get(3)), (Some(&DynamicValue::Uint32(2)), Some(&DynamicValue::Uint32(3)), Some(&DynamicValue::Uint32(4))));
    } else {
        panic!("expected default version message");
    }

    // 2. SetTenantVersionRequest & GetTenantVersionRequest (override + reset to default)
    let mut set_ten = Vec::new();
    encode_semver(2, 3, 1, 2, &mut set_ten);
    assert_eq!(quasar_rpc(&ctx, &mut player, "eve.content.orchestrator.admin.api.SetTenantVersionRequest", set_ten.clone()).await.status_code, STATUS_FORBIDDEN);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.admin.api.SetTenantVersionRequest", set_ten).await.status_code, STATUS_OK);

    let r_ten = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.api.GetTenantVersionRequest", Vec::new()).await;
    assert_eq!(r_ten.status_code, STATUS_OK);
    let dyn_ten = decode_dyn(&ctx, "eve.content.orchestrator.api.GetTenantVersionResponse", &r_ten.payload.unwrap().value);
    if let Some(DynamicValue::Message(v)) = dyn_ten.get(1) {
        assert_eq!((v.get(1), v.get(2), v.get(3)), (Some(&DynamicValue::Uint32(3)), Some(&DynamicValue::Uint32(1)), Some(&DynamicValue::Uint32(2))));
    } else {
        panic!("expected tenant version message");
    }

    let mut reset_ten = Vec::new();
    encode_varint_field(1, 1, &mut reset_ten);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.admin.api.SetTenantVersionRequest", reset_ten).await.status_code, STATUS_OK);
    let r_ten_reset = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.api.GetTenantVersionRequest", Vec::new()).await;
    assert_eq!(r_ten_reset.status_code, STATUS_OK);
    let dyn_ten_reset = decode_dyn(&ctx, "eve.content.orchestrator.api.GetTenantVersionResponse", &r_ten_reset.payload.unwrap().value);
    if let Some(DynamicValue::Message(v)) = dyn_ten_reset.get(1) {
        assert_eq!((v.get(1), v.get(2), v.get(3)), (Some(&DynamicValue::Uint32(2)), Some(&DynamicValue::Uint32(3)), Some(&DynamicValue::Uint32(4))));
    } else {
        panic!("expected reset tenant version message");
    }

    // 3. DryRunRulesetRequest (evaluated against real SDE solar systems)
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.api.DryRunRulesetRequest", Vec::new()).await.status_code, STATUS_BAD_REQUEST);
    let mut tag_hs = Vec::new();
    encode_string_field(1, "highsec", &mut tag_hs);
    let mut tag_wh = Vec::new();
    encode_string_field(1, "wormhole", &mut tag_wh);
    let mut ruleset_buf = Vec::new();
    encode_bytes_field(1, &tag_hs, &mut ruleset_buf);
    encode_bytes_field(4, &tag_wh, &mut ruleset_buf);
    let mut dry_req = Vec::new();
    encode_bytes_field(1, &ruleset_buf, &mut dry_req);
    let dry = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.api.DryRunRulesetRequest", dry_req).await;
    assert_eq!(dry.status_code, STATUS_OK);
    let dyn_dry = decode_dyn(&ctx, "eve.content.orchestrator.api.DryRunRulesetResponse", &dry.payload.unwrap().value);
    match dyn_dry.get(1) {
        Some(DynamicValue::Repeated(evals)) => assert!(!evals.is_empty()),
        other => panic!("expected evaluation_result list, got {other:?}"),
    }
    match dyn_dry.get(2) {
        Some(DynamicValue::Repeated(tags_eval)) => assert_eq!(tags_eval.len(), 2),
        other => panic!("expected ruleset_tags_evaluated list, got {other:?}"),
    }

    // 4. Deployment ListRequest, GetRequest, DisableRequest, EnableRequest + role checks
    let dlist = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.api.ListRequest", Vec::new()).await;
    assert_eq!(dlist.status_code, STATUS_OK);
    let dyn_dlist = decode_dyn(&ctx, "eve.content.orchestrator.deployment.api.ListResponse", &dlist.payload.unwrap().value);
    match dyn_dlist.get(1) {
        Some(DynamicValue::Repeated(deps)) => {
            assert_eq!(deps.len(), 1);
            let DynamicValue::Message(dep_msg) = &deps[0] else { panic!("expected Deployment message"); };
            assert_eq!(dep_msg.get(2), Some(&DynamicValue::String("pirate_insurgency_deployment".into())));
        }
        other => panic!("expected deployments list, got {other:?}"),
    }

    let dep_id = seq_bytes(101);
    assert_eq!(quasar_rpc(&ctx, &mut player, "eve.content.orchestrator.deployment.api.DisableRequest", dep_id.clone()).await.status_code, STATUS_FORBIDDEN);
    assert_eq!(quasar_rpc(&ctx, &mut player, "eve.content.orchestrator.deployment.api.EnableRequest", dep_id.clone()).await.status_code, STATUS_FORBIDDEN);

    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.api.DisableRequest", dep_id.clone()).await.status_code, STATUS_OK);
    let dget_dis = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.api.GetRequest", dep_id.clone()).await;
    assert_eq!(dget_dis.status_code, STATUS_OK);
    let dyn_dis = decode_dyn(&ctx, "eve.content.orchestrator.deployment.api.GetResponse", &dget_dis.payload.unwrap().value);
    if let Some(DynamicValue::Message(attr)) = dyn_dis.get(1) {
        assert_eq!(attr.get(7), Some(&DynamicValue::Bool(true)));
    } else {
        panic!("expected deployment attributes message");
    }
    ctx.db.with_snapshot(|s| {
        let st: OrchestratorState = serde_json::from_str(&s.service_state_blobs["content_orchestrator_state"]).unwrap();
        assert!(!st.deployments[&101].enabled);
    });

    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.api.EnableRequest", dep_id.clone()).await.status_code, STATUS_OK);
    let dget_en = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.api.GetRequest", dep_id).await;
    assert_eq!(dget_en.status_code, STATUS_OK);
    let dyn_en = decode_dyn(&ctx, "eve.content.orchestrator.deployment.api.GetResponse", &dget_en.payload.unwrap().value);
    assert!(matches!(dyn_en.get(1), Some(DynamicValue::Message(_))));
    ctx.db.with_snapshot(|s| {
        let st: OrchestratorState = serde_json::from_str(&s.service_state_blobs["content_orchestrator_state"]).unwrap();
        assert!(st.deployments[&101].enabled);
    });

    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.api.GetRequest", seq_bytes(9999)).await.status_code, STATUS_NOT_FOUND);
}

#[tokio::test]
// parity: quasar:eveProto/generated/eve/content/orchestrator/deployment/contentgroup/api/requests_pb2.pyj, quasar:eveProto/generated/eve/content/orchestrator/deployment/contenttype/api/requests_pb2.pyj, quasar:eveProto/generated/eve/content/orchestrator/deployment/instance/api/requests_pb2.pyj, quasar:eveProto/generated/eve/content/orchestrator/deployment/instance/admin/api/requests_pb2.pyj
async fn test_b29_orchestrator_content_and_instances() {
    let ctx = ServerContext::new();
    seed_test_orchestrator(&ctx);
    let mut sess = make_session(2, 1002, 2112000002, eve_machonet::service_roles::ROLE_GM_DEFAULT);
    let mut player = make_session(20, 2002, 2112000099, eve_machonet::service_roles::ROLE_PLAYER_DEFAULT);

    assert_eq!(quasar_rpc(&ctx, &mut player, "eve.content.orchestrator.deployment.contentgroup.api.GetRequest", seq_bytes(201)).await.status_code, STATUS_FORBIDDEN);
    let cg_resp = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.contentgroup.api.GetRequest", seq_bytes(201)).await;
    assert_eq!(cg_resp.status_code, STATUS_OK);
    let dyn_cg = decode_dyn(&ctx, "eve.content.orchestrator.deployment.contentgroup.api.GetResponse", &cg_resp.payload.unwrap().value);
    if let Some(DynamicValue::Message(attr)) = dyn_cg.get(1) {
        assert_eq!(attr.get(2), Some(&DynamicValue::String("insurgency_sites".into())));
    } else {
        panic!("expected content group attributes");
    }

    let ct_resp = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.contenttype.api.GetRequest", seq_bytes(301)).await;
    assert_eq!(ct_resp.status_code, STATUS_OK);
    let dyn_ct = decode_dyn(&ctx, "eve.content.orchestrator.deployment.contenttype.api.GetResponse", &ct_resp.payload.unwrap().value);
    if let Some(DynamicValue::Message(attr)) = dyn_ct.get(1) {
        assert_eq!(attr.get(1), Some(&DynamicValue::String("guristas_outpost".into())));
    } else {
        panic!("expected content type attributes");
    }

    let ilist = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.instance.api.ListRequest", seq_bytes(101)).await;
    assert_eq!(ilist.status_code, STATUS_OK);
    let dyn_ilist = decode_dyn(&ctx, "eve.content.orchestrator.deployment.instance.api.ListResponse", &ilist.payload.unwrap().value);
    match dyn_ilist.get(1) {
        Some(DynamicValue::Repeated(insts)) => {
            assert_eq!(insts.len(), 1);
            let DynamicValue::Message(inst_id) = &insts[0] else { panic!("expected instance Identifier"); };
            assert_eq!(inst_id.get(1), Some(&DynamicValue::Bytes(b"TESTORCHINST0001".to_vec())));
        }
        other => panic!("expected instances list, got {other:?}"),
    }

    let inst_req = uuid_bytes(b"TESTORCHINST0001");
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.instance.api.GetRequest", inst_req.clone()).await.status_code, STATUS_OK);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.instance.admin.api.RemoveRequest", inst_req.clone()).await.status_code, STATUS_OK);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.instance.api.GetRequest", inst_req).await.status_code, STATUS_NOT_FOUND);
}

#[tokio::test]
// parity: quasar:eveProto/generated/eve/cosmetic/market/component/listing/api/requests_pb2.pyj, quasar:eveProto/generated/eve/cosmetic/market/skin/listing/api/admin/requests_pb2.pyj
async fn test_b29_cosmetic_market_and_payment_admin() {
    let ctx = ServerContext::new();
    let seller_cid = 90_000_001u32;
    let listing_uuid = *b"TESTSKINLIST0001";
    ctx.db.with_mut_snapshot(|snap| {
        let mut cosm = CosmeticsMarketData::default();
        cosm.skin_listings.insert("54455354534b494e4c49535430303031".into(), SkinListingRecord {
            uuid: listing_uuid,
            seller_id: seller_cid,
            created_unix: 1_700_000_000,
            expires_unix: 1_800_000_000,
            price_isk: Some(1_000_000),
            price_plex_cents: None,
            skin_hex: "deadbeef".into(),
            quantity: 1,
            target: ListingTargetRecord::Public,
            branded: false,
        });
        snap.quasar_state_blobs.insert("quasar_cosmetics_market".into(), serde_json::to_string(&cosm).unwrap());
        snap.accounts.insert("admin_b29".into(), AccountRecord {
            user_id: 1003,
            username: "admin_b29".into(),
            password_token: WorldRepository::hash_password_token("admin_b29", "pw"),
            role: eve_db::service_roles::ROLE_GM_DEFAULT,
            banned_until_unix: None,
            ban_reason: None,
        });
        let mut ch: CharacterRecord = snap.characters.values().next().unwrap().clone();
        ch.character_id = seller_cid as i64;
        ch.user_id = 1003;
        ch.name = "Valeria Vance".into();
        ch.region_id = 10_000_002;
        snap.characters.insert(seller_cid as i64, ch);
    });

    let mut sess = make_session(3, 1003, seller_cid as i64, eve_machonet::service_roles::ROLE_GM_DEFAULT);
    let mut player = make_session(30, 2003, seller_cid as i64, eve_machonet::service_roles::ROLE_PLAYER_DEFAULT);

    // 1. Component listing GetAllRequest
    assert_eq!(quasar_rpc(&ctx, &mut player, "eve.cosmetic.market.component.listing.api.GetAllRequest", Vec::new()).await.status_code, STATUS_FORBIDDEN);
    let comp_resp = quasar_rpc(&ctx, &mut sess, "eve.cosmetic.market.component.listing.api.GetAllRequest", Vec::new()).await;
    assert_eq!(comp_resp.status_code, STATUS_OK);
    let dyn_comp = decode_dyn(&ctx, "eve.cosmetic.market.component.listing.api.GetAllResponse", &comp_resp.payload.unwrap().value);
    match dyn_comp.get(1) {
        Some(DynamicValue::Repeated(items)) => assert!(items.len() >= 4, "expected >= 4 SDE component listings, got {}", items.len()),
        other => panic!("expected >= 4 SDE component listings, got {other:?}"),
    }

    // 2. Skin listing admin SetExpirationRequest & GetAllListedByCharacterRequest
    let mut exp_req = uuid_bytes(&listing_uuid);
    let mut ts_buf = Vec::new();
    encode_varint_field(1, 1_900_000_000, &mut ts_buf);
    encode_bytes_field(2, &ts_buf, &mut exp_req);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.cosmetic.market.skin.listing.api.admin.SetExpirationRequest", exp_req).await.status_code, STATUS_OK);

    let by_char_resp = quasar_rpc(&ctx, &mut sess, "eve.cosmetic.market.skin.listing.api.admin.GetAllListedByCharacterRequest", seq_bytes(seller_cid as u64)).await;
    assert_eq!(by_char_resp.status_code, STATUS_OK);
    let dyn_by_char = decode_dyn(&ctx, "eve.cosmetic.market.skin.listing.api.admin.GetAllListedByCharacterResponse", &by_char_resp.payload.unwrap().value);
    let listings = match dyn_by_char.get(1) {
        Some(DynamicValue::Repeated(v)) => v,
        other => panic!("expected skin listings, got {other:?}"),
    };
    assert_eq!(listings.len(), 1);
    if let DynamicValue::Message(entry) = &listings[0] {
        if let Some(DynamicValue::Message(attr)) = entry.get(2) {
            if let Some(DynamicValue::Message(ts)) = attr.get(3) {
                assert_eq!(ts.get(1), Some(&DynamicValue::Int64(1_900_000_000)));
            } else { panic!("expected expiration timestamp"); }
        } else { panic!("expected listing attributes"); }
    }

    // Verify public cosmetics market GetAllRequest (which triggers CosmeticsMarketState::save()) preserves the admin expiration
    assert_eq!(quasar_rpc(&ctx, &mut player, "eve_public.cosmetic.market.component.listing.api.GetAllRequest", Vec::new()).await.status_code, STATUS_OK);
    let pub_get = quasar_rpc(&ctx, &mut player, "eve_public.cosmetic.market.skin.listing.api.GetRequest", uuid_bytes(&listing_uuid)).await;
    assert_eq!(pub_get.status_code, STATUS_OK);
    let dyn_pub = decode_dyn(&ctx, "eve_public.cosmetic.market.skin.listing.api.GetResponse", &pub_get.payload.unwrap().value);
    if let Some(DynamicValue::Message(listing)) = dyn_pub.get(1) {
        if let Some(DynamicValue::Message(attr)) = listing.get(2) {
            if let Some(DynamicValue::Message(ts)) = attr.get(4) {
                assert_eq!(ts.get(1), Some(&DynamicValue::Int64(1_900_000_000)));
            } else { panic!("expected public listing expiration timestamp"); }
        } else { panic!("expected public listing attributes"); }
    } else { panic!("expected public listing"); }
}

#[tokio::test]
// parity: quasar:eveProto/generated/eve/store/sale/admin/requests_pb2.pyj, quasar:eveProto/generated/eve/store/targetaudience/offer/api/admin/requests_pb2.pyj
async fn test_b29_store_sale_and_targeted_offer_admin() {
    let ctx = ServerContext::new();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ff = BTreeMap::new();
        ff.insert(5001u32, FailedFulfillmentRecord { id: 5001, user_id: 1004, plex_units: 250, retries: 0, fulfilled: false });
        ff.insert(5002u32, FailedFulfillmentRecord { id: 5002, user_id: 1004, plex_units: 100, retries: 0, fulfilled: false });
        snap.service_state_blobs.insert("store_failed_fulfillments".into(), serde_json::to_string(&ff).unwrap());
    });
    let mut sess = make_session(4, 1004, 2112000004, eve_machonet::service_roles::ROLE_GM_DEFAULT);

    // 1. Store sale admin RetryFailedFulfillmentRequest, RetryAll, DropFailedFulfillmentRequest
    let fid_req = seq_bytes(5001);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.store.sale.admin.RetryFailedFulfillmentRequest", fid_req.clone()).await.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|s| s.account_plex_vault.get(&1004).copied()), Some(250));

    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.store.sale.admin.RetryAllFailedFulfillmentsRequest", Vec::new()).await.status_code, STATUS_OK);
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.account_plex_vault.get(&1004).copied(), Some(350));
        assert!(get_plex_transactions(s).len() >= 2);
    });

    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.store.sale.admin.DropFailedFulfillmentRequest", fid_req.clone()).await.status_code, STATUS_OK);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.store.sale.admin.DropFailedFulfillmentRequest", fid_req).await.status_code, STATUS_NOT_FOUND);

    // 2. Targeted offer admin PromoteRequest, RetractRequest, BackfillPartnerCodeRequest
    let mut user_sub = Vec::new();
    encode_varint_field(1, (((1004i32 << 1) ^ (1004i32 >> 31)) as u32) as u64, &mut user_sub);
    let mut cat_sub = Vec::new();
    encode_string_field(1, "cat-entry-7001", &mut cat_sub);
    let mut offer_id = Vec::new();
    encode_bytes_field(1, &user_sub, &mut offer_id);
    encode_bytes_field(2, &cat_sub, &mut offer_id);

    let mut prom_req = Vec::new();
    encode_bytes_field(1, &offer_id, &mut prom_req);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.store.targetaudience.offer.api.admin.PromoteRequest", prom_req.clone()).await.status_code, STATUS_OK);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.store.targetaudience.offer.api.admin.RetractRequest", prom_req).await.status_code, STATUS_OK);

    let mut bf_req = sint32_user_bytes(1004);
    encode_bytes_field(3, &offer_id, &mut bf_req);
    encode_string_field(4, "PARTNER-B29", &mut bf_req);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.store.targetaudience.offer.api.admin.BackfillPartnerCodeRequest", bf_req).await.status_code, STATUS_OK);
    ctx.db.with_snapshot(|s| {
        let offers: BTreeMap<String, TargetedOfferAdminRecord> = s.service_state_blobs.get("store_targeted_offers").and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default();
        let rec = offers.get("1004:cat-entry-7001").expect("targeted offer persisted");
        assert!(rec.promoted && rec.retracted && rec.partner_code == "PARTNER-B29");
    });

    let bad_bf = quasar_rpc(&ctx, &mut sess, "eve.store.targetaudience.offer.api.admin.BackfillPartnerCodeRequest", Vec::new()).await;
    assert_eq!((bad_bf.status_code, bad_bf.status_message.as_str()), (STATUS_BAD_REQUEST, "user and partner_code required"));
}
