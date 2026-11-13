//! Edge-case unit tests for `quasar_freelance_defs`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_OK};
use eve_db::{CorpProjectRecord, InventoryFlag, InventoryItemRecord, PersistedProjectDetails, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::pb;
use eve_proto::quasar::{
    DynamicQuasarMessage, EvePublicRequest, EvePublicResponse, GoogleAny, QuasarDescriptorRegistry,
    WireMessage, TYPE_GOOGLEAPIS_PREFIX,
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
fn codec_and_pagination_edge_variants() {
    // extract_uuid: raw Bytes & non-matching types.
    assert_eq!(extract_uuid(Some(&DynamicValue::Bytes(vec![1, 2, 3]))), Some(vec![1, 2, 3]));
    assert_eq!(extract_uuid(Some(&DynamicValue::Uint32(7))), None);

    // extract_currency_isk: Message with Uint64 (ok & overflow), Sint64, Uint32, Sint32, unsupported,
    // and top-level Sint64, Uint32, Sint32, overflow Uint64, unsupported.
    let mut m_u64 = DynamicQuasarMessage::new("C");
    m_u64.set(1, DynamicValue::Uint64(1234));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Message(Box::new(m_u64)))), Some(1234));
    let mut m_over = DynamicQuasarMessage::new("C");
    m_over.set(1, DynamicValue::Uint64(u64::MAX));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Message(Box::new(m_over)))), None);
    let mut m_s64 = DynamicQuasarMessage::new("C");
    m_s64.set(1, DynamicValue::Sint64(-77));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Message(Box::new(m_s64)))), Some(-77));
    let mut m_u32 = DynamicQuasarMessage::new("C");
    m_u32.set(1, DynamicValue::Uint32(42));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Message(Box::new(m_u32)))), Some(42));
    let mut m_s32 = DynamicQuasarMessage::new("C");
    m_s32.set(1, DynamicValue::Sint32(-9));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Message(Box::new(m_s32)))), Some(-9));
    let mut m_bad = DynamicQuasarMessage::new("C");
    m_bad.set(1, DynamicValue::Bool(true));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Message(Box::new(m_bad)))), None);
    let m_empty = DynamicQuasarMessage::new("C");
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Message(Box::new(m_empty)))), None);

    assert_eq!(extract_currency_isk(Some(&DynamicValue::Sint64(-500))), Some(-500));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Uint32(300))), Some(300));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Sint32(-300))), Some(-300));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Uint64(u64::MAX))), None);
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Bool(false))), None);

    // paginate_slice: Int32, Uint64, Int64, <= 0 fallback (50), Bytes offset, clamped end.
    let items = vec![1, 2, 3, 4];
    let mut p_i32 = DynamicQuasarMessage::new("P");
    p_i32.set(1, DynamicValue::Int32(2));
    p_i32.set(2, DynamicValue::Bytes(b" 1 ".to_vec()));
    let (s1, t1) = paginate_slice(Some(&DynamicValue::Message(Box::new(p_i32))), &items);
    assert_eq!(s1, vec![2, 3]);
    assert_eq!(t1.as_deref(), Some("3"));

    let mut p_u64 = DynamicQuasarMessage::new("P");
    p_u64.set(1, DynamicValue::Uint64(2));
    p_u64.set(2, DynamicValue::String("2".into()));
    let (s2, t2) = paginate_slice(Some(&DynamicValue::Message(Box::new(p_u64))), &items);
    assert_eq!(s2, vec![3, 4]);
    assert_eq!(t2, None);

    let mut p_i64 = DynamicQuasarMessage::new("P");
    p_i64.set(1, DynamicValue::Int64(1));
    p_i64.set(2, DynamicValue::String("999".into()));
    let (s3, t3) = paginate_slice(Some(&DynamicValue::Message(Box::new(p_i64))), &items);
    assert!(s3.is_empty());
    assert_eq!(t3, None);

    let mut p_zero = DynamicQuasarMessage::new("P");
    p_zero.set(1, DynamicValue::Int32(0));
    let (s4, t4) = paginate_slice(Some(&DynamicValue::Message(Box::new(p_zero))), &items);
    assert_eq!(s4, items);
    assert_eq!(t4, None);

    let mut out = Vec::new();
    encode_next_page(2, None, &mut out);
    assert!(out.is_empty());
}

