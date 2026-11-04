//! Integration tests for Quasar ID, Implant, Industry, Inventory, ISK,
//! Killmail, Legal, Localization, Login, and LoyaltyPoints services and model protos.

use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicValue,
    EvePublicEvent, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
    QuasarDescriptorRegistry, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static CORR_SEQ: AtomicU64 = AtomicU64::new(5000);

fn seed_test_fixtures(ctx: &ServerContext) {
    let _ = ctx.db.provision_player_account("test_pilot", "test_pass_123");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.balance = eve_db::IskCents::from_isk(10_000_000);
            ch.loyalty_points.insert(1000002, 50_000);
        }
    });
}

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT | eve_machonet::service_roles::ROLE_GM_MASK;
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_ACCOUNTANT;
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
    let mut corr = *b"quasar-id-lp-000";
    corr[12..16].copy_from_slice(&(seq as u32).to_le_bytes());
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: corr.to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-quasar-it".to_vec(),
        authoritative_context: None,
        trace_context: None,
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
        .expect("SendRequest");
    extract_response_proto(&rep)
}

async fn quasar_publish_event(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload: Vec<u8>,
) {
    let ev = EvePublicEvent {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload,
        }),
        ..Default::default()
    };
    let kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "PublishEvent",
            &[PyRep::Bytes(ev.encode_proto())],
            &kw,
            None,
        )
        .await
        .expect("PublishEvent");
    assert_eq!(rep, PyRep::Bool(true));
}

async fn subscribe_to_notices(
    ctx: &ServerContext,
    sess: &mut SessionState,
    notice_types: &[&str],
) {
    let args: Vec<PyRep> = notice_types
        .iter()
        .map(|s| PyRep::String(s.to_string()))
        .collect();
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        sess,
        "publicGatewaySvc",
        None,
        // rpc-lint: allow Quasar notice subscription and poll routes
        "SubscribeToNotice",
        &args,
        &kw,
        None,
    )
    .await
    .expect("SubscribeToNotice");
}

async fn poll_notices(
    ctx: &ServerContext,
    sess: &mut SessionState,
) -> Vec<(String, Vec<u8>)> {
    let kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            // rpc-lint: allow Quasar notice subscription and poll routes
            "PollNotices",
            &[],
            &kw,
            None,
        )
        .await
        .expect("PollNotices");

    let mut out = Vec::new();
    if let PyRep::List(items) = rep {
        for it in items {
            if let PyRep::KeyVal(kv) = it {
                let mut type_url = String::new();
                let mut payload = Vec::new();
                for (k, v) in kv {
                    if k == "type_url" {
                        if let PyRep::String(s) = v {
                            type_url = s;
                        }
                    } else if k == "payload" {
                        if let PyRep::Bytes(b) = v {
                            payload = b;
                        }
                    }
                }
                out.push((type_url, payload));
            }
        }
    }
    out
}

fn encode_id_field(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_varint_field(1, id, &mut sub);
    encode_bytes_field(field_num, &sub, out);
}

fn make_lp_op_payload(
    op_uuid: [u8; 16],
    cid: Option<u64>,
    issuer: u64,
    amount: u64,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &op_uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);

    let mut wallet_bytes = Vec::new();
    if let Some(c) = cid {
        let mut c_bytes = Vec::new();
        encode_varint_field(1, c, &mut c_bytes);
        encode_bytes_field(1, &c_bytes, &mut wallet_bytes);
    }
    let mut issuer_bytes = Vec::new();
    encode_varint_field(1, issuer, &mut issuer_bytes);
    encode_bytes_field(3, &issuer_bytes, &mut wallet_bytes);

    let mut curr_bytes = Vec::new();
    encode_varint_field(1, amount, &mut curr_bytes);

    let mut attr_bytes = Vec::new();
    encode_bytes_field(1, &wallet_bytes, &mut attr_bytes);
    encode_bytes_field(2, &curr_bytes, &mut attr_bytes);
    encode_bytes_field(2, &attr_bytes, &mut out);
    out
}

