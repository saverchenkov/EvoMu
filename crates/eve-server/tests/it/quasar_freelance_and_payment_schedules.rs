//! End-to-end remediation integration tests for Quasar Freelance, Payment Schedules/Tokens,
//! Purchase, and New Eden Store (`quasar:*` fake_done batch 2 closure).

#![forbid(unsafe_code)]

use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag64,
    EvePublicRequest, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_server::router::ServerContext;
use eve_server::services::quasar_payment_store_types::{
    add_user_quick_pay_token, process_due_payment_schedules, PaymentTokenRecord, StoreCatalog,
    StoreCategory, StoreOffer, save_store_catalog,
};
use eve_server::services::quasar_social_freelance::QuasarFreelanceState;
use std::sync::Arc;

fn make_session(session_id: u64, user_id: i32, char_id: u32, corp_id: u32, is_director: bool) -> SessionState {
    let mut s = SessionState::new(session_id);
    s.user_id = Some(user_id as u64);
    s.character_id = Some(char_id as i64);
    s.corporation_id = Some(corp_id as i64);
    s.solar_system_id = Some(30_000_142);
    s.role = if is_director {
        u64::MAX
    } else {
        eve_machonet::service_roles::ROLE_PLAYER
    };
    s.corp_role = if is_director {
        eve_db::corp_roles::ROLE_DIRECTOR
    } else {
        0
    };
    s
}

fn quasar_call(
    ctx: &ServerContext,
    sess: &SessionState,
    type_name: &str,
    value: Vec<u8>,
) -> eve_proto::quasar::EvePublicResponse {
    let now = ctx.now_unix();
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-batch2-0001".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-batch2-001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    ctx.quasar_gateway
        .handle_public_request(sess, &req, None, false, now)
}

fn encode_id_msg(field_num: u32, id: u64) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_varint_field(1, id, &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(field_num, &inner, &mut out);
    out
}

fn extract_created_project(rsp: &eve_proto::quasar::EvePublicResponse) -> (Vec<u8>, Vec<u8>) {
    let payload = rsp.payload.as_ref().expect("CreateResponse payload");
    let wm = eve_proto::quasar::WireMessage::decode(&payload.value).expect("decode CreateResponse");
    let created_bytes = wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    let created_wm = eve_proto::quasar::WireMessage::decode(created_bytes).expect("decode created");
    let id_bytes = created_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap().to_vec();
    let id_wm = eve_proto::quasar::WireMessage::decode(&id_bytes).expect("decode Identifier");
    let uuid = id_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap().to_vec();
    let mut req_bytes = Vec::new();
    encode_bytes_field(1, &id_bytes, &mut req_bytes);
    (req_bytes, uuid)
}

fn poll_notice_names(ctx: &ServerContext, sess: &SessionState) -> Vec<String> {
    let pending: Vec<_> = ctx
        .quasar_gateway
        .pending_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect();
    let now = ctx.now_unix();
    for (scope, payload) in pending {
        let _ = ctx.quasar_gateway.publish_notice(scope, payload, now);
    }
    ctx.quasar_gateway
        .poll_notices(sess.session_id)
        .into_iter()
        .filter_map(|n| n.payload.map(|p| p.normalized_type_name().to_string()))
        .collect()
}