#[test]
fn deliver_request_error_rejection_and_completion_paths() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let now = 1_760_000_000_u64;
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let player = sess(Some(1), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);
    let unauth_user = sess(None, None, service_roles::ROLE_PLAYER_DEFAULT);
    let no_char = sess(Some(1), None, service_roles::ROLE_PLAYER_DEFAULT);
    let deliver_ty = "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest";
    let status_ty = "eve_public.freelance.contributionmethod.itemdelivery.api.GetStatusRequest";

    // Unauthenticated -> STATUS_UNAUTHORIZED; account without char -> STATUS_FORBIDDEN
    assert_eq!(call(&router, &unauth_user, deliver_ty, Vec::new(), now).status_code, STATUS_UNAUTHORIZED);
    assert_eq!(call(&router, &no_char, deliver_ty, Vec::new(), now).status_code, STATUS_FORBIDDEN);
    // Missing UUID -> STATUS_BAD_REQUEST
    assert_eq!(call(&router, &player, deliver_ty, Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    assert_eq!(call(&router, &player, status_ty, Vec::new(), now).status_code, STATUS_BAD_REQUEST);

    // Unknown UUID -> STATUS_NOT_FOUND
    let unknown_uuid = vec![0xee_u8; 16];
    let unk_deliv_req = pb::message(deliver_ty)
        .set("project.uuid", unknown_uuid.clone())
        .must_encode();
    let unk_status_req = pb::message(status_ty)
        .set("delivery.uuid", unknown_uuid)
        .must_encode();
    assert_eq!(call(&router, &player, deliver_ty, unk_deliv_req, now).status_code, STATUS_NOT_FOUND);
    assert_eq!(call(&router, &player, status_ty, unk_status_req, now).status_code, STATUS_NOT_FOUND);

    // Non-DeliverItem project returns DeliverResponse field 12 (project_does_not_allow_item_delivery)
    let pid_non_item = 610_i64;
    let uuid_non_item = corp_projects::freelance_uuid(now, pid_non_item).to_vec();
    let pid_item = 611_i64;
    let uuid_item = corp_projects::freelance_uuid(now, pid_item).to_vec();
    let station_id = db.with_snapshot(|s| s.characters[&cid].station_id.unwrap_or(60_003_760));
    let other_station = sde
        .solar_systems
        .values()
        .flat_map(|s| &s.stations)
        .find(|st| st.station_id != station_id)
        .map(|st| st.station_id)
        .unwrap_or(60_003_760);

    db.with_mut_snapshot(|s| {
        for (pid, meta_ty, uuid) in [
            (pid_non_item, "MineOre", &uuid_non_item),
            (pid_item, "DeliverItem", &uuid_item),
        ] {
            s.corp_projects.insert(
                pid,
                CorpProjectRecord {
                    project_id: pid,
                    corporation_id: corp_id,
                    name: format!("Proj {pid}"),
                    target_quantity: 20,
                    current_quantity: 0,
                    isk_per_unit: eve_db::IskCents(100_00),
                    escrow_remaining: eve_db::IskCents(10_000_00),
                },
            );
            s.alliance_war_state
                .project_meta
                .insert(pid, (meta_ty.into(), true, false));
            let mut det = PersistedProjectDetails::default();
            det.creator_id = cid;
            det.created_unix = now;
            det.status = "Active".into();
            det.filter_id = Some(34);
            det.batch_size = 5;
            det.max_per_character = Some(15);
            det.uuid_bytes = uuid.clone();
            det.committed_participants.push(cid);
            s.alliance_war_state.project_details.insert(pid, det);
        }
    });

    // GetStatusRequest: active with 0 progress -> field 1
    let st_active_req = pb::message(status_ty)
        .set("delivery.uuid", uuid_item.clone())
        .must_encode();
    let st_active = call(&router, &player, status_ty, st_active_req, now);
    assert_eq!(st_active.status_code, STATUS_OK);
    assert_eq!(WireMessage::decode(&st_active.payload.unwrap().value).unwrap().fields[0].field_number, 1);

    // Non-DeliverItem project -> field 12
    let non_item_req = pb::message(deliver_ty)
        .set("project.uuid", uuid_non_item.clone())
        .must_encode();
    let non_item_rsp = call(&router, &player, deliver_ty, non_item_req, now);
    assert_eq!(non_item_rsp.status_code, STATUS_OK);
    assert_eq!(WireMessage::decode(&non_item_rsp.payload.unwrap().value).unwrap().fields[0].field_number, 12);

    // Zero / missing quantity -> STATUS_BAD_REQUEST
    let req_zero_qty = pb::message(deliver_ty)
        .set("project.uuid", uuid_item.clone())
        .set("quantity", 0u32)
        .must_encode();
    assert_eq!(call(&router, &player, deliver_ty, req_zero_qty, now).status_code, STATUS_BAD_REQUEST);

    // Helper to build raw DeliverRequest with optional delivery_loc, item_id, item_loc (container, flag), qty
    let build_deliver = |del_loc: Option<(u32, i64)>, item_id: Option<i64>, item_loc: Option<(i64, i64)>, qty: u64| {
        pb::message(deliver_ty)
            .set("project.uuid", uuid_item.clone())
            .with(|b| match del_loc {
                Some((1, loc_id)) => b.set("delivery_location.station.sequential", loc_id as u64),
                Some((2, loc_id)) => b.set("delivery_location.structure.sequential", loc_id as u64),
                Some((other, _)) => panic!("unsupported delivery_location field {other}"),
                None => b,
            })
            .with(|b| match item_id {
                Some(iid) => b.set("item.sequential", iid as u64),
                None => b,
            })
            .set("quantity", qty as u32)
            .with(|b| match item_loc {
                Some((cont, flg)) => b
                    .set("item_location.item.sequential", cont as u64)
                    .set("item_location.flag.value", flg as u64),
                None => b,
            })
            .must_encode()
    };

    // Undocked -> rejection 5 (character_not_docked)
    db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&cid).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
    });
    let r_undocked = call(&router, &player, deliver_ty, build_deliver(None, None, None, 5), now);
    assert_eq!(WireMessage::decode(&r_undocked.payload.unwrap().value).unwrap().fields[0].field_number, 5);

    // Re-dock at station_id; test non-existent delivery location (3) & wrong dock location (5)
    db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().station_id = Some(station_id);
    });
    let r_bad_loc = call(&router, &player, deliver_ty, build_deliver(Some((1, 69_999_999)), None, None, 5), now);
    assert_eq!(WireMessage::decode(&r_bad_loc.payload.unwrap().value).unwrap().fields[0].field_number, 3);
    let r_wrong_dock = call(&router, &player, deliver_ty, build_deliver(Some((1, other_station)), None, None, 5), now);
    assert_eq!(WireMessage::decode(&r_wrong_dock.payload.unwrap().value).unwrap().fields[0].field_number, 5);

    // Seed explicit items for rejections 8 (missing), 6 (wrong owner), 9 (wrong type / singleton), 7 (wrong item_location)
    db.with_mut_snapshot(|s| {
        s.items.insert(
            700_001,
            InventoryItemRecord {
                item_id: 700_001,
                type_id: 34,
                owner_id: other_cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        s.items.insert(
            700_002,
            InventoryItemRecord {
                item_id: 700_002,
                type_id: 35, // wrong type
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        s.items.insert(
            700_003,
            InventoryItemRecord {
                item_id: 700_003,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 15,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let r_missing_item = call(&router, &player, deliver_ty, build_deliver(None, Some(799_999), None, 5), now);
    assert_eq!(WireMessage::decode(&r_missing_item.payload.unwrap().value).unwrap().fields[0].field_number, 8);
    let r_wrong_owner = call(&router, &player, deliver_ty, build_deliver(None, Some(700_001), None, 5), now);
    assert_eq!(WireMessage::decode(&r_wrong_owner.payload.unwrap().value).unwrap().fields[0].field_number, 6);
    let r_wrong_type = call(&router, &player, deliver_ty, build_deliver(None, Some(700_002), None, 5), now);
    assert_eq!(WireMessage::decode(&r_wrong_type.payload.unwrap().value).unwrap().fields[0].field_number, 9);
    let r_wrong_item_loc = call(
        &router,
        &player,
        deliver_ty,
        build_deliver(None, Some(700_003), Some((other_station, 4)), 5),
        now,
    );
    assert_eq!(WireMessage::decode(&r_wrong_item_loc.payload.unwrap().value).unwrap().fields[0].field_number, 7);

    // Explicit item full stack consumption (15 units) -> max_per_character (15) now exhausted
    let r_ok = call(
        &router,
        &player,
        deliver_ty,
        build_deliver(Some((1, station_id)), Some(700_003), Some((station_id, InventoryFlag::Hangar as i64)), 15),
        now,
    );
    assert_eq!(r_ok.status_code, STATUS_OK);
    assert!(db.with_snapshot(|s| !s.items.contains_key(&700_003)));

    // Exceeding max_per_character -> rejection 10 (quantity_too_high)
    let r_cap = call(&router, &player, deliver_ty, build_deliver(None, None, None, 5), now);
    assert_eq!(WireMessage::decode(&r_cap.payload.unwrap().value).unwrap().fields[0].field_number, 10);

    // Raise cap & test missing auto-select item -> ContributionFailedNotice + STATUS_BAD_REQUEST
    db.with_mut_snapshot(|s| {
        s.alliance_war_state
            .project_details
            .get_mut(&pid_item)
            .unwrap()
            .max_per_character = None;
        s.items.retain(|_, it| !(it.owner_id == cid && it.type_id == 34));
    });
    assert_eq!(
        call(&router, &player, deliver_ty, build_deliver(None, None, None, 5), now).status_code,
        STATUS_BAD_REQUEST
    );

    // Re-seed remaining 5 units and complete the project -> status becomes "Completed"
    db.with_mut_snapshot(|s| {
        s.items.insert(
            700_004,
            InventoryItemRecord {
                item_id: 700_004,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let r_complete = call(&router, &player, deliver_ty, build_deliver(None, None, None, 5), now);
    assert_eq!(r_complete.status_code, STATUS_OK);
    db.with_snapshot(|s| {
        assert_eq!(s.alliance_war_state.project_details[&pid_item].status, "Completed");
        assert!(s.alliance_war_state.project_meta[&pid_item].2);
    });

    // Subsequent delivery against completed project hits state != 1 -> STATUS_BAD_REQUEST
    assert_eq!(
        call(&router, &player, deliver_ty, build_deliver(None, None, None, 5), now).status_code,
        STATUS_BAD_REQUEST
    );

    // Cancelled/inactive project with 0 progress returns GetStatusResponse field 3
    db.with_mut_snapshot(|s| {
        s.alliance_war_state
            .project_details
            .get_mut(&pid_non_item)
            .unwrap()
            .status = "Cancelled".into();
    });
    let st_cancel_req = pb::message(status_ty)
        .set("delivery.uuid", uuid_non_item.clone())
        .must_encode();
    let st_cancel = call(&router, &player, status_ty, st_cancel_req, now);
    assert_eq!(st_cancel.status_code, STATUS_OK);
    assert_eq!(WireMessage::decode(&st_cancel.payload.unwrap().value).unwrap().fields[0].field_number, 1);

    let st_done_req = pb::message(status_ty)
        .set("delivery.uuid", uuid_item.clone())
        .must_encode();
    let st_done = call(&router, &player, status_ty, st_done_req, now);
    assert_eq!(st_done.status_code, STATUS_OK);

    // GetAllLatestWithinMajorRequest with major = 0 and missing major
    let def_ty = "eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorRequest";
    let maj0 = pb::message(def_ty).set("major", 0u32).must_encode();
    assert_eq!(call(&router, &player, def_ty, maj0, now).status_code, STATUS_OK);
    assert_eq!(call(&router, &player, def_ty, Vec::new(), now).status_code, STATUS_OK);
}

#[test]
fn mutants_cov_locked_explicit_autoselect_distractors_and_batch_math() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let now = 1_760_100_000_u64;
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let player = sess(Some(1), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);
    let deliver_ty = "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest";
    let station_id = db.with_snapshot(|s| s.characters[&cid].station_id.unwrap_or(60_003_760));
    let other_station = sde
        .solar_systems
        .values()
        .flat_map(|s| &s.stations)
        .find(|st| st.station_id != station_id)
        .map(|st| st.station_id)
        .unwrap_or(60_003_761);

    let pid = 612_i64;
    let uuid = corp_projects::freelance_uuid(now, pid).to_vec();

    // Distractors with LOWER item_ids than valid item 710_010:
    // 710_001: wrong owner (tests L276 == -> != and L277 && -> ||)
    // 710_002: ALSC-locked (tests L266 || -> &&, L277 ! deleted)
    // 710_003: singleton=true (tests L279 && -> ||, ! deleted)
    // 710_004 + 710_099: non-empty + wrong loc/flag (tests L280 && -> ||, ! deleted)
    // 710_010: valid item qty=17, batch_size=5 -> delivers 15 leaving 2 (kills L292 / -> %, *, * -> +, /)
    db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().station_id = Some(station_id);
        s.corp_projects.insert(
            pid,
            CorpProjectRecord {
                project_id: pid,
                corporation_id: corp_id,
                name: "Proj 612".into(),
                target_quantity: 100,
                current_quantity: 0,
                isk_per_unit: eve_db::IskCents(100_00),
                escrow_remaining: eve_db::IskCents(100_000_00),
            },
        );
        s.alliance_war_state
            .project_meta
            .insert(pid, ("DeliverItem".into(), true, false));
        let mut det = PersistedProjectDetails::default();
        det.creator_id = cid;
        det.created_unix = now;
        det.status = "Active".into();
        det.filter_id = Some(34);
        det.batch_size = 5;
        det.max_per_character = None;
        det.uuid_bytes = uuid.clone();
        det.committed_participants.push(cid);
        s.alliance_war_state.project_details.insert(pid, det);

        for (iid, owner, loc, flag, qty, singleton) in [
            (710_001_i64, other_cid, station_id, InventoryFlag::Hangar, 10_i32, false),
            (710_002, cid, station_id, InventoryFlag::Hangar, 10, false),
            (710_003, cid, station_id, InventoryFlag::Hangar, 10, true),
            (710_004, cid, other_station, InventoryFlag::Cargo, 10, false),
            (710_099, cid, 710_004, InventoryFlag::Cargo, 1, false),
            (710_010, cid, station_id, InventoryFlag::Hangar, 17, false),
            (710_011, cid, station_id, InventoryFlag::Hangar, 3, false),
        ] {
            s.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 34,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: qty,
                    singleton,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        s.owner_notes.insert(
            -8_400_000_000_000,
            eve_db::OwnerNoteRecord {
                note_id: -8_400_000_000_000,
                character_id: cid,
                item_id: 0,
                label: "alsc".into(),
                note: serde_json::json!({ "alsc_locked_items": [710_002] }).to_string(),
                timestamp_unix: now,
            },
        );
    });

    let build_req = |item_id: Option<i64>, qty: u64| {
        pb::message(deliver_ty)
            .set("project.uuid", uuid.clone())
            .with(|b| match item_id {
                Some(iid) => b.set("item.sequential", iid as u64),
                None => b,
            })
            .set("quantity", qty as u32)
            .must_encode()
    };

    // 1. Explicit ALSC-locked empty non-singleton stack -> rejection field 7 (kills L266 || -> &&)
    let rsp_locked = call(&router, &player, deliver_ty, build_req(Some(710_002), 5), now);
    assert_eq!(rsp_locked.status_code, STATUS_OK);
    assert_eq!(
        WireMessage::decode(&rsp_locked.payload.unwrap().value).unwrap().fields[0].field_number,
        7
    );

    // 2. Explicit qty < batch_size (3 < 5) -> take == 0 -> STATUS_BAD_REQUEST
    assert_eq!(
        call(&router, &player, deliver_ty, build_req(Some(710_011), 5), now).status_code,
        STATUS_BAD_REQUEST
    );

    // 3. Auto-select skips distractors 710_001..710_004 & 710_011, selects 710_010 (qty=17),
    // delivers (17 / 5) * 5 = 15, leaving qty=2!
    let rsp_auto = call(&router, &player, deliver_ty, build_req(None, 20), now);
    assert_eq!(rsp_auto.status_code, STATUS_OK);
    db.with_snapshot(|s| {
        assert_eq!(s.items[&710_010].quantity, 2);
        assert_eq!(s.items[&710_001].quantity, 10);
        assert_eq!(s.items[&710_002].quantity, 10);
        assert_eq!(s.items[&710_003].quantity, 10);
        assert_eq!(s.items[&710_004].quantity, 10);
        assert_eq!(s.corp_projects[&pid].current_quantity, 15);
    });
}
