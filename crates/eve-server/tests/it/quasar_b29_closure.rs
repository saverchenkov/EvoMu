//! Integration tests for Batch 29 Quasar Public Gateway services.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_OK};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static CORR_SEQ: AtomicU64 = AtomicU64::new(1);

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
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
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: format!("b29-quasar-{seq:05}").into_bytes(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        ..Default::default()
    };
    let empty_kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("publicGatewaySvc.SendRequest dispatch");
    extract_response_proto(&rep)
}

fn decode_dyn(ctx: &ServerContext, type_name: &str, bytes: &[u8]) -> DynamicQuasarMessage {
    ctx.quasar_gateway
        .descriptors
        .decode_dynamic(type_name, bytes)
        .unwrap_or_else(|e| panic!("failed to decode {type_name}: {e:?}"))
}

fn encode_seq_id(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_varint_field(1, id, &mut sub);
    encode_bytes_field(field_num, &sub, out);
}

fn encode_uuid_id(field_num: u32, uuid: &[u8], out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_bytes_field(1, uuid, &mut sub);
    encode_bytes_field(field_num, &sub, out);
}

fn encode_semver(field_num: u32, major: u64, minor: u64, patch: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    if major > 0 {
        encode_varint_field(1, major, &mut sub);
    }
    if minor > 0 {
        encode_varint_field(2, minor, &mut sub);
    }
    if patch > 0 {
        encode_varint_field(3, patch, &mut sub);
    }
    encode_bytes_field(field_num, &sub, out);
}

#[tokio::test]
// parity: quasar:eveProto/generated/eve/content/orchestrator/admin/api/requests_pb2.pyj, quasar:eveProto/generated/eve/content/orchestrator/api/requests_pb2.pyj, quasar:eveProto/generated/eve/content/orchestrator/deployment/api/requests_pb2.pyj
async fn test_b29_orchestrator_version_and_deployment() {
    let ctx = ServerContext::new();
    let mut sess = make_session(1, 1001, 2112000001, 98000001);

    // 1. SetDefaultTenantVersionRequest & GetDefaultTenantVersionRequest
    let mut set_def = Vec::new();
    encode_semver(1, 2, 3, 4, &mut set_def);
    let r1 = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.content.orchestrator.admin.api.SetDefaultTenantVersionRequest",
        set_def,
    )
    .await;
    assert_eq!(r1.status_code, STATUS_OK);

    let r2 = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.content.orchestrator.api.GetDefaultTenantVersionRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(r2.status_code, STATUS_OK);
    let dyn_ver = decode_dyn(
        &ctx,
        "eve.content.orchestrator.api.GetDefaultTenantVersionResponse",
        &r2.payload.unwrap().value,
    );
    if let Some(DynamicValue::Message(v)) = dyn_ver.get(1) {
        assert_eq!(v.get(1), Some(&DynamicValue::Uint32(2)));
        assert_eq!(v.get(2), Some(&DynamicValue::Uint32(3)));
        assert_eq!(v.get(3), Some(&DynamicValue::Uint32(4)));
    } else {
        panic!("expected version message");
    }

    // 2. DryRunRulesetRequest
    let dry = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.content.orchestrator.api.DryRunRulesetRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(dry.status_code, STATUS_OK);
    let dyn_dry = decode_dyn(
        &ctx,
        "eve.content.orchestrator.api.DryRunRulesetResponse",
        &dry.payload.unwrap().value,
    );
    assert!(dyn_dry.get(1).is_some());

    // 3. Deployment ListRequest, GetRequest, DisableRequest, EnableRequest
    let dlist = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.content.orchestrator.deployment.api.ListRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(dlist.status_code, STATUS_OK);

    let mut dep_id = Vec::new();
    encode_seq_id(1, 101, &mut dep_id);
    let dget = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.content.orchestrator.deployment.api.GetRequest",
        dep_id.clone(),
    )
    .await;
    assert_eq!(dget.status_code, STATUS_OK);

    let ddis = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.content.orchestrator.deployment.api.DisableRequest",
        dep_id,
    )
    .await;
    assert_eq!(ddis.status_code, STATUS_OK);

    let mut bad_dep = Vec::new();
    encode_seq_id(1, 9999, &mut bad_dep);
    let derr = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.content.orchestrator.deployment.api.GetRequest",
        bad_dep,
    )
    .await;
    assert_eq!(derr.status_code, STATUS_NOT_FOUND);
}