#[tokio::test]
async fn test_quasar_batch2_freelance_projects_and_item_delivery_e2e() {
    // Regression only; freelance rows are attested by the live player-session suite in quasar_freelance_and_corporation_live.rs.

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.db.with_mut_snapshot(|s| {
        s.service_state_blobs.remove("nes_store_catalog");
    });
    let _reg = QuasarDescriptorRegistry::open_default().expect("quasar descriptors");
    let now = ctx.now_unix();

    let corp_id = 98_600_111_u32;
    let director_cid = 90_000_001_u32;
    let worker_cid = 90_000_002_u32;
    let jita_station = 60_003_760_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id as i64;
        corp.name = "Freelance E2E Corp".into();
        corp.ceo_id = director_cid as i64;
        corp.master_wallet = IskCents::from_isk(10_000_000);
        snap.corporations.insert(corp_id as i64, corp);

        if let Some(ch) = snap.characters.get_mut(&(director_cid as i64)) {
            ch.corporation_id = corp_id as i64;
            ch.station_id = Some(jita_station);
        }
        if let Some(ch) = snap.characters.get_mut(&(worker_cid as i64)) {
            ch.corporation_id = corp_id as i64;
            ch.station_id = Some(jita_station);
        }
        snap.items.insert(
            888_777_666,
            InventoryItemRecord {
                item_id: 888_777_666,
                type_id: 34, // Tritanium
                owner_id: worker_cid as i64,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let dir_sess = make_session(201, 1, director_cid, corp_id, true);
    let wrk_sess = make_session(202, 2, worker_cid, corp_id, false);
    ctx.quasar_gateway.subscribe_session(&dir_sess, vec![], now);
    ctx.quasar_gateway.subscribe_session(&wrk_sess, vec![], now);

    // 1. CreateRequest with explicit 1,000,000 ISK budget (100,000 ISK/unit * 10 target)
    let mut create_req = Vec::new();
    encode_string_field(1, "Tritanium Haul", &mut create_req);
    encode_string_field(2, "Deliver 10 Tritanium", &mut create_req);
    encode_varint_field(3, 10, &mut create_req); // target_progress = 10
    let (mut cfg_b, mut param_b, mut opts_b, mut mod_b) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "34", &mut opts_b); // required_type_id = 34 (Tritanium)
    encode_bytes_field(3, &opts_b, &mut param_b);
    encode_bytes_field(3, &param_b, &mut cfg_b);
    encode_bytes_field(6, &cfg_b, &mut create_req);
    encode_varint_field(4, 5, &mut mod_b); // batch_size = 5
    encode_bytes_field(7, &mod_b, &mut create_req);
    let mut isk_curr = Vec::new();
    encode_varint_field(1, encode_zigzag64(100_000), &mut isk_curr);
    let mut rew_msg = Vec::new();
    encode_bytes_field(1, &isk_curr, &mut rew_msg);
    encode_bytes_field(10, &rew_msg, &mut create_req);

    let create_rsp = quasar_call(&ctx, &dir_sess, "eve_public.freelance.project.api.CreateRequest", create_req);
    assert_eq!(create_rsp.status_code, STATUS_OK, "{}", create_rsp.status_message);
    let (proj_req, proj_uuid) = extract_created_project(&create_rsp);

    // Verify corp wallet debited by 1,000,000 ISK and CreatedNotice emitted
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corporations[&(corp_id as i64)].master_wallet, IskCents::from_isk(9_000_000));
    });
    let dir_notices = poll_notice_names(&ctx, &dir_sess);
    assert!(dir_notices.iter().any(|n| n == "eve_public.freelance.project.api.CreatedNotice"));

    // 2. Query endpoints: GetRequest, GetForManagerRequest, GetAllActiveRequest, GetAllActiveForCorporationRequest, GetAllBroadcastedRequest
    assert_eq!(quasar_call(&ctx, &wrk_sess, "eve_public.freelance.project.api.GetRequest", proj_req.clone()).status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &dir_sess, "eve_public.freelance.project.api.GetForManagerRequest", proj_req.clone()).status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &wrk_sess, "eve_public.freelance.project.api.GetAllActiveRequest", Vec::new()).status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &wrk_sess, "eve_public.freelance.project.api.GetAllActiveForCorporationRequest", encode_id_msg(1, corp_id as u64)).status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &wrk_sess, "eve_public.freelance.project.api.GetAllBroadcastedRequest", Vec::new()).status_code, STATUS_OK);

    // 3. CommitRequest -> ParticipantAddedNotice
    let commit_rsp = quasar_call(&ctx, &wrk_sess, "eve_public.freelance.project.api.CommitRequest", proj_req.clone());
    assert_eq!(commit_rsp.status_code, STATUS_OK);
    let dir_notices = poll_notice_names(&ctx, &dir_sess);
    assert!(dir_notices.iter().any(|n| n == "eve_public.freelance.project.api.ParticipantAddedNotice"));
    assert_eq!(quasar_call(&ctx, &wrk_sess, "eve_public.freelance.project.api.GetAllCommittedRequest", Vec::new()).status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &wrk_sess, "eve_public.freelance.project.api.GetParticipationDetailsRequest", proj_req.clone()).status_code, 403);
    assert_eq!(quasar_call(&ctx, &dir_sess, "eve_public.freelance.project.api.GetParticipationDetailsRequest", proj_req.clone()).status_code, STATUS_OK);

    // 4. DeliverRequest -> deposits into CorpDeliveries hangar, emits ContributionSucceededNotice, ProgressedNotice, CompletedNotice
    let mut deliver_req = proj_req.clone();
    encode_varint_field(4, 10, &mut deliver_req);
    let deliver_rsp = quasar_call(&ctx, &wrk_sess, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", deliver_req);
    assert_eq!(deliver_rsp.status_code, STATUS_OK, "{}", deliver_rsp.status_message);

    // Verify items moved from worker Hangar (100 -> 90) to CorpDeliveries (10)
    ctx.db.with_snapshot(|snap| {
        let worker_qty: i32 = snap.items.values()
            .filter(|it| it.owner_id == worker_cid as i64 && it.flag == InventoryFlag::Hangar && it.type_id == 34)
            .map(|it| it.quantity)
            .sum();
        let corp_deliv_qty: i32 = snap.items.values()
            .filter(|it| it.owner_id == corp_id as i64 && it.flag == InventoryFlag::CorpDeliveries && it.type_id == 34)
            .map(|it| it.quantity)
            .sum();
        assert_eq!(worker_qty, 90);
        assert_eq!(corp_deliv_qty, 10);
    });

    let wrk_notices = poll_notice_names(&ctx, &wrk_sess);
    assert!(wrk_notices.iter().any(|n| n == "eve_public.freelance.contributionmethod.itemdelivery.api.ContributionSucceededNotice"));
    let dir_notices = poll_notice_names(&ctx, &dir_sess);
    assert!(dir_notices.iter().any(|n| n == "eve_public.freelance.project.api.ProgressedNotice"));
    assert!(dir_notices.iter().any(|n| n == "eve_public.freelance.project.api.CompletedNotice"));

    // GetStatusRequest on itemdelivery
    let status_rsp = quasar_call(&ctx, &wrk_sess, "eve_public.freelance.contributionmethod.itemdelivery.api.GetStatusRequest", proj_req.clone());
    assert_eq!(status_rsp.status_code, STATUS_OK);

    // 5. RedeemRewardRequest -> ParticipantRewardRedeemedNotice, GetAllUnredeemedRequest, GetStatsRequest
    assert_eq!(quasar_call(&ctx, &wrk_sess, "eve_public.freelance.project.api.GetAllUnredeemedRequest", Vec::new()).status_code, STATUS_OK);
    let redeem_rsp = quasar_call(&ctx, &wrk_sess, "eve_public.freelance.project.api.RedeemRewardRequest", proj_req.clone());
    assert_eq!(redeem_rsp.status_code, STATUS_OK);
    let wrk_notices = poll_notice_names(&ctx, &wrk_sess);
    assert!(wrk_notices.iter().any(|n| n == "eve_public.freelance.project.api.ParticipantRewardRedeemedNotice"));
    assert_eq!(quasar_call(&ctx, &dir_sess, "eve_public.freelance.project.api.GetStatsRequest", proj_req).status_code, STATUS_OK);

    // 6. Verify persistence across QuasarFreelanceState::from_db
    let reloaded = QuasarFreelanceState::from_db(Arc::clone(&ctx.db));
    let projs = reloaded.projects.read().unwrap();
    assert_eq!(projs.get(&proj_uuid).map(|p| p.state), Some(2));
    drop(projs);

    // 7. Second project: KickRequest, ResignRequest, CloseRequest, GetAllInactiveRequest, DeleteRequest -> AclDeletedNotice
    let mut create_req2 = Vec::new();
    encode_string_field(1, "Second Project", &mut create_req2);
    encode_string_field(2, "Test lifecycle", &mut create_req2);
    encode_varint_field(3, 5, &mut create_req2);
    encode_bytes_field(6, &cfg_b, &mut create_req2);
    encode_bytes_field(7, &mod_b, &mut create_req2);
    encode_bytes_field(10, &rew_msg, &mut create_req2);
    let c2_rsp = quasar_call(&ctx, &dir_sess, "eve_public.freelance.project.api.CreateRequest", create_req2);
    assert_eq!(c2_rsp.status_code, STATUS_OK);
    let (proj2_req, _uuid2) = extract_created_project(&c2_rsp);

    assert_eq!(quasar_call(&ctx, &wrk_sess, "eve_public.freelance.project.api.CommitRequest", proj2_req.clone()).status_code, STATUS_OK);
    let mut char_id_inner = Vec::new();
    encode_varint_field(1, worker_cid as u64, &mut char_id_inner);
    let mut kick_req = proj2_req.clone();
    encode_bytes_field(2, &char_id_inner, &mut kick_req);
    assert_eq!(quasar_call(&ctx, &dir_sess, "eve_public.freelance.project.api.KickRequest", kick_req).status_code, STATUS_OK);
    let wrk_notices = poll_notice_names(&ctx, &wrk_sess);
    assert!(wrk_notices.iter().any(|n| n == "eve_public.freelance.project.api.ParticipantKickedNotice"));

    assert_eq!(quasar_call(&ctx, &wrk_sess, "eve_public.freelance.project.api.CommitRequest", proj2_req.clone()).status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &wrk_sess, "eve_public.freelance.project.api.ResignRequest", proj2_req.clone()).status_code, STATUS_OK);
    let dir_notices = poll_notice_names(&ctx, &dir_sess);
    assert!(dir_notices.iter().any(|n| n == "eve_public.freelance.project.api.ParticipantRemovedNotice"));

    assert_eq!(quasar_call(&ctx, &dir_sess, "eve_public.freelance.project.api.CloseRequest", proj2_req.clone()).status_code, STATUS_OK);
    let dir_notices = poll_notice_names(&ctx, &dir_sess);
    assert!(dir_notices.iter().any(|n| n == "eve_public.freelance.project.api.ClosedNotice"));
    assert_eq!(quasar_call(&ctx, &dir_sess, "eve_public.freelance.project.api.GetAllInactiveRequest", Vec::new()).status_code, STATUS_OK);
}

