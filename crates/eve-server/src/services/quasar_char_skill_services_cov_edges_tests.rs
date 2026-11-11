//! Deep edge, error, ACL, boundary, and protobuf wire decoding tests for `quasar_char_skill_services`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use eve_db::{
    InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository,
};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny,
    QuasarDescriptorRegistry, WireMessage, WireValue,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn new_router() -> (PublicGatewayRouter, Arc<WorldRepository>, Arc<SdeRegistry>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    (router, db, sde)
}

fn make_ctx(uid: Option<u32>, cid: Option<u32>, role: u64) -> AuthoritativeContext {
    let mut ctx = AuthoritativeContext::for_session(
        uid.map(|u| u as i32),
        cid,
        Some(98_000_001),
        role,
        "tranquility",
        vec![1],
    );
    ctx.service_role_mask = role;
    ctx
}

fn decode_wire_fields(bytes: &[u8]) -> Vec<(u32, WireValue)> {
    WireMessage::decode(bytes)
        .unwrap()
        .fields
        .into_iter()
        .map(|f| (f.field_number, f.value))
        .collect()
}

#[test]
fn extract_helpers_signed_nested_and_overflow_guards() {
    // Double-nested message (field 1 -> Message -> field 1 -> Uint64)
    let mut inner = DynamicQuasarMessage::new("Inner");
    inner.set(1, DynamicValue::Uint64(42_000));
    let mut mid = DynamicQuasarMessage::new("Mid");
    mid.set(1, DynamicValue::Message(Box::new(inner)));
    let mut outer = DynamicQuasarMessage::new("Outer");
    outer.set(1, DynamicValue::Message(Box::new(mid)));
    assert_eq!(extract_u64_checked(&outer, 1).unwrap(), Some(42_000));

    // Submessage with field 1 == None falls back to field 2
    let mut sub_f2 = DynamicQuasarMessage::new("SubF2");
    sub_f2.set(2, DynamicValue::Int32(-77));
    let mut msg_f2 = DynamicQuasarMessage::new("MsgF2");
    msg_f2.set(1, DynamicValue::Message(Box::new(sub_f2)));
    assert_eq!(extract_u64_checked(&msg_f2, 1).unwrap(), Some(77));

    // Signed negative Int64/Sint64/Sint32
    let mut signed_msg = DynamicQuasarMessage::new("Signed");
    signed_msg.set(1, DynamicValue::Sint64(-1234));
    signed_msg.set(2, DynamicValue::Sint32(-55));
    signed_msg.set(3, DynamicValue::Uint64(u64::from(u32::MAX) + 1));
    signed_msg.set(4, DynamicValue::Uint64(u64::MAX));
    signed_msg.set(5, DynamicValue::Uint64(0));
    assert_eq!(extract_required_u64(&signed_msg, 1, "x").unwrap(), 1234);
    assert_eq!(extract_required_u32(&signed_msg, 2, "x").unwrap(), 55);
    assert_eq!(
        extract_required_u32(&signed_msg, 3, "u32_ovf").unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        extract_required_i64(&signed_msg, 4, "i64_ovf").unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        extract_required_u64(&signed_msg, 5, "zero").unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        extract_required_u64(&signed_msg, 99, "missing").unwrap_err().0,
        STATUS_BAD_REQUEST
    );

    // Corrupt wire bytes decoded gracefully
    let bad_dyn = decode_wire_dynamic("bad.Msg", &[0xFF, 0xFF, 0xFF, 0x00]);
    assert!(bad_dyn.fields.is_empty());
}