#[tokio::test]
// parity: quasar:eveProto/generated/eve/content/orchestrator/deployment/contentgroup/api/requests_pb2.pyj, quasar:eveProto/generated/eve/content/orchestrator/deployment/contenttype/api/requests_pb2.pyj, quasar:eveProto/generated/eve/content/orchestrator/deployment/instance/api/requests_pb2.pyj, quasar:eveProto/generated/eve/content/orchestrator/deployment/instance/admin/api/requests_pb2.pyj
async fn test_b29_orchestrator_content_and_instances() {
    let ctx = ServerContext::new();
    let mut sess = make_session(2, 1002, 2112000002, 98000001);

    // 1. ContentGroup GetRequest
    let mut cg_req = Vec::new();
    encode_seq_id(1, 201, &mut cg_req);
    let cg_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.content.orchestrator.deployment.contentgroup.api.GetRequest",
        cg_req,
    )
    .await;
    assert_eq!(cg_resp.status_code, STATUS_OK);
    let dyn_cg = decode_dyn(
        &ctx,
        "eve.content.orchestrator.deployment.contentgroup.api.GetResponse",
        &cg_resp.payload.unwrap().value,
    );
    if let Some(DynamicValue::Message(attr)) = dyn_cg.get(1) {
        assert_eq!(
            attr.get(2),
            Some(&DynamicValue::String("insurgency_sites".into()))
        );
    } else {
        panic!("expected content group attributes");
    }

    // 2. ContentType GetRequest
    let mut ct_req = Vec::new();
    encode_seq_id(1, 301, &mut ct_req);
    let ct_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.content.orchestrator.deployment.contenttype.api.GetRequest",
        ct_req,
    )
    .await;
    assert_eq!(ct_resp.status_code, STATUS_OK);
    let dyn_ct = decode_dyn(
        &ctx,
        "eve.content.orchestrator.deployment.contenttype.api.GetResponse",
        &ct_resp.payload.unwrap().value,
    );
    if let Some(DynamicValue::Message(attr)) = dyn_ct.get(1) {
        assert_eq!(
            attr.get(1),
            Some(&DynamicValue::String("guristas_outpost".into()))
        );
    } else {
        panic!("expected content type attributes");
    }

    // 3. Instance ListRequest, GetRequest, Admin RemoveRequest
    let mut ilist_req = Vec::new();
    encode_seq_id(1, 101, &mut ilist_req);
    let ilist = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.content.orchestrator.deployment.instance.api.ListRequest",
        ilist_req,
    )
    .await;
    assert_eq!(ilist.status_code, STATUS_OK);

    let mut inst_req = Vec::new();
    encode_uuid_id(1, b"b29-orch-inst-01", &mut inst_req);
    let iget = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.content.orchestrator.deployment.instance.api.GetRequest",
        inst_req.clone(),
    )
    .await;
    assert_eq!(iget.status_code, STATUS_OK);

    let irem = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.content.orchestrator.deployment.instance.admin.api.RemoveRequest",
        inst_req.clone(),
    )
    .await;
    assert_eq!(irem.status_code, STATUS_OK);

    let iget_after = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.content.orchestrator.deployment.instance.api.GetRequest",
        inst_req,
    )
    .await;
    assert_eq!(iget_after.status_code, STATUS_NOT_FOUND);
}