#[tokio::test]
async fn test_quasar_batch2_payment_schedules_and_tokens_e2e() {

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.db.with_mut_snapshot(|s| {
        s.service_state_blobs.remove("nes_store_catalog");
    });
    let reg = QuasarDescriptorRegistry::open_default().expect("quasar descriptors");
    let now = ctx.now_unix();

    // Seed Omega offer with explicit price_cents in nes_store_catalog
    ctx.db.with_mut_snapshot(|snap| {
        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: vec![StoreCategory {
                    id: 1,
                    name: "Omega".into(),
                    description: "Omega".into(),
                }],
                products: vec![],
                offers: vec![StoreOffer {
                    id: 1001,
                    name: "1 Month Omega".into(),
                    description: "30 Days Omega".into(),
                    category_id: 1,
                    price_plex: 500,
                    product_ids: vec![],
                    targeted: false,
                    price_cents: Some(1_999),
                    currency: Some("USD".into()),
                }],
            },
        );
    });

    let unauth = SessionState::new(300);
    assert_eq!(quasar_call(&ctx, &unauth, "eve_public.payment.token.api.GetQuickPayRequest", Vec::new()).status_code, STATUS_UNAUTHORIZED);
    assert_eq!(quasar_call(&ctx, &unauth, "eve_public.payment.token.api.GetRequest", encode_id_msg(1, 1)).status_code, STATUS_UNAUTHORIZED);
    assert_eq!(quasar_call(&ctx, &unauth, "eve_public.payment.token.api.DisableAllRequest", Vec::new()).status_code, STATUS_UNAUTHORIZED);
    assert_eq!(quasar_call(&ctx, &unauth, "eve_public.payment.schedule.api.GetRequest", encode_id_msg(1, 1)).status_code, STATUS_UNAUTHORIZED);
    assert_eq!(quasar_call(&ctx, &unauth, "eve_public.payment.schedule.api.GetActiveByTypeRequest", Vec::new()).status_code, STATUS_UNAUTHORIZED);

    let sess = make_session(301, 1, 90_000_001, 1_000_009, false);
    let other_sess = make_session(302, 2, 90_000_002, 1_000_009, false);
    ctx.quasar_gateway.subscribe_session(&sess, vec![], now);

    // 1. Add quick-pay token -> QuickPayAddedNotice
    add_user_quick_pay_token(
        &ctx.db,
        &ctx.quasar_gateway.pending_notices,
        1,
        PaymentTokenRecord {
            id: 55,
            user_id: 1,
            token_str: "tok_visa_4242".into(),
            is_quick_pay: true,
            consumed: false,
        },
    );
    let notices = poll_notice_names(&ctx, &sess);
    assert!(notices.iter().any(|n| n == "eve_public.payment.token.api.QuickPayAddedNotice"));

    // 2. GetQuickPayRequest & GetRequest
    let qp_rsp = quasar_call(&ctx, &sess, "eve_public.payment.token.api.GetQuickPayRequest", Vec::new());
    assert_eq!(qp_rsp.status_code, STATUS_OK);
    let tok_get = quasar_call(&ctx, &sess, "eve_public.payment.token.api.GetRequest", encode_id_msg(1, 55));
    assert_eq!(tok_get.status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &other_sess, "eve_public.payment.token.api.GetRequest", encode_id_msg(1, 55)).status_code, STATUS_NOT_FOUND);

    // 3. CostRequest & TokenRequest -> creates schedule & emits SetNotice
    let mut cost_req = Vec::new();
    encode_varint_field(1, 1001, &mut cost_req);
    let cost_rsp = quasar_call(&ctx, &sess, "eve_public.payment.purchase.api.CostRequest", cost_req);
    assert_eq!(cost_rsp.status_code, STATUS_OK);

    let mut off_id = Vec::new();
    encode_string_field(1, "1001", &mut off_id);
    let mut order = Vec::new();
    encode_bytes_field(1, &off_id, &mut order);
    encode_varint_field(2, 1, &mut order);
    let mut tok_id = Vec::new();
    encode_varint_field(1, 55, &mut tok_id);
    let mut tok_req = Vec::new();
    encode_bytes_field(1, &order, &mut tok_req);
    encode_bytes_field(2, &tok_id, &mut tok_req);
    let purch_rsp = quasar_call(&ctx, &sess, "eve_public.payment.purchase.api.TokenRequest", tok_req);
    assert_eq!(purch_rsp.status_code, STATUS_OK, "{}", purch_rsp.status_message);

    let notices = poll_notice_names(&ctx, &sess);
    assert!(notices.iter().any(|n| n == "eve_public.payment.schedule.api.SetNotice"));

    // 4. payment.schedule.api.GetActiveByTypeRequest & GetRequest
    let mut by_type_req = Vec::new();
    encode_varint_field(1, 1, &mut by_type_req);
    let active_rsp = quasar_call(&ctx, &sess, "eve_public.payment.schedule.api.GetActiveByTypeRequest", by_type_req);
    assert_eq!(active_rsp.status_code, STATUS_OK);
    let active_dyn = reg.decode_dynamic(
        "eve_public.payment.schedule.api.GetActiveByTypeResponse",
        &active_rsp.payload.as_ref().unwrap().value,
    ).unwrap();
    let sched_id = match active_dyn.get(1) {
        Some(eve_proto::quasar::DynamicValue::Message(m)) => match m.get(1) {
            Some(eve_proto::quasar::DynamicValue::Uint64(id)) => *id,
            _ => panic!("missing sched id"),
        },
        _ => panic!("missing sched ident"),
    };
    assert_eq!(quasar_call(&ctx, &sess, "eve_public.payment.schedule.api.GetRequest", encode_id_msg(1, sched_id)).status_code, STATUS_OK);

    // 5. DisableAllRequest -> QuickPayRemovedNotice, then due schedule tick -> CancelledNotice
    assert_eq!(quasar_call(&ctx, &sess, "eve_public.payment.token.api.DisableAllRequest", Vec::new()).status_code, STATUS_OK);
    let notices = poll_notice_names(&ctx, &sess);
    assert!(notices.iter().any(|n| n == "eve_public.payment.token.api.QuickPayRemovedNotice"));

    process_due_payment_schedules(&ctx.db, &ctx.quasar_gateway.pending_notices, now + 31 * 86_400);
    let notices = poll_notice_names(&ctx, &sess);
    assert!(notices.iter().any(|n| n == "eve_public.payment.schedule.api.CancelledNotice"));
}

