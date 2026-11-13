//! Unit tests for `quasar_assetholding_entitlements` and `quasar_entitlement_ledger`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
    STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::pb;
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
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
    pb::message("eve.assetholding.entitlement.Identifier")
        .set("holder.character.sequential", holder as u32)
        .set("asset.uuid", uuid.to_vec())
        .set("group_by.name", group_name)
        .must_encode()
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

    let spawn_req = pb::message("eve.assetholding.asset.api.SpawnRequest")
        .set("internal_name", "tritanium-pack")
        .set_with("itemstack", |p| {
            p.add_with("stacks", |s| {
                s.set("type.sequential", 34u32).set("quantity", 20u32)
            })
        })
        .must_encode();
    let spawn_rsp = call_rsp(&router, &gm, "eve.assetholding.asset.api.SpawnRequest", spawn_req, now);
    assert_eq!(spawn_rsp.status_code, STATUS_OK);

    let asset_uuid = super::super::quasar_entitlement_ledger::load(&router.db)
        .assets
        .values()
        .next()
        .expect("spawned asset")
        .uuid;

    // HoldRequest (stack) + CreateRequest (character_title)
    let hold_req = pb::message("eve.assetholding.asset.api.HoldRequest")
        .set_with("itemstack", |hp| {
            hp.set_with("by_typed_quantities", |tq| {
                tq.add_with("items", |s| {
                    s.set("type.sequential", 34u32).set("quantity", 20u32)
                })
            })
            .set("desired_capacity", 4u64)
        })
        .must_encode();
    assert_eq!(call(&router, &gm, "eve.assetholding.asset.api.HoldRequest", hold_req, now), STATUS_OK);

    let create_req = pb::message("eve.assetholding.asset.api.CreateRequest")
        .set("unit.unit_character_title.title.uuid", super::super::quasar_cosmetics_drafts::id_to_uuid_bytes(42))
        .set("finite", 2u64)
        .must_encode();
    assert_eq!(call(&router, &gm, "eve.assetholding.asset.api.CreateRequest", create_req, now), STATUS_OK);

    // 2. Asset GetRequest (internal + public: missing -> 400, unknown -> 404, valid -> 200)
    assert_eq!(call(&router, &player, "eve.assetholding.asset.api.GetRequest", Vec::new(), now), STATUS_BAD_REQUEST);
    assert_eq!(call(&router, &player, "eve_public.assetholding.asset.api.GetRequest", Vec::new(), now), STATUS_BAD_REQUEST);
    let bad_get = pb::message("eve.assetholding.asset.api.GetRequest")
        .set("asset.uuid", vec![0xFFu8; 16])
        .must_encode();
    assert_eq!(call(&router, &player, "eve.assetholding.asset.api.GetRequest", bad_get.clone(), now), STATUS_NOT_FOUND);
    assert_eq!(call(&router, &player, "eve_public.assetholding.asset.api.GetRequest", bad_get, now), STATUS_NOT_FOUND);

    let good_get = pb::message("eve.assetholding.asset.api.GetRequest")
        .set("asset.uuid", asset_uuid.to_vec())
        .must_encode();
    assert_eq!(call(&router, &player, "eve.assetholding.asset.api.GetRequest", good_get.clone(), now), STATUS_OK);
    assert_eq!(call(&router, &player, "eve_public.assetholding.asset.api.GetRequest", good_get, now), STATUS_OK);

    // 3. DirectDeliveryRequest + GetUnredeemed + Get + Redeem + Delete
    let eid = build_ent_id(holder_cid, &asset_uuid, "promo");
    let deliv_req = pb::message("eve.assetholding.entitlement.api.DirectDeliveryRequest")
        .add_with("entitlements", |entry| {
            entry.set_raw_message("entitlement", eid.clone()).set("quantity", 3u64)
        })
        .must_encode();
    assert_eq!(call(&router, &gm, "eve.assetholding.entitlement.api.DirectDeliveryRequest", deliv_req, now), STATUS_OK);

    assert_eq!(call(&router, &unauth, "eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest", Vec::new(), now), STATUS_UNAUTHORIZED);
    let unred_other = pb::message("eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest")
        .set("holder.character.sequential", other_cid as u32)
        .must_encode();
    assert_eq!(call(&router, &player, "eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest", unred_other, now), STATUS_FORBIDDEN);

    let unred_self = pb::message("eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest")
        .set("group_by_name", "promo")
        .must_encode();
    assert_eq!(call(&router, &player, "eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest", unred_self, now), STATUS_OK);

    let get_ent = pb::message("eve_public.assetholding.entitlement.api.GetRequest")
        .set_raw_message("entitlement", eid)
        .must_encode();
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
    let spawn_req = pb::message("eve.assetholding.asset.api.SpawnRequest")
        .set("internal_name", "tritanium-edge")
        .set_with("itemstack", |p| {
            p.add_with("stacks", |s| {
                s.set("type.sequential", 34u32).set("quantity", 20u32)
            })
        })
        .must_encode();
    assert_eq!(call(&router, &gm, "eve.assetholding.asset.api.SpawnRequest", spawn_req, now), STATUS_OK);

    let asset_uuid = super::super::quasar_entitlement_ledger::load(&router.db)
        .assets
        .values()
        .next()
        .expect("asset")
        .uuid;

    // Fully redeem a 2-count entitlement for group "done" and grant active 3-count for group "promo"
    let eid_done = build_ent_id(holder_cid, &asset_uuid, "done");
    let deliv_done = pb::message("eve.assetholding.entitlement.api.DirectDeliveryRequest")
        .add_with("entitlements", |entry| {
            entry.set_raw_message("entitlement", eid_done.clone()).set("quantity", 2u64)
        })
        .must_encode();
    assert_eq!(call(&router, &gm, "eve.assetholding.entitlement.api.DirectDeliveryRequest", deliv_done, now), STATUS_OK);
    let red_done = pb::message("eve_public.assetholding.entitlement.api.RedeemRequest")
        .set_raw_message("entitlement", eid_done)
        .must_encode();
    let player = make_sess(Some(1), Some(holder_cid as i64), service_roles::ROLE_PLAYER_DEFAULT);
    assert_eq!(call(&router, &player, "eve_public.assetholding.entitlement.api.RedeemRequest", red_done, now), STATUS_OK);
    assert!(super::super::quasar_entitlement_ledger::load(&router.db)
        .entitlements
        .values()
        .any(|e| e.group_name == "done" && e.redeemed == e.total));

    let eid_promo = build_ent_id(holder_cid, &asset_uuid, "promo");
    let deliv_promo = pb::message("eve.assetholding.entitlement.api.DirectDeliveryRequest")
        .add_with("entitlements", |entry| {
            entry.set_raw_message("entitlement", eid_promo).set("quantity", 3u64)
        })
        .must_encode();
    assert_eq!(call(&router, &gm, "eve.assetholding.entitlement.api.DirectDeliveryRequest", deliv_promo, now), STATUS_OK);

    // Grant entitlement held directly by uid_only (caller_cid == None && caller_uid == Some(4242))
    let eid_uid = build_ent_id(uid_only, &asset_uuid, "uidgrp");
    let deliv_uid = pb::message("eve.assetholding.entitlement.api.DirectDeliveryRequest")
        .add_with("entitlements", |entry| {
            entry.set_raw_message("entitlement", eid_uid).set("quantity", 1u64)
        })
        .must_encode();
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