#[tokio::test]
// parity: quasar:eveProto/generated/eve/cosmetic/market/component/listing/api/requests_pb2.pyj, quasar:eveProto/generated/eve/cosmetic/market/skin/listing/api/admin/requests_pb2.pyj, quasar:eveProto/generated/eve/payment/admin/api/requests_pb2.pyj
async fn test_b29_cosmetic_market_and_payment_admin() {
    let ctx = ServerContext::new();
    let mut sess = make_session(3, 1003, 2112000001, 98000001);

    // 1. Component listing GetAllRequest
    let comp_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.cosmetic.market.component.listing.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(comp_resp.status_code, STATUS_OK);
    let dyn_comp = decode_dyn(
        &ctx,
        "eve.cosmetic.market.component.listing.api.GetAllResponse",
        &comp_resp.payload.unwrap().value,
    );
    if let Some(DynamicValue::Repeated(items)) = dyn_comp.get(1) {
        assert_eq!(items.len(), 1);
    } else {
        panic!("expected component listings");
    }

    // 2. Skin listing admin SetExpirationRequest & GetAllListedByCharacterRequest
    let mut exp_req = Vec::new();
    encode_uuid_id(1, b"b29-skin-list-01", &mut exp_req);
    let mut ts_buf = Vec::new();
    encode_varint_field(1, 1_900_000_000, &mut ts_buf);
    encode_bytes_field(2, &ts_buf, &mut exp_req);
    let exp_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
        exp_req,
    )
    .await;
    assert_eq!(exp_resp.status_code, STATUS_OK);

    let mut by_char_req = Vec::new();
    encode_seq_id(1, 2112000001, &mut by_char_req);
    let by_char_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.cosmetic.market.skin.listing.api.admin.GetAllListedByCharacterRequest",
        by_char_req,
    )
    .await;
    assert_eq!(by_char_resp.status_code, STATUS_OK);

    // 3. Payment admin ProcessPaymentFilesRequest & GetPersonalDataRequest
    let mut proc_req = Vec::new();
    encode_seq_id(1, 42, &mut proc_req);
    let proc_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.payment.admin.api.ProcessPaymentFilesRequest",
        proc_req,
    )
    .await;
    assert_eq!(
        proc_resp.status_code, STATUS_OK,
        "ProcessPaymentFilesRequest failed: {}",
        proc_resp.status_message
    );

    let mut pd_req = Vec::new();
    encode_seq_id(1, 1003, &mut pd_req);
    let pd_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.payment.admin.api.GetPersonalDataRequest",
        pd_req,
    )
    .await;
    assert_eq!(pd_resp.status_code, STATUS_OK);
    let dyn_pd = decode_dyn(
        &ctx,
        "eve.payment.admin.api.GetPersonalDataResponse",
        &pd_resp.payload.unwrap().value,
    );
    assert!(dyn_pd.get(3).is_some() || dyn_pd.fields.is_empty());
}

#[tokio::test]
// parity: quasar:eveProto/generated/eve/store/sale/admin/requests_pb2.pyj, quasar:eveProto/generated/eve/store/targetaudience/offer/api/admin/requests_pb2.pyj
async fn test_b29_store_sale_and_targeted_offer_admin() {
    let ctx = ServerContext::new();
    let mut sess = make_session(4, 1004, 2112000004, 98000001);

    // 1. Store sale admin RetryFailedFulfillmentRequest, RetryAll, DropFailedFulfillmentRequest
    let mut fid_req = Vec::new();
    encode_seq_id(1, 5001, &mut fid_req);
    let retry_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.store.sale.admin.RetryFailedFulfillmentRequest",
        fid_req.clone(),
    )
    .await;
    assert_eq!(retry_resp.status_code, STATUS_OK);

    let retry_all = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.store.sale.admin.RetryAllFailedFulfillmentsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(retry_all.status_code, STATUS_OK);

    let drop_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.store.sale.admin.DropFailedFulfillmentRequest",
        fid_req.clone(),
    )
    .await;
    assert_eq!(drop_resp.status_code, STATUS_OK);

    let drop_missing = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.store.sale.admin.DropFailedFulfillmentRequest",
        fid_req,
    )
    .await;
    assert_eq!(drop_missing.status_code, STATUS_NOT_FOUND);

    // 2. Targeted offer admin PromoteRequest, RetractRequest, BackfillPartnerCodeRequest
    let mut offer_id = Vec::new();
    encode_seq_id(1, 1004, &mut offer_id);
    let mut cat_buf = Vec::new();
    encode_string_field(1, "cat-entry-7001", &mut cat_buf);
    encode_bytes_field(2, &cat_buf, &mut offer_id);

    let mut prom_req = Vec::new();
    encode_bytes_field(1, &offer_id, &mut prom_req);
    let prom_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.store.targetaudience.offer.api.admin.PromoteRequest",
        prom_req,
    )
    .await;
    assert_eq!(prom_resp.status_code, STATUS_OK);

    let mut ret_req = Vec::new();
    encode_bytes_field(1, &offer_id, &mut ret_req);
    let ret_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.store.targetaudience.offer.api.admin.RetractRequest",
        ret_req,
    )
    .await;
    assert_eq!(ret_resp.status_code, STATUS_OK);

    let mut bf_req = Vec::new();
    encode_seq_id(1, 1004, &mut bf_req);
    encode_bytes_field(3, &offer_id, &mut bf_req);
    encode_string_field(4, "PARTNER-B29", &mut bf_req);
    let bf_resp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.store.targetaudience.offer.api.admin.BackfillPartnerCodeRequest",
        bf_req,
    )
    .await;
    assert_eq!(bf_resp.status_code, STATUS_OK);

    let bad_bf = quasar_rpc(
        &ctx,
        &mut sess,
        "eve.store.targetaudience.offer.api.admin.BackfillPartnerCodeRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(bad_bf.status_code, STATUS_BAD_REQUEST);
    assert_eq!(bad_bf.status_message, "user and partner_code required");
}
