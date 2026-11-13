//! Unit tests for `quasar_cosmetics_drafts`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_OK};
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

fn sess(uid: Option<u64>, cid: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(uid.unwrap_or(1) * 100 + cid.unwrap_or(0) as u64);
    s.user_id = uid;
    s.character_id = cid;
    s.corporation_id = Some(98_000_001);
    s.role = role;
    s
}

fn call(router: &PublicGatewayRouter, s: &SessionState, ty: &str, value: Vec<u8>, now: u64) -> EvePublicResponse {
    router.handle_public_request(
        s,
        &EvePublicRequest {
            correlation_uuid: vec![9; 16],
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
fn uuid_and_wire_extraction_helpers() {
    let u16 = id_to_uuid_bytes(0x1234_5678_9abc_def0);
    assert_eq!(extract_id_from_uuid_bytes(&u16), 0x1234_5678_9abc_def0);
    assert_eq!(extract_id_from_uuid_bytes(&[0, 0, 1, 2]), 0x0000_0102);
    assert_eq!(extract_id_from_uuid_bytes(&[1, 2, 3]), 0);

    let mut dyn_msg = DynamicQuasarMessage::new("test.Msg");
    dyn_msg.set(1, DynamicValue::Uint64(42));
    let (uuid_opt, id_opt) = extract_draft_id_or_uuid(&dyn_msg, &[]);
    assert_eq!(id_opt, Some(42));
    assert_eq!(uuid_opt, Some(id_to_uuid_bytes(42).to_vec()));

    let raw_id = pb::message("eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest")
        .set("draft.uuid", id_to_uuid_bytes(77).to_vec())
        .must_encode();
    let empty_dyn = DynamicQuasarMessage::new("test.Empty");
    let (w_uuid, w_id) = extract_draft_id_or_uuid(&empty_dyn, &raw_id);
    assert_eq!(w_id, Some(77));
    assert_eq!(w_uuid, Some(id_to_uuid_bytes(77).to_vec()));

    // Design parameter extraction via wire fallback
    let expected_ed_cfg = pb::message("eve_public.cosmetic.ship.skin.thirdparty.draft.EditorConfiguration")
        .set("yaw", 1.5f32)
        .must_encode();
    let save_wire = pb::message("eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest")
        .set(
            "layout",
            pb::message("eve_public.cosmetic.ship.skin.thirdparty.Layout")
                .add_with("slots", |b| b.set("id.sequential", 5u64))
                .set("ship_type.sequential", 587u64)
                .set("pattern_blend_mode", pb::enum_name("BLEND_MODE_EXCLUSION")),
        )
        .add_with("editor_configurations", |b| {
            b.set("id.sequential", 9u64)
                .set("config", pb::raw_message(&expected_ed_cfg))
        })
        .must_encode();

    let (des_slot, pat, mats, ed_cfg, ship_tid) = extract_design_parameters(&empty_dyn, &save_wire);
    assert_eq!(des_slot, Some(5));
    assert_eq!(pat, Some(3));
    assert_eq!(mats, Some(vec![5, 9]));
    assert_eq!(ed_cfg, expected_ed_cfg);
    assert_eq!(ship_tid, 587);

    // Notice & attribute encoders
    let rec = SkinrDraftRecord {
        draft_id: 101,
        uuid: id_to_uuid_bytes(101).to_vec(),
        name: "MySKIN".into(),
        line_name: "LineA".into(),
        design_slot_id: 5,
        pattern_id: 3,
        material_ids: vec![5, 9],
        updated_at_unix: 1_760_000_000,
        editor_cfg: expected_ed_cfg,
        ship_type_id: 587,
    };
    assert!(!encode_draft_attributes(&rec).is_empty());
    assert!(!encode_set_notice(90_000_001, Some(10)).is_empty());
    assert!(!encode_set_notice(90_000_001, None).is_empty());
    assert_eq!(encode_granted_notice(10), encode_revoked_notice(10));

    let mut st_data = QuasarCosmeticsStateData::default();
    st_data.equipped_titles.insert(90_000_001, 88);
    let titles = get_or_init_titles(90_000_001, &mut st_data);
    assert!(titles.contains(&88));
}

#[test]
fn skinr_draft_crud_capacity_and_cross_character_isolation() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        shared_sde(),
    );
    let now = 1_760_000_000u64;
    let p1 = sess(Some(1), Some(90_000_001), service_roles::ROLE_PLAYER_DEFAULT);
    let p2 = sess(Some(2), Some(90_000_002), service_roles::ROLE_PLAYER_DEFAULT);

    // Save capacity
    let cap_rsp = call(
        &router,
        &p1,
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSaveCapacityRequest",
        Vec::new(),
        now,
    );
    assert_eq!(cap_rsp.status_code, STATUS_OK);

    // Save draft for p1
    let save_req = pb::message("eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest")
        .set("name", "Valkyrie Draft")
        .set("line_name", "Valkyrie Line")
        .must_encode();
    let save_rsp = call(
        &router,
        &p1,
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest",
        save_req,
        now,
    );
    assert_eq!(save_rsp.status_code, STATUS_OK);
    let saved_wm = WireMessage::decode(&save_rsp.payload.unwrap().value).expect("decode SaveResponse");
    let draft_id_bytes = saved_wm
        .fields
        .iter()
        .find(|f| f.field_number == 1)
        .and_then(|f| f.value.as_bytes())
        .expect("id field")
        .to_vec();

    // GetAllSaved
    let all_rsp = call(
        &router,
        &p1,
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetAllSavedRequest",
        Vec::new(),
        now,
    );
    assert_eq!(all_rsp.status_code, STATUS_OK);
    assert!(!all_rsp.payload.unwrap().value.is_empty());

    // GetRequest validation: missing ID -> 400, p2 cross-char -> 403, p1 valid -> 200
    assert_eq!(
        call(&router, &p1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest", Vec::new(), now).status_code,
        STATUS_BAD_REQUEST
    );
    let id_req = pb::message("eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest")
        .set("draft", pb::raw_message(&draft_id_bytes))
        .must_encode();
    assert_eq!(
        call(&router, &p2, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest", id_req.clone(), now).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(&router, &p1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest", id_req.clone(), now).status_code,
        STATUS_OK
    );

    // UpdateRequest: missing ID -> 400, p2 cross-char -> 403, p1 valid -> 200
    assert_eq!(
        call(&router, &p1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest", Vec::new(), now).status_code,
        STATUS_BAD_REQUEST
    );
    let upd_req = pb::message("eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest")
        .set("id", pb::raw_message(&draft_id_bytes))
        .set("name", "Updated Valkyrie")
        .set("line_name", "Updated Line")
        .must_encode();
    assert_eq!(
        call(&router, &p2, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest", upd_req.clone(), now).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(&router, &p1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest", upd_req, now).status_code,
        STATUS_OK
    );

    // DeleteRequest: missing ID -> 400, p2 cross-char -> 403, p1 valid -> 200, second delete -> 404
    assert_eq!(
        call(&router, &p1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest", Vec::new(), now).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        call(&router, &p2, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest", id_req.clone(), now).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(&router, &p1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest", id_req.clone(), now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &p1, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest", id_req, now).status_code,
        STATUS_NOT_FOUND
    );
}

#[test]
fn character_cosmetic_titles_grant_equip_unequip_and_revoke() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        shared_sde(),
    );
    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let player = sess(Some(1), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);
    let gm = sess(Some(1), Some(cid), service_roles::ROLE_GM_DEFAULT);

    // Equip unowned title -> 400
    let equip_req = pb::message("eve_public.cosmetic.character.title.api.EquipRequest")
        .set("title.uuid", id_to_uuid_bytes(7).to_vec())
        .must_encode();
    assert_eq!(
        call(&router, &player, "eve_public.cosmetic.character.title.api.EquipRequest", equip_req.clone(), now).status_code,
        STATUS_BAD_REQUEST
    );

    // Admin grant title 7
    let public_grant_req =
        pb::message("eve_public.cosmetic.character.title.api.admin.GrantRequest")
            .set("id.uuid", id_to_uuid_bytes(7).to_vec())
            .must_encode();
    let eve_grant_req = pb::message("eve.cosmetic.character.title.api.admin.GrantRequest")
        .set("character.sequential", cid as u64)
        .set("id.uuid", id_to_uuid_bytes(7).to_vec())
        .must_encode();
    assert_eq!(
        call(&router, &player, "eve_public.cosmetic.character.title.api.admin.GrantRequest", public_grant_req.clone(), now).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(&router, &gm, "eve_public.cosmetic.character.title.api.admin.GrantRequest", public_grant_req, now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &gm, "eve.cosmetic.character.title.api.admin.GrantRequest", eve_grant_req, now).status_code,
        STATUS_OK
    );

    // GetOwned & Equip
    let owned_rsp = call(&router, &player, "eve_public.cosmetic.character.title.api.GetOwnedRequest", Vec::new(), now);
    assert_eq!(owned_rsp.status_code, STATUS_OK);
    assert!(!owned_rsp.payload.unwrap().value.is_empty());

    assert_eq!(
        call(&router, &player, "eve_public.cosmetic.character.title.api.EquipRequest", equip_req, now).status_code,
        STATUS_OK
    );
    assert_eq!(
        db.with_snapshot(|s| s.characters[&cid].title.clone()),
        "Title 7"
    );

    // GetEquippedForCharacter
    let eq_rsp = call(
        &router,
        &player,
        "eve_public.cosmetic.character.title.api.GetEquippedForCharacterRequest",
        Vec::new(),
        now,
    );
    assert_eq!(eq_rsp.status_code, STATUS_OK);

    // Admin Revoke while equipped clears equipped title
    let public_revoke_req =
        pb::message("eve_public.cosmetic.character.title.api.admin.RevokeRequest")
            .set("id.uuid", id_to_uuid_bytes(7).to_vec())
            .must_encode();
    assert_eq!(
        call(&router, &gm, "eve_public.cosmetic.character.title.api.admin.RevokeRequest", public_revoke_req, now).status_code,
        STATUS_OK
    );

    // Unequip explicit call
    assert_eq!(
        call(&router, &player, "eve_public.cosmetic.character.title.api.UnequipRequest", Vec::new(), now).status_code,
        STATUS_OK
    );
    assert_eq!(db.with_snapshot(|s| s.characters[&cid].title.clone()), "");
}

#[cfg(test)]
#[path = "quasar_cosmetics_drafts_extra_unit_tests.rs"]
mod quasar_cosmetics_drafts_extra_unit_tests;