#[tokio::test]
async fn test_quasar_batch2_store_catalog_and_target_audience_e2e() {

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.db.with_mut_snapshot(|s| {
        s.service_state_blobs.remove("nes_store_catalog");
    });
    let reg = QuasarDescriptorRegistry::open_default().expect("quasar descriptors");
    let admin = make_session(401, 1, 90_000_001, 1_000_009, true);

    // 1. Create Category, Product (SDE type 40519 Skill Extractor), and Offers via live admin requests
    let mut cat_attr = Vec::new();
    encode_string_field(1, "Services", &mut cat_attr);
    let mut cat_req = Vec::new();
    encode_bytes_field(1, &cat_attr, &mut cat_req);
    assert_eq!(quasar_call(&ctx, &admin, "eve.store.admin.api.CreateCategoryRequest", cat_req).status_code, STATUS_OK);

    let mut prod_attr = Vec::new();
    encode_string_field(1, "Skill Extractor", &mut prod_attr);
    let mut eve_item = Vec::new();
    encode_bytes_field(1, &encode_id_msg(1, 40_519)[2..], &mut eve_item);
    encode_varint_field(2, 1, &mut eve_item);
    encode_bytes_field(2, &eve_item, &mut prod_attr);
    let mut prod_req = Vec::new();
    encode_varint_field(1, 1, &mut prod_req); // catalog = 1 (Ingame)
    encode_bytes_field(2, &prod_attr, &mut prod_req);
    assert_eq!(quasar_call(&ctx, &admin, "eve.store.admin.api.CreateProductRequest", prod_req).status_code, STATUS_OK);

    for (name, targeted) in [("Regular Offer", false), ("Targeted Offer A", true), ("Targeted Offer B", true)] {
        let mut off_attr = Vec::new();
        encode_string_field(1, name, &mut off_attr);
        let mut price = Vec::new();
        encode_varint_field(1, 15_000, &mut price); // 150 PLEX
        encode_bytes_field(5, &price, &mut off_attr);
        if targeted {
            encode_varint_field(7, 1, &mut off_attr);
        }
        encode_string_field(16, "featured", &mut off_attr);
        encode_bytes_field(17, &encode_id_msg(1, 1)[2..], &mut off_attr);
        let mut off_req = Vec::new();
        encode_bytes_field(1, &off_attr, &mut off_req);
        assert_eq!(quasar_call(&ctx, &admin, "eve.store.admin.api.CreateOfferRequest", off_req).status_code, STATUS_OK);
    }

    // 2. Category, Product, Offer queries
    assert_eq!(quasar_call(&ctx, &admin, "eve_public.store.category.api.GetAllRequest", Vec::new()).status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &admin, "eve_public.store.category.api.GetRequest", encode_id_msg(1, 1)).status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &admin, "eve_public.store.category.api.GetRequest", encode_id_msg(1, 999)).status_code, STATUS_NOT_FOUND);
    assert_eq!(quasar_call(&ctx, &admin, "eve_public.store.product.api.GetRequest", encode_id_msg(1, 101)).status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &admin, "eve_public.store.offer.api.GetAllRequest", Vec::new()).status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &admin, "eve_public.store.offer.api.GetAllTargetedRequest", Vec::new()).status_code, STATUS_OK);

    let mut off_inner = Vec::new();
    encode_string_field(1, "1001", &mut off_inner);
    let mut off_get = Vec::new();
    encode_bytes_field(1, &off_inner, &mut off_get);
    assert_eq!(quasar_call(&ctx, &admin, "eve_public.store.offer.api.GetRequest", off_get).status_code, STATUS_OK);

    let mut tag_req = Vec::new();
    encode_string_field(2, "featured", &mut tag_req);
    assert_eq!(quasar_call(&ctx, &admin, "eve_public.store.offer.api.GetAllByTagRequest", tag_req).status_code, STATUS_OK);

    // 3. Target-audience ListRequest pagination (page.size = 1 -> next_page.token = "1") & GetRequest
    let mut page1 = Vec::new();
    encode_varint_field(1, 1, &mut page1);
    let mut list_req1 = Vec::new();
    encode_bytes_field(1, &page1, &mut list_req1);
    let list_rsp1 = quasar_call(&ctx, &admin, "eve_public.store.targetaudience.offer.api.ListRequest", list_req1);
    assert_eq!(list_rsp1.status_code, STATUS_OK);
    let list_dyn1 = reg.decode_dynamic(
        "eve_public.store.targetaudience.offer.api.ListResponse",
        &list_rsp1.payload.as_ref().unwrap().value,
    ).unwrap();
    let next_tok = match list_dyn1.get(2) {
        Some(eve_proto::quasar::DynamicValue::Message(np)) => match np.get(1) {
            Some(eve_proto::quasar::DynamicValue::String(s)) => s.clone(),
            _ => panic!("missing next_page token"),
        },
        _ => panic!("missing next_page"),
    };
    assert_eq!(next_tok, "1");

    let mut page2 = Vec::new();
    encode_varint_field(1, 1, &mut page2);
    encode_string_field(2, &next_tok, &mut page2);
    let mut list_req2 = Vec::new();
    encode_bytes_field(1, &page2, &mut list_req2);
    let list_rsp2 = quasar_call(&ctx, &admin, "eve_public.store.targetaudience.offer.api.ListRequest", list_req2);
    assert_eq!(list_rsp2.status_code, STATUS_OK);
    let list_dyn2 = reg.decode_dynamic(
        "eve_public.store.targetaudience.offer.api.ListResponse",
        &list_rsp2.payload.as_ref().unwrap().value,
    ).unwrap();
    let page2_offer_id = match list_dyn2.get(1) {
        Some(eve_proto::quasar::DynamicValue::Repeated(items)) => match items.first() {
            Some(eve_proto::quasar::DynamicValue::Message(ident)) => match ident.get(2) {
                Some(eve_proto::quasar::DynamicValue::Message(off)) => match off.get(1) {
                    Some(eve_proto::quasar::DynamicValue::String(s)) => s.clone(),
                    _ => panic!("missing offer_id string"),
                },
                _ => panic!("missing offer_id"),
            },
            _ => panic!("missing item"),
        },
        _ => panic!("missing items"),
    };
    assert_eq!(page2_offer_id, "1003");

    let (mut u_b, mut o_b, mut ta_ident) = (Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, 1, &mut u_b);
    encode_string_field(1, "1002", &mut o_b);
    encode_bytes_field(1, &u_b, &mut ta_ident);
    encode_bytes_field(2, &o_b, &mut ta_ident);
    let mut ta_get_req = Vec::new();
    encode_bytes_field(1, &ta_ident, &mut ta_get_req);
    assert_eq!(quasar_call(&ctx, &admin, "eve_public.store.targetaudience.offer.api.GetRequest", ta_get_req).status_code, STATUS_OK);
    assert_eq!(quasar_call(&ctx, &admin, "eve_public.store.targetaudience.offer.api.GetRequest", Vec::new()).status_code, STATUS_BAD_REQUEST);
}
