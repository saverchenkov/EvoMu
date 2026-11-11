//! Extra unit and mutation-killing tests for `services::quasar_cosmetics_drafts`.

#![forbid(unsafe_code)]

use super::super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
};
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, EvePublicRequest, EvePublicResponse, GoogleAny,
    QuasarDescriptorRegistry, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn sess(uid: Option<u64>, cid: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(uid.unwrap_or(1) * 100 + cid.unwrap_or(0) as u64);
    s.user_id = uid;
    s.character_id = cid;
    s.corporation_id = Some(98_000_001);
    s.role = role;
    s
}

fn call(
    router: &PublicGatewayRouter,
    s: &SessionState,
    ty: &str,
    value: Vec<u8>,
    now: u64,
) -> EvePublicResponse {
    router.handle_public_request(
        s,
        &EvePublicRequest {
            correlation_uuid: vec![19; 16],
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{ty}"),
                value,
            }),
            ..Default::default()
        },
        None,
        false,
        now,
    )
}

#[test]
fn extract_draft_id_and_design_parameters_all_dynamic_and_wire_branches() {
    // 1. extract_draft_id_or_uuid: all DynamicValue variants on field 1
    for (val, expected_id) in [
        (DynamicValue::Uint64(51), Some(51)),
        (DynamicValue::Int64(52), Some(52)),
        (DynamicValue::Uint32(53), Some(53)),
        (DynamicValue::Int32(54), Some(54)),
        (DynamicValue::Bytes(id_to_uuid_bytes(55).to_vec()), Some(55)),
        (DynamicValue::Bytes(vec![1, 2]), None), // < 4 bytes -> id == 0 -> None
    ] {
        let mut d = DynamicQuasarMessage::new("test.Msg");
        d.set(1, val);
        let (u, id) = extract_draft_id_or_uuid(&d, &[]);
        assert!(u.is_some());
        assert_eq!(id, expected_id);
    }

    // Message(m) with Uint64 at field 1, and Message(m) with short Bytes (id == 0 -> None)
    let mut sub_u = DynamicQuasarMessage::new("sub");
    sub_u.set(1, DynamicValue::Uint64(88));
    let mut d_msg = DynamicQuasarMessage::new("msg");
    d_msg.set(1, DynamicValue::Message(Box::new(sub_u)));
    assert_eq!(extract_draft_id_or_uuid(&d_msg, &[]).1, Some(88));

    let mut sub_short = DynamicQuasarMessage::new("sub");
    sub_short.set(1, DynamicValue::Bytes(vec![7, 8]));
    d_msg.set(1, DynamicValue::Message(Box::new(sub_short)));
    let (u_short, id_short) = extract_draft_id_or_uuid(&d_msg, &[]);
    assert_eq!(u_short, Some(vec![7, 8]));
    assert_eq!(id_short, None);

    // Raw wire fallback when field 1 bytes are NOT a valid nested protobuf message
    let mut raw_direct = Vec::new();
    let non_proto_uuid = [0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 99];
    encode_bytes_field(1, &non_proto_uuid, &mut raw_direct);
    let empty_d = DynamicQuasarMessage::new("empty");
    let (u_raw, id_raw) = extract_draft_id_or_uuid(&empty_d, &raw_direct);
    assert_eq!(u_raw, Some(non_proto_uuid.to_vec()));
    assert_eq!(id_raw, Some(99));

    // 2. extract_design_parameters: DynamicQuasarMessage branches (layout at field 4, editor_cfgs at field 5)
    let mut slot1 = DynamicQuasarMessage::new("slot");
    let mut slot1_id = DynamicQuasarMessage::new("id");
    slot1_id.set(1, DynamicValue::Uint32(11));
    slot1.set(1, DynamicValue::Message(Box::new(slot1_id)));

    let mut slot2 = DynamicQuasarMessage::new("slot");
    slot2.set(1, DynamicValue::Uint64(12));

    let mut slot3 = DynamicQuasarMessage::new("slot");
    slot3.set(1, DynamicValue::Uint32(13));

    let mut ship_m = DynamicQuasarMessage::new("ship");
    ship_m.set(1, DynamicValue::Uint64(621)); // Caracal

    let mut layout_m = DynamicQuasarMessage::new("layout");
    layout_m.set(
        1,
        DynamicValue::Repeated(vec![
            DynamicValue::Message(Box::new(slot1)),
            DynamicValue::Message(Box::new(slot2)),
            DynamicValue::Message(Box::new(slot3)),
        ]),
    );
    layout_m.set(2, DynamicValue::Message(Box::new(ship_m)));
    layout_m.set(3, DynamicValue::Enum(4));

    let mut ed1 = DynamicQuasarMessage::new("ed");
    let mut ed1_id = DynamicQuasarMessage::new("id");
    ed1_id.set(1, DynamicValue::Uint64(14));
    ed1.set(1, DynamicValue::Message(Box::new(ed1_id)));
    ed1.set(2, DynamicValue::Bytes(b"ed_payload".to_vec()));

    let mut ed2 = DynamicQuasarMessage::new("ed");
    ed2.set(1, DynamicValue::Uint32(15));

    let mut upd_d = DynamicQuasarMessage::new("UpdateRequest");
    upd_d.set(3, DynamicValue::Message(Box::new(layout_m)));
    upd_d.set(
        5,
        DynamicValue::Repeated(vec![
            DynamicValue::Message(Box::new(ed1)),
            DynamicValue::Message(Box::new(ed2)),
        ]),
    );

    let (des_slot, pat, mats, ed_cfg, ship_tid) = extract_design_parameters(&upd_d, &[]);
    assert_eq!(des_slot, Some(11));
    assert_eq!(pat, Some(4));
    assert_eq!(mats, Some(vec![11, 12, 13, 14, 15]));
    assert_eq!(ed_cfg, b"ed_payload".to_vec());
    assert_eq!(ship_tid, 621);

    // Direct Uint32 for lm.get(2) and lm.get(3) via field 4 layout
    let mut layout_direct = DynamicQuasarMessage::new("layout");
    layout_direct.set(2, DynamicValue::Uint32(587));
    layout_direct.set(3, DynamicValue::Uint32(2));
    let mut save_d = DynamicQuasarMessage::new("SaveRequest");
    save_d.set(4, DynamicValue::Message(Box::new(layout_direct)));
    let (_, pat2, mats2, _, ship2) = extract_design_parameters(&save_d, &[]);
    assert_eq!(pat2, Some(2));
    assert_eq!(mats2, None);
    assert_eq!(ship2, 587);
}

