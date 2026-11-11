//! Unit tests for `quasar_assetholding_entitlements` and `quasar_entitlement_ledger`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
    STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn new_router() -> PublicGatewayRouter {
    let reg = QuasarDescriptorRegistry::open_default_or_empty();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(reg.clone(), Arc::clone(&db), shared_sde());
    register_assetholding_handlers(&router, db, shared_sde(), reg);
    router
}

fn make_sess(uid: Option<u64>, cid: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(88);
    s.user_id = uid;
    s.character_id = cid;
    s.corporation_id = Some(98_000_001);
    s.role = role;
    s
}

fn call_rsp(
    router: &PublicGatewayRouter,
    sess: &SessionState,
    type_name: &str,
    payload: Vec<u8>,
    now: u64,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        correlation_uuid: vec![1; 16],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload,
        }),
        ..Default::default()
    };
    router.handle_public_request(sess, &req, None, false, now)
}

fn call(router: &PublicGatewayRouter, sess: &SessionState, type_name: &str, payload: Vec<u8>, now: u64) -> u32 {
    call_rsp(router, sess, type_name, payload, now).status_code
}

fn build_ent_id(holder: u64, uuid: &[u8], group_name: &str) -> Vec<u8> {
    let mut ch = Vec::new();
    encode_varint_field(1, holder, &mut ch);
    let mut h = Vec::new();
    encode_bytes_field(1, &ch, &mut h);
    let mut a = Vec::new();
    encode_bytes_field(1, uuid, &mut a);
    let mut g = Vec::new();
    encode_string_field(1, group_name, &mut g);
    let mut eid = Vec::new();
    encode_bytes_field(1, &h, &mut eid);
    encode_bytes_field(2, &a, &mut eid);
    encode_bytes_field(3, &g, &mut eid);
    eid
}