#[tokio::test]
async fn test_quasar_id_implant_and_login() {
    // parity: feat:quasar.id
    // parity: feat:quasar.implant
    // parity: feat:quasar.login
    eve_testkit::attest(&["feat:quasar.id", "feat:quasar.implant", "feat:quasar.login"]);

    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(7001, 1, 90_000_001, 1_000_170);
    let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");

    // 1. Quasar ID: 6 proto files in descriptor registry
    let id_files = reg.files_by_domain.get("id").expect("id domain files");
    assert_eq!(id_files.len(), 6, "Quasar ID domain must contain exactly 6 proto files");
    assert!(reg.files_by_proto_name.contains_key("eve/id/api/api.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/id/auth/auth.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/id/federate/twitch/api/api.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/id/federate/twitch/twitch.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/id/id.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/id/legacy/api/api.proto"));

    // Dispatch ID API requests
    let mut id_sub = Vec::new();
    encode_bytes_field(1, b"usr-000000000001", &mut id_sub);
    let mut id_req = Vec::new();
    encode_bytes_field(1, &id_sub, &mut id_req);
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.id.api.GetRequest", id_req).await;
    assert_eq!(rsp.status_code, STATUS_OK, "eve.id.api.GetRequest: {}", rsp.status_message);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.id.api.GetResponse"));

    let mut by_user_req = Vec::new();
    encode_id_field(1, 1, &mut by_user_req);
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.id.api.GetByUserRequest", by_user_req).await;
    assert_eq!(rsp.status_code, STATUS_OK, "eve.id.api.GetByUserRequest: {}", rsp.status_message);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.id.api.GetByUserResponse"));

    let mut legacy_req = Vec::new();
    encode_id_field(1, 1, &mut legacy_req);
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.id.legacy.api.GetUserRequest", legacy_req).await;
    assert_eq!(rsp.status_code, STATUS_OK, "eve.id.legacy.api.GetUserRequest: {}", rsp.status_message);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.id.legacy.api.GetUserResponse"));

    // Twitch federation routing
    let mut link_req = Vec::new();
    encode_string_field(1, "twitch_user_alpha", &mut link_req);
    encode_string_field(2, "twitch_code_xyz", &mut link_req);
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.id.federate.twitch.api.CompleteLinkingRequest", link_req).await;
    assert_eq!(rsp.status_code, STATUS_OK, "CompleteLinkingRequest: {}", rsp.status_message);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.id.federate.twitch.api.CompleteLinkingResponse"));

    let mut tw_get_req = Vec::new();
    let mut id_sub = Vec::new();
    encode_bytes_field(1, b"twitch_user_alpha", &mut id_sub);
    encode_bytes_field(1, &id_sub, &mut tw_get_req);
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.id.federate.twitch.api.GetRequest", tw_get_req).await;
    assert_eq!(rsp.status_code, STATUS_OK, "eve.id.federate.twitch.api.GetRequest: {}", rsp.status_message);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.id.federate.twitch.api.GetResponse"));

    // 2. Quasar Implant: 1 proto file in descriptor registry
    let implant_files = reg.files_by_domain.get("implant").expect("implant domain files");
    assert_eq!(implant_files.len(), 1, "Quasar implant domain must contain exactly 1 proto file");
    assert!(reg.files_by_proto_name.contains_key("eve/implant/implant_type.proto"));

    // Dispatch implant GetAllRequest
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.implanttype.GetAllRequest", Vec::new()).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.implanttype.GetAllResponse"));

    // Dynamic decode model message eve.implanttype.Attributes
    let mut imp_attr_bytes = Vec::new();
    encode_string_field(1, "Ocular Filter - Standard", &mut imp_attr_bytes);
    encode_string_field(2, "Increases perception attribute", &mut imp_attr_bytes);
    encode_varint_field(3, 1, &mut imp_attr_bytes); // slot 1
    let dyn_attr = reg.decode_dynamic("eve.implanttype.Attributes", &imp_attr_bytes).expect("decode implant attributes");
    let name_str = match dyn_attr.get(1) {
        Some(DynamicValue::String(s)) => s.as_str(),
        _ => "",
    };
    assert_eq!(name_str, "Ocular Filter - Standard");
    let slot_num = match dyn_attr.get(3) {
        Some(DynamicValue::Uint32(n)) => *n as u64,
        Some(DynamicValue::Uint64(n)) => *n,
        Some(DynamicValue::Int32(n)) => *n as u64,
        Some(DynamicValue::Int64(n)) => *n as u64,
        _ => 0,
    };
    assert_eq!(slot_num, 1);

    // 3. Quasar Login: 2 proto files in descriptor registry
    let login_files = reg.files_by_domain.get("login").expect("login domain files");
    assert_eq!(login_files.len(), 2, "Quasar login domain must contain exactly 2 proto files");
    assert!(reg.files_by_proto_name.contains_key("eve/login/milestone.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve_public/login/milestone.proto"));

    // Dynamic decode model messages for login milestones
    let mut ms_bytes = Vec::new();
    encode_varint_field(1, 14, &mut ms_bytes); // sequential milestone 14
    let dyn_ms = reg.decode_dynamic("eve.login.milestone.Identifier", &ms_bytes).expect("decode eve.login.milestone");
    let ms_seq = match dyn_ms.get(1) {
        Some(DynamicValue::Uint32(n)) => *n as u64,
        Some(DynamicValue::Uint64(n)) => *n,
        _ => 0,
    };
    assert_eq!(ms_seq, 14);

    let dyn_pub_ms = reg.decode_dynamic("eve_public.login.milestone.Identifier", &ms_bytes).expect("decode eve_public.login.milestone");
    let pub_ms_seq = match dyn_pub_ms.get(1) {
        Some(DynamicValue::Uint32(n)) => *n as u64,
        Some(DynamicValue::Uint64(n)) => *n,
        _ => 0,
    };
    assert_eq!(pub_ms_seq, 14);
}