#[test]
fn shipcaster_negative_branches_and_structure_acl_checks() {
    let (router, db, sde) = new_router();
    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let cid = 90_000_001_i64;
    let ship_id = 100_000_555_i64;
    let station_id = 60_003_760_i64;
    let hisec_sys = 30_000_142_u32;

    let sc = &handlers["eve.ship.api.CanJumpStarterSpaceShipCasterRequest"];
    let st_get = &handlers["eve.structure.api.GetRequest"];

    let player_ctx = make_ctx(Some(1), Some(cid as u32), 0);
    let unauth_ctx = make_ctx(None, None, 0);
    let admin_no_char = make_ctx(Some(1), None, u64::MAX);
    let empty_msg = DynamicQuasarMessage::new("Empty");

    // 1. Corrupt cluster_misc_state JSON -> can_jump returns 0
    db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.cluster_misc_state = None;
    });
    let r1 = sc(&player_ctx, &dummy_any, &empty_msg).unwrap();
    assert_eq!(decode_wire_fields(&r1.value)[0].1, WireValue::Varint(0));

    // 2. Valid starter_launchers, but docked ship (location_id == station_id) -> 0
    db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.cluster_misc_state = serde_json::from_str(r#"{"starter_launchers":[1]}"#).ok();
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.solar_system_id = hisec_sys;
            ch.active_ship_id = ship_id;
            ch.jump_cooldown_until_unix = 0;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        decode_wire_fields(&sc(&player_ctx, &dummy_any, &empty_msg).unwrap().value)[0].1,
        WireValue::Varint(0)
    );

    // 3. active_ship_id == 0 -> 0; missing character -> 0
    db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().active_ship_id = 0;
    });
    assert_eq!(
        decode_wire_fields(&sc(&player_ctx, &dummy_any, &empty_msg).unwrap().value)[0].1,
        WireValue::Varint(0)
    );
    let missing_char_ctx = make_ctx(Some(999), Some(99_999_999), 0);
    assert_eq!(
        decode_wire_fields(&sc(&missing_char_ctx, &dummy_any, &empty_msg).unwrap().value)[0].1,
        WireValue::Varint(0)
    );

    // 4. Destroyed shipcaster structure fallback -> 0
    let mut sde_mut = (*sde).clone();
    let mut sc_type = sde.types.values().next().unwrap().clone();
    sc_type.type_id = 81_001;
    sc_type.name = "Caldari Shipcaster".into();
    sde_mut.types.insert(81_001, sc_type);
    let router2 = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::new(sde_mut),
    );
    db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.cluster_misc_state = None;
        snap.characters.get_mut(&cid).unwrap().active_ship_id = ship_id;
        snap.items.get_mut(&ship_id).unwrap().location_id = i64::from(hisec_sys);
        snap.structures.insert(
            1_000_000_811,
            UpwellStructureRecord {
                structure_id: 1_000_000_811,
                type_id: 81_001,
                owner_corp_id: 98_000_001,
                solar_system_id: hisec_sys,
                name: "Dead Shipcaster".into(),
                state: "Destroyed".into(),
                ..Default::default()
            },
        );
    });
    let h2 = router2.handlers.read().unwrap();
    assert_eq!(
        decode_wire_fields(
            &h2["eve.ship.api.CanJumpStarterSpaceShipCasterRequest"](
                &player_ctx,
                &dummy_any,
                &empty_msg
            )
            .unwrap()
            .value
        )[0]
            .1,
        WireValue::Varint(0)
    );

    // 5. eve.structure.api.GetRequest: unauth, missing id, ACL denied vs allowed, admin without char
    let st_id = 1_000_000_991_i64;
    let owner_corp = db.with_snapshot(|s| s.characters[&cid].corporation_id);
    db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35832,
                owner_corp_id: 98_999_999, // different corp -> ACL denies cid
                solar_system_id: hisec_sys,
                name: "Private Astrahus".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });
    let mut st_msg = DynamicQuasarMessage::new("StGet");
    st_msg.set(1, DynamicValue::Int64(st_id));
    assert_eq!(st_get(&unauth_ctx, &dummy_any, &st_msg).unwrap_err().0, STATUS_UNAUTHORIZED);
    assert_eq!(st_get(&player_ctx, &dummy_any, &empty_msg).unwrap_err().0, STATUS_BAD_REQUEST);
    assert_eq!(st_get(&player_ctx, &dummy_any, &st_msg).unwrap_err().0, STATUS_NOT_FOUND);
    assert!(st_get(&admin_no_char, &dummy_any, &st_msg).is_ok());

    // Change structure owner_corp_id to player's corp -> non-admin ACL allows & response encodes expected fields
    db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&st_id).unwrap().owner_corp_id = owner_corp;
    });
    let st_ok = st_get(&player_ctx, &dummy_any, &st_msg).unwrap();
    let outer = decode_wire_fields(&st_ok.value);
    assert_eq!(outer[0].0, 1);
    if let WireValue::LengthDelimited(ref attrs) = outer[0].1 {
        let a_fields = decode_wire_fields(attrs);
        assert!(a_fields.iter().any(|(fnr, v)| *fnr == 4 && matches!(v, WireValue::LengthDelimited(b) if b == b"Private Astrahus")));
    } else {
        panic!("expected length-delimited attrs");
    }
}