#[test]
fn asset_spawn_hold_create_lookup_and_entitlement_redemption_lifecycle() {
    let router = new_router();
    let holder_cid = 90_000_001u64;
    let other_cid = 90_000_055u64;
    router.db.with_mut_snapshot(|s| {
        let mut ch2 = s.characters[&(holder_cid as i64)].clone();
        ch2.character_id = other_cid as i64;
        ch2.user_id = 55;
        s.characters.insert(other_cid as i64, ch2);
    });

    let gm = make_sess(Some(1), Some(holder_cid as i64), service_roles::ROLE_GM_DEFAULT);
    let player = make_sess(Some(1), Some(holder_cid as i64), service_roles::ROLE_PLAYER_DEFAULT);
    let unauth = make_sess(None, None, 0);
    let other = make_sess(Some(55), Some(other_cid as i64), service_roles::ROLE_PLAYER_DEFAULT);
    let now = 1_750_000_000u64;

    // 1. SpawnRequest validation & creation (stack + capacity)
    assert_eq!(call(&router, &player, "eve.assetholding.asset.api.SpawnRequest", Vec::new(), now), STATUS_FORBIDDEN);
    assert_eq!(call(&router, &gm, "eve.assetholding.asset.api.SpawnRequest", Vec::new(), now), STATUS_BAD_REQUEST);

    let mut spawn_req = Vec::new();
    encode_string_field(2, "tritanium-pack", &mut spawn_req);
    let (mut t_sub, mut st_sub, mut stacks_sub) = (Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, 34, &mut t_sub);
    encode_bytes_field(1, &t_sub, &mut st_sub);
    encode_varint_field(2, 20, &mut st_sub);
    encode_bytes_field(1, &st_sub, &mut stacks_sub);
    encode_bytes_field(6, &stacks_sub, &mut spawn_req);
    let spawn_rsp = call_rsp(&router, &gm, "eve.assetholding.asset.api.SpawnRequest", spawn_req, now);
    assert_eq!(spawn_rsp.status_code, STATUS_OK);

    let asset_uuid = super::super::quasar_entitlement_ledger::load(&router.db)
        .assets
        .values()
        .next()
        .expect("spawned asset")
        .uuid;

    // HoldRequest (stack) + CreateRequest (character_title)
    let (mut typed, mut stack_hp, mut hold_req) = (Vec::new(), Vec::new(), Vec::new());
    encode_bytes_field(1, &st_sub, &mut typed);
    encode_bytes_field(1, &typed, &mut stack_hp);
    encode_varint_field(7, 4, &mut stack_hp);
    encode_bytes_field(7, &stack_hp, &mut hold_req);
    assert_eq!(call(&router, &gm, "eve.assetholding.asset.api.HoldRequest", hold_req, now), STATUS_OK);

    let mut create_req = Vec::new();
    let (mut u_sub, mut t_id_sub, mut title_u) = (Vec::new(), Vec::new(), Vec::new());
    encode_bytes_field(
        1,
        &super::super::quasar_cosmetics_drafts::id_to_uuid_bytes(42),
        &mut t_id_sub,
    );
    encode_bytes_field(1, &t_id_sub, &mut title_u);
    encode_bytes_field(12, &title_u, &mut u_sub);
    encode_bytes_field(4, &u_sub, &mut create_req);
    encode_varint_field(6, 2, &mut create_req);
    assert_eq!(call(&router, &gm, "eve.assetholding.asset.api.CreateRequest", create_req, now), STATUS_OK);

    // 2. Asset GetRequest (internal + public: missing -> 400, unknown -> 404, valid -> 200)
    assert_eq!(call(&router, &player, "eve.assetholding.asset.api.GetRequest", Vec::new(), now), STATUS_BAD_REQUEST);
    assert_eq!(call(&router, &player, "eve_public.assetholding.asset.api.GetRequest", Vec::new(), now), STATUS_BAD_REQUEST);
    let (mut bad_uuid, mut bad_get) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &[0xFF; 16], &mut bad_uuid);
    encode_bytes_field(1, &bad_uuid, &mut bad_get);
    assert_eq!(call(&router, &player, "eve.assetholding.asset.api.GetRequest", bad_get.clone(), now), STATUS_NOT_FOUND);
    assert_eq!(call(&router, &player, "eve_public.assetholding.asset.api.GetRequest", bad_get, now), STATUS_NOT_FOUND);

    let (mut good_uuid, mut good_get) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &asset_uuid, &mut good_uuid);
    encode_bytes_field(1, &good_uuid, &mut good_get);
    assert_eq!(call(&router, &player, "eve.assetholding.asset.api.GetRequest", good_get.clone(), now), STATUS_OK);
    assert_eq!(call(&router, &player, "eve_public.assetholding.asset.api.GetRequest", good_get, now), STATUS_OK);

    // 3. DirectDeliveryRequest + GetUnredeemed + Get + Redeem + Delete
    let eid = build_ent_id(holder_cid, &asset_uuid, "promo");
    let mut deliv_entry = Vec::new();
    encode_bytes_field(2, &eid, &mut deliv_entry);
    encode_varint_field(10, 3, &mut deliv_entry);
    let mut deliv_req = Vec::new();
    encode_bytes_field(2, &deliv_entry, &mut deliv_req);
    assert_eq!(call(&router, &gm, "eve.assetholding.entitlement.api.DirectDeliveryRequest", deliv_req, now), STATUS_OK);

    assert_eq!(call(&router, &unauth, "eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest", Vec::new(), now), STATUS_UNAUTHORIZED);
    let (mut ch_other, mut h_other, mut unred_other) = (Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, other_cid, &mut ch_other);
    encode_bytes_field(1, &ch_other, &mut h_other);
    encode_bytes_field(1, &h_other, &mut unred_other);
    assert_eq!(call(&router, &player, "eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest", unred_other, now), STATUS_FORBIDDEN);

    let mut unred_self = Vec::new();
    encode_string_field(2, "promo", &mut unred_self);
    assert_eq!(call(&router, &player, "eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest", unred_self, now), STATUS_OK);

    let mut get_ent = Vec::new();
    encode_bytes_field(1, &eid, &mut get_ent);
    assert_eq!(call(&router, &other, "eve_public.assetholding.entitlement.api.GetRequest", get_ent.clone(), now), STATUS_FORBIDDEN);
    assert_eq!(call(&router, &player, "eve_public.assetholding.entitlement.api.GetRequest", get_ent.clone(), now), STATUS_OK);
    assert_eq!(call(&router, &player, "eve.assetholding.entitlement.api.GetRequest", get_ent.clone(), now), STATUS_OK);

    assert_eq!(call(&router, &player, "eve_public.assetholding.entitlement.api.RedeemRequest", get_ent.clone(), now), STATUS_OK);

    // Non-admin DeleteRequest on partially redeemed entitlement fails; RedeliverStale + AutoRedeem cleans up!
    assert_eq!(call(&router, &player, "eve.assetholding.entitlement.api.DeleteRequest", get_ent.clone(), now), STATUS_FORBIDDEN);
    assert_eq!(call(&router, &gm, "eve.assetholding.entitlement.api.admin.RedeliverStaleEntitlementsRequest", Vec::new(), now), STATUS_OK);
    assert_eq!(call(&router, &gm, "eve_public.assetholding.entitlement.api.admin.AutoRedeemRequest", Vec::new(), now), STATUS_OK);
    assert_eq!(call(&router, &gm, "eve.assetholding.entitlement.api.admin.ExpireRequest", get_ent, now), STATUS_OK);

    // 4. Direct notice helpers deliver to subscribed character session (+ cached notices replayed on subscribe)
    let sub_sess = make_sess(Some(1), Some(holder_cid as i64), service_roles::ROLE_PLAYER_DEFAULT);
    let mut rx = router.subscribe_session(&sub_sess, Vec::new(), now);
    assert_eq!(publish_asset_capacity_claimed_notice(&router, holder_cid as u32, 1, 10, 9, now).unwrap(), 1);
    assert_eq!(publish_asset_deleted_notice(&router, holder_cid as u32, 1, now).unwrap(), 1);
    let mut delivered = 0usize;
    while rx.try_recv().is_ok() {
        delivered += 1;
    }
    assert_eq!(delivered, 5);
}