#[tokio::test]
async fn test_quasar_industry_and_inventory() {
    // parity: feat:quasar.industry
    // parity: feat:quasar.inventory
    eve_testkit::attest(&["feat:quasar.industry", "feat:quasar.inventory"]);

    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(7002, 1, 90_000_001, 1_000_170);
    let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");

    // 1. Quasar Industry: 24 proto files in descriptor registry
    let ind_files = reg.files_by_domain.get("industry").expect("industry domain files");
    assert_eq!(ind_files.len(), 24, "Quasar industry domain must contain exactly 24 proto files");
    assert!(reg.files_by_proto_name.contains_key("eve/industry/activity.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/industry/facility.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/industry/job.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/industry/job/api/events.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/industry/job/manufacturing.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/industry/job/reaction.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/industry/reprocess/api/events.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/industry/system.proto"));

    // Dispatch industry facility GetRequest & system GetCostIndicesRequest
    let mut fac_req = Vec::new();
    encode_id_field(1, 60_003_760, &mut fac_req);
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.industry.facility.GetRequest", fac_req).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.industry.facility.GetResponse"));

    let mut sys_req = Vec::new();
    encode_id_field(1, 30_000_142, &mut sys_req);
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.industry.system.GetCostIndicesRequest", sys_req).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.industry.system.GetCostIndicesResponse"));

    // Industry job event publishing
    let mut installed_ev = Vec::new();
    let mut job_bytes = Vec::new();
    encode_varint_field(1, 101, &mut job_bytes);
    encode_bytes_field(1, &job_bytes, &mut installed_ev);
    quasar_publish_event(&ctx, &mut sess, "eve.industry.job.api.Installed", installed_ev).await;
    let events = ctx.quasar_gateway.published_events.read().unwrap();
    assert!(events.iter().any(|e| e.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve.industry.job.api.Installed"))));
    drop(events);

    // 2. Quasar Inventory: 21 proto files in descriptor registry
    let inv_files = reg.files_by_domain.get("inventory").expect("inventory domain files");
    assert_eq!(inv_files.len(), 21, "Quasar inventory domain must contain exactly 21 proto files");
    assert!(reg.files_by_proto_name.contains_key("eve/inventory/category.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/inventory/group.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/inventory/generic_item.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/inventory/generic_item_type.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/inventory/container/secure/auditlog.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/inventory/assetholding/operation/api/requests.proto"));

    // Dispatch inventory category, group, genericitemtype and genericname requests
    let mut cat_req = Vec::new();
    encode_id_field(1, 6, &mut cat_req); // Ship
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.inventory.category.GetRequest", cat_req).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.inventory.category.GetResponse"));

    let mut grp_req = Vec::new();
    encode_id_field(1, 25, &mut grp_req); // Frigate
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.inventory.group.GetRequest", grp_req).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.inventory.group.GetResponse"));

    let mut type_req = Vec::new();
    encode_id_field(1, 587, &mut type_req); // Rifter
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.inventory.genericitemtype.GetRequest", type_req).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.inventory.genericitemtype.GetResponse"));

    let mut name_req = Vec::new();
    encode_id_field(1, 587, &mut name_req);
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.inventory.genericname.GetNameByIDRequest", name_req).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.inventory.genericname.GetNameByIDResponse"));

    // Secure container auditlog Entry dynamic decode
    let mut entry_bytes = Vec::new();
    encode_id_field(2, 100_001, &mut entry_bytes); // container id
    encode_id_field(3, 3293, &mut entry_bytes);    // container type
    encode_id_field(7, 90_000_001, &mut entry_bytes); // character (field 7)
    let dyn_entry = reg.decode_dynamic("eve.inventory.container.secure.auditlog.Entry", &entry_bytes)
        .expect("decode auditlog entry");
    assert!(dyn_entry.get(2).is_some());
    assert!(dyn_entry.get(7).is_some());
}