#[test]
fn extract_target_char_and_title_all_formats_and_error_cases() {
    let ctx_with_char = AuthoritativeContext::for_session_at(
        Some(1),
        Some(90_000_001),
        None,
        0,
        "tranquility",
        Vec::new(),
        1_760_000_000,
    );
    let ctx_anon = AuthoritativeContext::for_session_at(
        None,
        None,
        None,
        0,
        "tranquility",
        Vec::new(),
        1_760_000_000,
    );

    // 1. Missing both -> BAD_REQUEST ("Target character ID required")
    let empty_d = DynamicQuasarMessage::new("empty");
    assert_eq!(
        extract_target_char_and_title(&ctx_anon, &empty_d, &[])
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // 2. 2-field format with valid char but title_id == 0 -> BAD_REQUEST ("Title ID required")
    let mut d_zero_title = DynamicQuasarMessage::new("req");
    d_zero_title.set(1, DynamicValue::Uint32(90_000_001));
    d_zero_title.set(2, DynamicValue::Uint64(0));
    assert_eq!(
        extract_target_char_and_title(&ctx_anon, &d_zero_title, &[])
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // 3. 2-field format with direct Uint64 char and Uint32 title
    let mut d_2f = DynamicQuasarMessage::new("req");
    d_2f.set(1, DynamicValue::Uint64(90_000_002));
    d_2f.set(2, DynamicValue::Uint32(33));
    assert_eq!(
        extract_target_char_and_title(&ctx_anon, &d_2f, &[]).unwrap(),
        (90_000_002, 33)
    );

    // 4. 2-field format with Message(Uint64) char and Message(Uint32) title
    let mut cm = DynamicQuasarMessage::new("char");
    cm.set(1, DynamicValue::Uint64(90_000_003));
    let mut tm = DynamicQuasarMessage::new("title");
    tm.set(1, DynamicValue::Uint32(44));
    let mut d_2f_msg = DynamicQuasarMessage::new("req");
    d_2f_msg.set(1, DynamicValue::Message(Box::new(cm)));
    d_2f_msg.set(2, DynamicValue::Message(Box::new(tm)));
    assert_eq!(
        extract_target_char_and_title(&ctx_anon, &d_2f_msg, &[]).unwrap(),
        (90_000_003, 44)
    );

    // 5. 1-field format (title at field 1, char from ctx): Message(Uint64), direct Bytes, direct Uint32
    let mut tm64 = DynamicQuasarMessage::new("title");
    tm64.set(1, DynamicValue::Uint64(55));
    let mut d_1f = DynamicQuasarMessage::new("req");
    d_1f.set(1, DynamicValue::Message(Box::new(tm64)));
    assert_eq!(
        extract_target_char_and_title(&ctx_with_char, &d_1f, &[]).unwrap(),
        (90_000_001, 55)
    );

    let mut d_1f_bytes = DynamicQuasarMessage::new("req");
    d_1f_bytes.set(1, DynamicValue::Bytes(id_to_uuid_bytes(66).to_vec()));
    assert_eq!(
        extract_target_char_and_title(&ctx_with_char, &d_1f_bytes, &[]).unwrap(),
        (90_000_001, 66)
    );

    // 6. Wire fallback for 1-field varint and 2-field varint
    let mut wire_1f = Vec::new();
    encode_varint_field(1, 77, &mut wire_1f);
    assert_eq!(
        extract_target_char_and_title(&ctx_with_char, &empty_d, &wire_1f).unwrap(),
        (90_000_001, 77)
    );

    let mut wire_2f = Vec::new();
    encode_varint_field(1, 90_000_009, &mut wire_2f);
    encode_varint_field(2, 88, &mut wire_2f);
    assert_eq!(
        extract_target_char_and_title(&ctx_anon, &empty_d, &wire_2f).unwrap(),
        (90_000_009, 88)
    );
}

#[test]
fn draft_legacy_migration_capacity_enforcement_and_unauthenticated_guards() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        shared_sde(),
    );
    let cosmetics = crate::services::quasar_cosmetics_services::QuasarCosmeticsState::from_db(Arc::clone(&db));
    register_draft_and_title_handlers(
        &router,
        Arc::clone(&cosmetics),
        Arc::clone(&router.descriptors),
    );
    let now = 1_760_000_000u64;
    let cid = 90_000_001u32;
    let anon = sess(Some(1), None, service_roles::ROLE_PLAYER_DEFAULT);
    let p1 = sess(Some(1), Some(cid as i64), service_roles::ROLE_PLAYER_DEFAULT);

    // 1. All draft & title player endpoints reject unauthenticated caller with 403
    for ep in [
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetAllSavedRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSaveCapacityRequest",
        "eve_public.cosmetic.character.title.api.GetOwnedRequest",
        "eve_public.cosmetic.character.title.api.EquipRequest",
        "eve_public.cosmetic.character.title.api.UnequipRequest",
    ] {
        assert_eq!(
            call(&router, &anon, ep, Vec::new(), now).status_code,
            STATUS_FORBIDDEN,
            "{ep}"
        );
    }

    // 2. Seed legacy skinr_drafts and custom_capacity = 2
    {
        let mut data = cosmetics.data.write().unwrap();
        data.skinr_drafts
            .insert(5001, (cid, "Legacy migrated draft".into()));
        data.custom_capacity.insert(cid, 2);
    }

    // GetSaveCapacity returns custom capacity 2
    let cap_rsp = call(
        &router,
        &p1,
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSaveCapacityRequest",
        Vec::new(),
        now,
    );
    assert_eq!(cap_rsp.status_code, STATUS_OK);
    let cap_wm = WireMessage::decode(&cap_rsp.payload.unwrap().value).unwrap();
    assert_eq!(cap_wm.fields[0].value.as_varint(), Some(2));

    // GetAllSaved migrates legacy draft 5001 into saved_drafts
    let all_rsp = call(
        &router,
        &p1,
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetAllSavedRequest",
        Vec::new(),
        now,
    );
    assert_eq!(all_rsp.status_code, STATUS_OK);
    assert!(cosmetics
        .data
        .read()
        .unwrap()
        .saved_drafts
        .get(&cid)
        .unwrap()
        .contains_key(&5001));

    // Save 2nd draft (reaches capacity 2) -> succeeds
    let save_ok = call(
        &router,
        &p1,
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest",
        Vec::new(),
        now,
    );
    assert_eq!(save_ok.status_code, STATUS_OK);

    // Save 3rd draft (exceeds capacity 2) -> fails with STATUS_BAD_REQUEST
    let save_over = call(
        &router,
        &p1,
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest",
        Vec::new(),
        now,
    );
    assert_eq!(save_over.status_code, STATUS_BAD_REQUEST);

    // Update non-existent draft -> STATUS_NOT_FOUND
    let mut nf_id = Vec::new();
    encode_bytes_field(1, &encode_draft_id_msg(&id_to_uuid_bytes(999_999)), &mut nf_id);
    assert_eq!(
        call(
            &router,
            &p1,
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest",
            nf_id.clone(),
            now
        )
        .status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        call(
            &router,
            &p1,
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest",
            nf_id,
            now
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    // Update draft 5001 with full layout + editor_configurations
    let mut slot_id_msg = Vec::new();
    encode_varint_field(1, 7, &mut slot_id_msg);
    let mut slot_msg = Vec::new();
    encode_bytes_field(1, &slot_id_msg, &mut slot_msg);
    let mut ship_msg = Vec::new();
    encode_varint_field(1, 587, &mut ship_msg);
    let mut layout = Vec::new();
    encode_bytes_field(1, &slot_msg, &mut layout);
    encode_bytes_field(2, &ship_msg, &mut layout);
    encode_varint_field(3, 2, &mut layout);

    let mut ed_entry = Vec::new();
    encode_bytes_field(1, &slot_id_msg, &mut ed_entry);
    encode_bytes_field(2, b"new-cfg", &mut ed_entry);

    let mut upd_req = Vec::new();
    encode_bytes_field(1, &encode_draft_id_msg(&id_to_uuid_bytes(5001)), &mut upd_req);
    encode_string_field(2, "Renamed 5001", &mut upd_req);
    encode_string_field(3, "Renamed Line", &mut upd_req);
    encode_bytes_field(4, &layout, &mut upd_req);
    encode_bytes_field(5, &ed_entry, &mut upd_req);
    assert_eq!(
        call(
            &router,
            &p1,
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest",
            upd_req,
            now
        )
        .status_code,
        STATUS_OK
    );
}

#[test]
fn cosmetic_title_explicit_zero_uuid_clear_and_unequipped_revoke() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        shared_sde(),
    );
    let cosmetics = crate::services::quasar_cosmetics_services::QuasarCosmeticsState::from_db(Arc::clone(&db));
    register_draft_and_title_handlers(
        &router,
        Arc::clone(&cosmetics),
        Arc::clone(&router.descriptors),
    );
    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let player = sess(Some(1), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);
    let gm = sess(Some(1), Some(cid), service_roles::ROLE_GM_DEFAULT);

    // 1. EquipRequest with empty payload -> 400
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.cosmetic.character.title.api.EquipRequest",
            Vec::new(),
            now
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    // 2. Grant titles 10 and 11, equip 10
    for tid in [10u64, 11u64] {
        let mut char_msg = Vec::new();
        encode_varint_field(1, cid as u64, &mut char_msg);
        let mut title_msg = Vec::new();
        encode_bytes_field(1, &id_to_uuid_bytes(tid), &mut title_msg);
        let mut grant_req = Vec::new();
        encode_bytes_field(1, &char_msg, &mut grant_req);
        encode_bytes_field(2, &title_msg, &mut grant_req);
        assert_eq!(
            call(
                &router,
                &gm,
                "eve_public.cosmetic.character.title.api.admin.GrantRequest",
                grant_req,
                now
            )
            .status_code,
            STATUS_OK
        );
    }

    let mut t10_msg = Vec::new();
    encode_bytes_field(1, &id_to_uuid_bytes(10), &mut t10_msg);
    let mut eq10 = Vec::new();
    encode_bytes_field(1, &t10_msg, &mut eq10);
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.cosmetic.character.title.api.EquipRequest",
            eq10,
            now
        )
        .status_code,
        STATUS_OK
    );

    // 3. Revoke unequipped title 11 via eve.cosmetic.character.title.api.admin.RevokeRequest -> 10 stays equipped
    let mut char_msg = Vec::new();
    encode_varint_field(1, cid as u64, &mut char_msg);
    let mut t11_msg = Vec::new();
    encode_bytes_field(1, &id_to_uuid_bytes(11), &mut t11_msg);
    let mut rev11 = Vec::new();
    encode_bytes_field(1, &char_msg, &mut rev11);
    encode_bytes_field(2, &t11_msg, &mut rev11);
    assert_eq!(
        call(
            &router,
            &gm,
            "eve.cosmetic.character.title.api.admin.RevokeRequest",
            rev11,
            now
        )
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        cosmetics.data.read().unwrap().equipped_titles.get(&(cid as u32)),
        Some(&10)
    );

    // 4. EquipRequest with 16 zero bytes explicitly clears the equipped title!
    let mut zero_msg = Vec::new();
    encode_bytes_field(1, &[0u8; 16], &mut zero_msg);
    let mut clear_eq = Vec::new();
    encode_bytes_field(1, &zero_msg, &mut clear_eq);
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.cosmetic.character.title.api.EquipRequest",
            clear_eq,
            now
        )
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        cosmetics.data.read().unwrap().equipped_titles.get(&(cid as u32)),
        None
    );

    // 5. GetEquippedForCharacterRequest with explicit character field 1 when no title is equipped -> field 2 = 1
    let mut get_eq_req = Vec::new();
    encode_bytes_field(1, &char_msg, &mut get_eq_req);
    let eq_none_rsp = call(
        &router,
        &player,
        "eve_public.cosmetic.character.title.api.GetEquippedForCharacterRequest",
        get_eq_req,
        now,
    );
    assert_eq!(eq_none_rsp.status_code, STATUS_OK);
    let eq_wm = WireMessage::decode(&eq_none_rsp.payload.unwrap().value).unwrap();
    assert_eq!(eq_wm.fields[0].field_number, 2);
    assert_eq!(eq_wm.fields[0].value.as_varint(), Some(1));
}