#[test]
fn extract_u64_matrix_empty_asset_uuids_and_unredeemed_holder_filters() {
    use eve_proto::quasar::AuthoritativeContext;

    // 1. Direct extract_u64_from_field variant matrix (top-level & nested Message)
    let mut deeply_nested = DynamicQuasarMessage::new("Deep");
    deeply_nested.set(1, DynamicValue::Uint32(42));

    let mut sub_msg_u64 = DynamicQuasarMessage::new("SubU64");
    sub_msg_u64.set(1, DynamicValue::Uint64(99));
    let mut sub_msg_u32 = DynamicQuasarMessage::new("SubU32");
    sub_msg_u32.set(1, DynamicValue::Uint32(77));
    let mut sub_msg_i64 = DynamicQuasarMessage::new("SubI64");
    sub_msg_i64.set(1, DynamicValue::Int64(88));
    let mut sub_msg_i32 = DynamicQuasarMessage::new("SubI32");
    sub_msg_i32.set(1, DynamicValue::Int32(66));
    let mut sub_msg_nested = DynamicQuasarMessage::new("SubNested");
    sub_msg_nested.set(1, DynamicValue::Message(Box::new(deeply_nested)));
    let mut sub_msg_bool = DynamicQuasarMessage::new("SubBool");
    sub_msg_bool.set(1, DynamicValue::Bool(true));

    let mut msg = DynamicQuasarMessage::new("TestHolderMsg");
    msg.set(1, DynamicValue::Message(Box::new(sub_msg_u64)));
    msg.set(2, DynamicValue::Message(Box::new(sub_msg_u32)));
    msg.set(3, DynamicValue::Message(Box::new(sub_msg_i64)));
    msg.set(4, DynamicValue::Message(Box::new(sub_msg_i32)));
    msg.set(5, DynamicValue::Message(Box::new(sub_msg_nested)));
    msg.set(6, DynamicValue::Message(Box::new(sub_msg_bool)));
    msg.set(7, DynamicValue::Uint64(1234));
    msg.set(8, DynamicValue::Uint32(567));
    msg.set(9, DynamicValue::Int64(890));
    msg.set(10, DynamicValue::Int32(321));
    msg.set(11, DynamicValue::String("ignored".into()));

    assert_eq!(extract_u64_from_field(&msg, 1), 99);
    assert_eq!(extract_u64_from_field(&msg, 2), 77);
    assert_eq!(extract_u64_from_field(&msg, 3), 88);
    assert_eq!(extract_u64_from_field(&msg, 4), 66);
    assert_eq!(extract_u64_from_field(&msg, 5), 42);
    assert_eq!(extract_u64_from_field(&msg, 6), 0);
    assert_eq!(extract_u64_from_field(&msg, 7), 1234);
    assert_eq!(extract_u64_from_field(&msg, 8), 567);
    assert_eq!(extract_u64_from_field(&msg, 9), 890);
    assert_eq!(extract_u64_from_field(&msg, 10), 321);
    assert_eq!(extract_u64_from_field(&msg, 11), 0);
    assert_eq!(extract_u64_from_field(&msg, 99), 0);

    // 2. Invoke handlers directly & via gateway for empty UUIDs and unredeemed edge cases
    let router = new_router();
    let holder_cid = 90_000_001u64;
    let other_cid = 90_000_055u64;
    let uid_only = 4242u64;
    let now = 1_750_000_500u64;
    router.db.with_mut_snapshot(|s| {
        let mut ch_uid = s.characters[&(holder_cid as i64)].clone();
        ch_uid.character_id = uid_only as i64;
        ch_uid.user_id = uid_only;
        s.characters.insert(uid_only as i64, ch_uid);
    });

    let gm = make_sess(Some(1), Some(holder_cid as i64), service_roles::ROLE_GM_DEFAULT);

    // Spawn two assets & grant two entitlements: one in "promo" (partially redeemed),
    // one in "full" (fully redeemed -> total == redeemed, must be excluded from GetUnredeemed),
    // plus one belonging to uid_only so user-fallback path returns real results.
    let mut spawn_req = Vec::new();
    encode_string_field(2, "tritanium-edge", &mut spawn_req);
    let (mut t_sub, mut st_sub, mut stacks_sub) = (Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, 34, &mut t_sub);
    encode_bytes_field(1, &t_sub, &mut st_sub);
    encode_varint_field(2, 20, &mut st_sub);
    encode_bytes_field(1, &st_sub, &mut stacks_sub);
    encode_bytes_field(6, &stacks_sub, &mut spawn_req);
    assert_eq!(call(&router, &gm, "eve.assetholding.asset.api.SpawnRequest", spawn_req, now), STATUS_OK);

    let asset_uuid = super::super::quasar_entitlement_ledger::load(&router.db)
        .assets
        .values()
        .next()
        .expect("asset")
        .uuid;

    // Fully redeem a 2-count entitlement for group "done" and grant active 3-count for group "promo"
    let eid_done = build_ent_id(holder_cid, &asset_uuid, "done");
    let mut deliv_done = Vec::new();
    let mut ent_done = Vec::new();
    encode_bytes_field(2, &eid_done, &mut ent_done);
    encode_varint_field(10, 2, &mut ent_done);
    encode_bytes_field(2, &ent_done, &mut deliv_done);
    assert_eq!(call(&router, &gm, "eve.assetholding.entitlement.api.DirectDeliveryRequest", deliv_done, now), STATUS_OK);
    let mut red_done = Vec::new();
    encode_bytes_field(1, &eid_done, &mut red_done);
    let player = make_sess(Some(1), Some(holder_cid as i64), service_roles::ROLE_PLAYER_DEFAULT);
    assert_eq!(call(&router, &player, "eve_public.assetholding.entitlement.api.RedeemRequest", red_done, now), STATUS_OK);
    assert!(super::super::quasar_entitlement_ledger::load(&router.db)
        .entitlements
        .values()
        .any(|e| e.group_name == "done" && e.redeemed == e.total));

    let eid_promo = build_ent_id(holder_cid, &asset_uuid, "promo");
    let mut deliv_promo = Vec::new();
    let mut ent_promo = Vec::new();
    encode_bytes_field(2, &eid_promo, &mut ent_promo);
    encode_varint_field(10, 3, &mut ent_promo);
    encode_bytes_field(2, &ent_promo, &mut deliv_promo);
    assert_eq!(call(&router, &gm, "eve.assetholding.entitlement.api.DirectDeliveryRequest", deliv_promo, now), STATUS_OK);

    // Grant entitlement held directly by uid_only (caller_cid == None && caller_uid == Some(4242))
    let eid_uid = build_ent_id(uid_only, &asset_uuid, "uidgrp");
    let mut deliv_uid = Vec::new();
    let mut ent_uid = Vec::new();
    encode_bytes_field(2, &eid_uid, &mut ent_uid);
    encode_varint_field(10, 1, &mut ent_uid);
    encode_bytes_field(2, &ent_uid, &mut deliv_uid);
    assert_eq!(call(&router, &gm, "eve.assetholding.entitlement.api.DirectDeliveryRequest", deliv_uid, now), STATUS_OK);

    let dummy_any = GoogleAny::default();
    let handlers = router.handlers.read().unwrap();

    // Asset GetRequest (internal & public) with empty Bytes(vec![]) inside Identifier
    let mut inner_empty = DynamicQuasarMessage::new("Identifier");
    inner_empty.set(1, DynamicValue::Bytes(Vec::new()));
    let mut get_empty = DynamicQuasarMessage::new("GetRequest");
    get_empty.set(1, DynamicValue::Message(Box::new(inner_empty)));
    let p_ctx = AuthoritativeContext::for_session(Some(1), Some(holder_cid as u32), None, 0, "evomu", vec![]);
    assert_eq!(
        handlers["eve.assetholding.asset.api.GetRequest"](&p_ctx, &dummy_any, &get_empty)
            .expect_err("empty uuid -> 400")
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.assetholding.asset.api.GetRequest"](&p_ctx, &dummy_any, &get_empty)
            .expect_err("public empty uuid -> 400")
            .0,
        STATUS_BAD_REQUEST
    );

    let unred_h = &handlers["eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest"];

    // holder_raw > u32::MAX -> Invalid holder ID
    let mut overflow_msg = DynamicQuasarMessage::new("GetUnredeemedByHolderAndGroupNameRequest");
    overflow_msg.set(1, DynamicValue::Uint64(1u64 << 40));
    assert_eq!(unred_h(&p_ctx, &dummy_any, &overflow_msg).expect_err("overflow holder").0, STATUS_BAD_REQUEST);

    // Direct handler with no caller_cid, no caller_uid, non-admin -> 403 Authentication required
    let empty_ctx = AuthoritativeContext::for_session(None, None, None, 0, "evomu", vec![]);
    let empty_msg = DynamicQuasarMessage::new("GetUnredeemedByHolderAndGroupNameRequest");
    assert_eq!(unred_h(&empty_ctx, &dummy_any, &empty_msg).expect_err("unauth").0, STATUS_FORBIDDEN);

    // Admin with no caller_cid/uid and req_holder == 0 -> 400 Holder ID required
    let mut admin_no_id_ctx = AuthoritativeContext::for_session(None, None, None, 0, "evomu", vec![]);
    admin_no_id_ctx.service_role_mask = service_roles::ROLE_ADMIN;
    assert_eq!(unred_h(&admin_no_id_ctx, &dummy_any, &empty_msg).expect_err("holder required").0, STATUS_BAD_REQUEST);

    // Admin querying another holder's entitlements -> 200 OK
    let mut admin_full_ctx = AuthoritativeContext::for_session(Some(1), Some(other_cid as u32), None, 0, "evomu", vec![]);
    admin_full_ctx.service_role_mask = service_roles::ROLE_ADMIN;
    let mut req_holder_msg = DynamicQuasarMessage::new("GetUnredeemedByHolderAndGroupNameRequest");
    req_holder_msg.set(1, DynamicValue::Uint64(holder_cid));
    let rsp_admin = unred_h(&admin_full_ctx, &dummy_any, &req_holder_msg).expect("admin queries holder");
    assert!(!rsp_admin.value.is_empty());

    // group_filter == Some("") matches unredeemed "promo" and excludes fully redeemed "done"
    req_holder_msg.set(2, DynamicValue::String(String::new()));
    let rsp_empty_grp = unred_h(&p_ctx, &dummy_any, &req_holder_msg).expect("empty group filter");
    // Compare against filtering exclusively for "done" (fully redeemed -> empty payload)
    req_holder_msg.set(2, DynamicValue::String("done".into()));
    let rsp_done_only = unred_h(&p_ctx, &dummy_any, &req_holder_msg).expect("done excluded");
    assert!(!rsp_empty_grp.value.is_empty());
    assert!(rsp_done_only.value.is_empty());

    // caller_cid == None && caller_uid == Some(uid_only) fallback returns uid_only's entitlement
    let uid_ctx = AuthoritativeContext::for_session(Some(uid_only as i32), None, None, 0, "evomu", vec![]);
    let rsp_uid = unred_h(&uid_ctx, &dummy_any, &empty_msg).expect("uid fallback");
    assert!(!rsp_uid.value.is_empty());
}