#[tokio::test]
async fn test_quasar_isk_and_killmail() {
    // parity: feat:quasar.isk
    // parity: feat:quasar.killmail
    eve_testkit::attest(&["feat:quasar.isk", "feat:quasar.killmail"]);

    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(7003, 1, 90_000_001, 1_000_170);
    let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");

    // 1. Quasar ISK: 6 proto files in descriptor registry
    let isk_files = reg.files_by_domain.get("isk").expect("isk domain files");
    assert_eq!(isk_files.len(), 6, "Quasar ISK domain must contain exactly 6 proto files");
    assert!(reg.files_by_proto_name.contains_key("eve/isk/isk.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/isk/wallet/api/events.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/isk/wallet/api/requests.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/isk/wallet/wallet.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve_public/isk/isk.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve_public/isk/wallet/wallet.proto"));

    // Subscribe to ISK GrantCommanded notice
    subscribe_to_notices(&ctx, &mut sess, &["eve.isk.wallet.api.GrantCommanded"]).await;

    // Dispatch TransferByOwnerRequest
    let mut xfer_req = Vec::new();
    encode_varint_field(1, 1, &mut xfer_req); // from_wallet_owner (bool)
    encode_id_field(2, 90_000_001, &mut xfer_req); // acting character
    let mut to_owner = Vec::new();
    encode_id_field(1, 90_000_002, &mut to_owner); // character owner
    encode_bytes_field(4, &to_owner, &mut xfer_req); // transfer_to
    let mut curr_bytes = Vec::new();
    encode_varint_field(1, 50_000, &mut curr_bytes); // 50,000 ISK units
    encode_bytes_field(5, &curr_bytes, &mut xfer_req); // amount

    let rsp = quasar_rpc(&ctx, &mut sess, "eve.isk.wallet.api.TransferByOwnerRequest", xfer_req).await;
    assert_eq!(rsp.status_code, STATUS_OK, "TransferByOwnerRequest: {}", rsp.status_message);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.isk.wallet.api.TransferByOwnerResponse"));

    // Verify GrantCommanded notice delivery
    let notices = poll_notices(&ctx, &mut sess).await;
    assert!(
        notices.iter().any(|(t, _)| t.ends_with("eve.isk.wallet.api.GrantCommanded")),
        "Subscribed session must receive eve.isk.wallet.api.GrantCommanded notice"
    );

    // Dynamic decode model message eve.isk.wallet.Attributes
    let mut wallet_attr = Vec::new();
    let mut owner_bytes = Vec::new();
    encode_id_field(1, 90_000_001, &mut owner_bytes); // character owner
    encode_bytes_field(1, &owner_bytes, &mut wallet_attr);
    let mut bal_bytes = Vec::new();
    encode_varint_field(1, 9_950_000, &mut bal_bytes);
    encode_bytes_field(2, &bal_bytes, &mut wallet_attr);

    let dyn_wallet = reg.decode_dynamic("eve.isk.wallet.Attributes", &wallet_attr).expect("decode wallet attributes");
    assert!(dyn_wallet.get(1).is_some());
    assert!(dyn_wallet.get(2).is_some());

    // 2. Quasar Killmail: 2 proto files in descriptor registry
    let km_files = reg.files_by_domain.get("killmail").expect("killmail domain files");
    assert_eq!(km_files.len(), 2, "Quasar killmail domain must contain exactly 2 proto files");
    assert!(reg.files_by_proto_name.contains_key("eve/killmail/api/events.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/killmail/killmail.proto"));

    // Dispatch Killmail Attributes
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.killmail.Attributes", Vec::new()).await;
    assert_eq!(rsp.status_code, STATUS_OK, "Killmail Attributes: {}", rsp.status_message);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.killmail.Attributes"));

    // Publish eve.killmail.api.ShipLossReport event
    let mut loss_ev = Vec::new();
    encode_id_field(1, 100_001, &mut loss_ev); // killmail id
    encode_id_field(3, 30_000_142, &mut loss_ev); // solar system
    quasar_publish_event(&ctx, &mut sess, "eve.killmail.api.ShipLossReport", loss_ev).await;
    let events = ctx.quasar_gateway.published_events.read().unwrap();
    assert!(events.iter().any(|e| e.payload.as_ref().is_some_and(|p| p.type_url.ends_with("eve.killmail.api.ShipLossReport"))));
    drop(events);

    // Dynamic decode eve.killmail.Identifier
    let mut km_id_bytes = Vec::new();
    encode_varint_field(1, 987_654, &mut km_id_bytes);
    let dyn_km_id = reg.decode_dynamic("eve.killmail.Identifier", &km_id_bytes).expect("decode killmail identifier");
    let km_seq = match dyn_km_id.get(1) {
        Some(DynamicValue::Uint32(n)) => *n as u64,
        Some(DynamicValue::Uint64(n)) => *n,
        _ => 0,
    };
    assert_eq!(km_seq, 987_654);
}