#[test]
fn stargate_locks_sov_acls_agents_and_monolith_edges() {
    let (router, db, sde) = new_router();
    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let cid = 90_000_001_u32;
    let player_ctx = make_ctx(Some(1), Some(cid), 0);
    let unauth_ctx = make_ctx(None, None, 0);
    let zarzakh = 30_100_000_u32;

    // 1. Stargate lock unauth, req_cid == 0 allowed, expired lock ignored, blocked payload decoded
    let get_lock = &handlers["eve.stargate.lock.api.GetRequest"];
    let val_jump = &handlers["eve.stargate.lock.api.ValidateJumpRequest"];
    let mut lock_msg = DynamicQuasarMessage::new("Lock");
    lock_msg.set(1, DynamicValue::Uint64(0));
    lock_msg.set(2, DynamicValue::Uint32(zarzakh));
    lock_msg.set(3, DynamicValue::Uint64(50_000_222));

    assert_eq!(get_lock(&unauth_ctx, &dummy_any, &lock_msg).unwrap_err().0, STATUS_UNAUTHORIZED);
    assert_eq!(val_jump(&unauth_ctx, &dummy_any, &lock_msg).unwrap_err().0, STATUS_UNAUTHORIZED);

    let now = player_ctx.now_unix();
    db.with_mut_snapshot(|snap| {
        snap.zarzakh_gate_locks.insert(i64::from(cid), (50_000_111, now)); // expired (exp <= now)
    });
    assert_eq!(get_lock(&player_ctx, &dummy_any, &lock_msg).unwrap_err().0, STATUS_NOT_FOUND);
    assert_eq!(
        decode_wire_fields(&val_jump(&player_ctx, &dummy_any, &lock_msg).unwrap().value)[0].1,
        WireValue::Varint(1)
    );

    let exp_future = now + 1_200;
    db.with_mut_snapshot(|snap| {
        snap.zarzakh_gate_locks
            .insert(i64::from(cid), (50_000_111, exp_future));
    });
    let get_ok = get_lock(&player_ctx, &dummy_any, &lock_msg).unwrap();
    let get_fields = decode_wire_fields(&get_ok.value);
    assert_eq!(get_fields.len(), 2);

    let blocked = val_jump(&player_ctx, &dummy_any, &lock_msg).unwrap();
    let b_fields = decode_wire_fields(&blocked.value);
    assert_eq!(b_fields[0], (1, WireValue::Varint(0)));
    assert_eq!(b_fields[1].0, 2);

    // 2. Sovereignty unclaimed system & ADM fallback to 10 when no system_states entry
    let unclaimed_sys = sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| sid != 30_000_142)
        .unwrap_or(30_000_142);
    db.with_mut_snapshot(|snap| {
        snap.system_states.remove(&unclaimed_sys);
    });
    let mut sys_msg = DynamicQuasarMessage::new("Sys");
    sys_msg.set(1, DynamicValue::Uint32(unclaimed_sys));
    let claim_unclaimed = handlers["eve.sovereignty.api.GetClaimRequest"](
        &player_ctx,
        &dummy_any,
        &sys_msg,
    )
    .unwrap();
    assert_eq!(decode_wire_fields(&claim_unclaimed.value), vec![(1, WireValue::Varint(1))]);

    let adm_default = handlers["eve.sovereignty.activitydefensemodifier.api.GetLevelRequest"](
        &player_ctx,
        &dummy_any,
        &sys_msg,
    )
    .unwrap();
    assert_eq!(decode_wire_fields(&adm_default.value), vec![(1, WireValue::Varint(10))]);

    // 3. Agent SearchRequest Bytes & empty query
    let first_agent = sde.agents.values().next().expect("SDE has agents");
    let tok = first_agent.name.split_whitespace().next().unwrap_or("a");
    let mut ag_bytes = DynamicQuasarMessage::new("SearchBytes");
    ag_bytes.set(1, DynamicValue::Bytes(tok.as_bytes().to_vec()));
    let sb_res = handlers["eve.agent.SearchRequest"](&player_ctx, &dummy_any, &ag_bytes).unwrap();
    assert!(!sb_res.value.is_empty());

    let mut sub_bytes = DynamicQuasarMessage::new("SubBytes");
    sub_bytes.set(1, DynamicValue::Bytes(tok.as_bytes().to_vec()));
    let mut ag_nested_bytes = DynamicQuasarMessage::new("SearchNestedBytes");
    ag_nested_bytes.set(1, DynamicValue::Message(Box::new(sub_bytes)));
    assert!(!handlers["eve.agent.SearchRequest"](&player_ctx, &dummy_any, &ag_nested_bytes)
        .unwrap()
        .value
        .is_empty());

    let ag_empty = DynamicQuasarMessage::new("EmptySearch");
    assert!(handlers["eve.agent.SearchRequest"](&player_ctx, &dummy_any, &ag_empty)
        .unwrap()
        .value
        .is_empty());

    // 4. Corrupt quasar_acl_registry falls back to default ACLs 1 & 2, and GetManagedRequest filters by role 3|4
    db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.acl_registry = None;
    });
    let mut mg_msg = DynamicQuasarMessage::new("Managed");
    mg_msg.set(1, DynamicValue::Uint64(90_000_001));
    let mg1 = handlers["eve.acl.api.GetManagedRequest"](&player_ctx, &dummy_any, &mg_msg).unwrap();
    assert_eq!(decode_wire_fields(&mg1.value).len(), 2);

    mg_msg.set(1, DynamicValue::Uint64(90_000_002));
    let mg2 = handlers["eve.acl.api.GetManagedRequest"](&player_ctx, &dummy_any, &mg_msg).unwrap();
    assert!(mg2.value.is_empty());

    // 5. Missing required args on FW corp stats & offer GetRequest -> STATUS_BAD_REQUEST
    let empty_msg = DynamicQuasarMessage::new("Empty");
    assert_eq!(
        handlers["eve.factionwarfare.stats.corporation.GetRequest"](&player_ctx, &dummy_any, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve.fulfillment.offer.api.GetRequest"](&player_ctx, &dummy_any, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // 6. RewardDeliveryRequest explicit overrides vs fallback & monolith.character.location structure fallback
    let mut rd_msg = DynamicQuasarMessage::new("RewardDelivery");
    rd_msg.set(1, DynamicValue::Uint64(888));
    rd_msg.set(2, DynamicValue::Uint64(999));
    let rd_res = handlers["eve.fulfillment.RewardDeliveryRequest"](&player_ctx, &dummy_any, &rd_msg).unwrap();
    assert_eq!(decode_wire_fields(&rd_res.value).len(), 4);

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&i64::from(cid)) {
            ch.station_id = None;
            ch.structure_id = Some(1_000_000_991);
            ch.solar_system_id = 30_000_142;
        }
    });
    let loc_res = handlers["eve.monolith.character.location.GetRequest"](
        &player_ctx,
        &dummy_any,
        &empty_msg,
    )
    .unwrap();
    let loc_fields = decode_wire_fields(&loc_res.value);
    assert_eq!(loc_fields.len(), 3);
    if let WireValue::LengthDelimited(ref b) = loc_fields[2].1 {
        assert_eq!(decode_wire_fields(b)[0], (1, WireValue::Varint(1_000_000_991)));
    } else {
        panic!("expected structure_id submessage");
    }
}