#[tokio::test]
async fn test_quasar_legal_localization_and_loyaltypoints() {
    // parity: feat:quasar.legal
    // parity: feat:quasar.localization
    // parity: feat:quasar.loyaltypoints
    eve_testkit::attest(&["feat:quasar.legal", "feat:quasar.localization", "feat:quasar.loyaltypoints"]);

    let ctx = ServerContext::new();
    seed_test_fixtures(&ctx);
    let mut sess = make_session(7004, 1, 90_000_001, 1_000_170);
    let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");

    // 1. Quasar Legal: 2 proto files in descriptor registry
    let legal_files = reg.files_by_domain.get("legal").expect("legal domain files");
    assert_eq!(legal_files.len(), 2, "Quasar legal domain must contain exactly 2 proto files");
    assert!(reg.files_by_proto_name.contains_key("eve/legal/agreement/agreement.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/legal/agreement/api/requests.proto"));

    // Dispatch GetLatestRequest & AcceptRequest
    let mut eula_req = Vec::new();
    encode_varint_field(1, 1, &mut eula_req); // DOCUMENT_TYPE_EULA = 1
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.legal.agreement.api.GetLatestRequest", eula_req).await;
    assert_eq!(rsp.status_code, STATUS_OK, "GetLatestRequest failed: {}", rsp.status_message);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.legal.agreement.api.GetLatestResponse"));

    let mut accept_req = Vec::new();
    encode_id_field(1, 1, &mut accept_req); // user
    encode_varint_field(2, 1, &mut accept_req); // agreement_type DOCUMENT_TYPE_EULA = 1
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.user.legal.agreement.api.AcceptRequest", accept_req).await;
    assert_eq!(rsp.status_code, STATUS_OK, "AcceptRequest: {}", rsp.status_message);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.user.legal.agreement.api.AcceptResponse"));

    // 2. Quasar Localization: 7 proto files in descriptor registry
    let loc_files = reg.files_by_domain.get("localization").expect("localization domain files");
    assert_eq!(loc_files.len(), 7, "Quasar localization domain must contain exactly 7 proto files");
    assert!(reg.files_by_proto_name.contains_key("eve/localization/api/requests.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/localization/cerberus/api/requests.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/localization/cerberus/cerberus.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/localization/message.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve_public/localization/cerberus/cerberus.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve_public/localization/localization.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve_public/localization/message.proto"));

    // Dispatch localization requests
    let mut loc_req = Vec::new();
    encode_id_field(1, 1001, &mut loc_req);
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.localization.message.api.GetLocalizedTextRequest", loc_req).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.localization.message.api.GetLocalizedTextResponse"));

    let rsp = quasar_rpc(&ctx, &mut sess, "eve.localization.cerberus.api.GetLocalizedTextRequest", Vec::new()).await;
    assert_eq!(rsp.status_code, STATUS_OK);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.localization.cerberus.api.GetLocalizedTextResponse"));

    // Dynamic decode eve_public.localization.message.Identifier
    let mut loc_id_bytes = Vec::new();
    encode_varint_field(1, 42, &mut loc_id_bytes);
    let dyn_loc = reg.decode_dynamic("eve_public.localization.message.Identifier", &loc_id_bytes)
        .expect("decode localization message Identifier");
    let loc_seq = match dyn_loc.get(1) {
        Some(DynamicValue::Uint32(n)) => *n as u64,
        Some(DynamicValue::Uint64(n)) => *n,
        _ => 0,
    };
    assert_eq!(loc_seq, 42);

    // 3. Quasar LoyaltyPoints: 9 proto files in descriptor registry
    let lp_files = reg.files_by_domain.get("loyaltypoints").expect("loyaltypoints domain files");
    assert_eq!(lp_files.len(), 9, "Quasar loyaltypoints domain must contain exactly 9 proto files");
    assert!(reg.files_by_proto_name.contains_key("eve/loyaltypoints/api/events.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/loyaltypoints/evermarks/wallet/api/events.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/loyaltypoints/loyaltypoints.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/loyaltypoints/wallet/api/api.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/loyaltypoints/wallet/assetholding/operation/api/events.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/loyaltypoints/wallet/assetholding/operation/api/requests.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/loyaltypoints/wallet/assetholding/operation/operation.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve/loyaltypoints/wallet/wallet.proto"));
    assert!(reg.files_by_proto_name.contains_key("eve_public/loyaltypoints/loyaltypoints.proto"));

    // Subscribe to LP BalanceAdded notice
    subscribe_to_notices(&ctx, &mut sess, &["eve.loyaltypoints.wallet.api.BalanceAdded"]).await;

    // Deposit LP via operation API
    let dep_payload = make_lp_op_payload([0x33u8; 16], Some(90_000_001), 1000002, 5_000);
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.loyaltypoints.assetholding.operation.api.DepositRequest", dep_payload).await;
    assert_eq!(rsp.status_code, STATUS_OK, "DepositRequest: {}", rsp.status_message);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.loyaltypoints.assetholding.operation.api.DepositResponse"));

    // Verify BalanceAdded notice
    let notices = poll_notices(&ctx, &mut sess).await;
    assert!(
        notices.iter().any(|(t, _)| t.ends_with("eve.loyaltypoints.wallet.api.BalanceAdded")),
        "Subscribed session must receive eve.loyaltypoints.wallet.api.BalanceAdded notice"
    );

    // Withdraw LP via operation API
    let wd_payload = make_lp_op_payload([0x34u8; 16], Some(90_000_001), 1000002, 2_000);
    let rsp = quasar_rpc(&ctx, &mut sess, "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest", wd_payload).await;
    assert_eq!(rsp.status_code, STATUS_OK, "WithdrawRequest: {}", rsp.status_message);
    assert!(rsp.payload.as_ref().unwrap().type_url.ends_with("eve.loyaltypoints.assetholding.operation.api.WithdrawResponse"));
}
