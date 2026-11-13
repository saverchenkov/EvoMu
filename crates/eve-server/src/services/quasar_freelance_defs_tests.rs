//! Unit tests for `quasar_freelance_defs`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_OK};
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
            correlation_uuid: vec![15; 16],
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
fn uuid_currency_and_pagination_helpers() {
    let mut b1 = Vec::new();
    encode_uuid_field(1, &[0xaa; 16], &mut b1);
    assert!(!b1.is_empty());
    let mut b2 = Vec::new();
    encode_char_id_field(2, 90_000_001, &mut b2);
    assert!(!b2.is_empty());

    let mut id_msg = DynamicQuasarMessage::new("test.Id");
    id_msg.set(1, DynamicValue::Bytes(vec![0xbb; 16]));
    let wrapped = DynamicValue::Message(Box::new(id_msg));
    assert_eq!(extract_uuid(Some(&wrapped)), Some(vec![0xbb; 16]));
    assert_eq!(extract_uuid(None), None);

    assert_eq!(extract_currency_isk(Some(&DynamicValue::Uint64(5_000))), Some(5_000));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Int32(-10))), Some(-10));
    let mut cm = DynamicQuasarMessage::new("test.Curr");
    cm.set(1, DynamicValue::Int64(99_000));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Message(Box::new(cm)))), Some(99_000));
    assert_eq!(extract_currency_isk(None), None);

    let items = vec![10, 20, 30, 40, 50];
    let (all, tok0) = paginate_slice(None, &items);
    assert_eq!(all, items);
    assert_eq!(tok0, None);

    let mut pm = DynamicQuasarMessage::new("test.Page");
    pm.set(1, DynamicValue::Uint32(2));
    pm.set(2, DynamicValue::String("1".to_string()));
    let (page, tok1) = paginate_slice(Some(&DynamicValue::Message(Box::new(pm))), &items);
    assert_eq!(page, vec![20, 30]);
    assert_eq!(tok1, Some("3".to_string()));

    let mut np_out = Vec::new();
    encode_next_page(2, tok1.as_deref(), &mut np_out);
    assert!(!np_out.is_empty());
}

#[test]
fn contribution_method_definitions_and_freelance_item_delivery() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let corp_id = 98_000_001i64;
    let player = sess(Some(1), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);

    // 1. Definition endpoints
    let latest_rsp = call(
        &router,
        &player,
        "eve_public.freelance.contributionmethod.definition.api.GetAllLatestRequest",
        Vec::new(),
        now,
    );
    assert_eq!(latest_rsp.status_code, STATUS_OK);
    assert!(!latest_rsp.payload.unwrap().value.is_empty());

    let maj1 = pb::message(
        "eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorRequest",
    )
    .set("major", 1u32)
    .must_encode();
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorRequest",
            maj1,
            now
        )
        .status_code,
        STATUS_OK
    );
    let maj2 = pb::message(
        "eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorRequest",
    )
    .set("major", 2u32)
    .must_encode();
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorRequest",
            maj2,
            now
        )
        .status_code,
        STATUS_OK
    );

    // 2. Seed an active DeliverItem freelance project + hangar item
    let pid = 505i64;
    let uuid = corp_projects::freelance_uuid(now, pid).to_vec();
    let station_id = db.with_snapshot(|s| s.characters[&cid].station_id.unwrap_or(60_003_760));
    db.with_mut_snapshot(|s| {
        s.corp_projects.insert(
            pid,
            CorpProjectRecord {
                project_id: pid,
                corporation_id: corp_id,
                name: "Deliver Tritanium".into(),
                target_quantity: 100,
                current_quantity: 0,
                isk_per_unit: eve_db::IskCents(1_000_00),
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
        det.batch_size = 10;
        det.uuid_bytes = uuid.clone();
        det.committed_participants.push(cid);
        s.alliance_war_state.project_details.insert(pid, det);

        s.items.insert(
            888_001,
            InventoryItemRecord {
                item_id: 888_001,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // GetStatusRequest before delivery
    let status_req = pb::message(
        "eve_public.freelance.contributionmethod.itemdelivery.api.GetStatusRequest",
    )
    .set("delivery.uuid", uuid.clone())
    .must_encode();
    let st_rsp1 = call(
        &router,
        &player,
        "eve_public.freelance.contributionmethod.itemdelivery.api.GetStatusRequest",
        status_req.clone(),
        now,
    );
    assert_eq!(st_rsp1.status_code, STATUS_OK);

    // DeliverRequest (deliver 50 units of Tritanium)
    let deliv_req = pb::message(
        "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest",
    )
    .set("project.uuid", uuid.clone())
    .set("quantity", 50u32)
    .must_encode();
    let del_rsp = call(
        &router,
        &player,
        "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest",
        deliv_req,
        now,
    );
    assert_eq!(del_rsp.status_code, STATUS_OK);
    let del_wm = WireMessage::decode(&del_rsp.payload.unwrap().value).unwrap();
    assert_eq!(del_wm.fields[0].field_number, 1);
    assert_eq!(db.with_snapshot(|s| s.corp_projects[&pid].current_quantity), 50);

    // GetStatusRequest after delivery (progress > 0 -> field 2)
    let st_rsp2 = call(
        &router,
        &player,
        "eve_public.freelance.contributionmethod.itemdelivery.api.GetStatusRequest",
        status_req,
        now,
    );
    assert_eq!(st_rsp2.status_code, STATUS_OK);
    let st_wm = WireMessage::decode(&st_rsp2.payload.unwrap().value).unwrap();
    assert_eq!(st_wm.fields[0].field_number, 2);
}

#[test]
fn freelance_helpers_and_item_delivery_rejections_and_completion() {
    // 1. Helper edge cases: extract_uuid, extract_currency_isk, paginate_slice, encode_next_page
    let mut bad_uuid = DynamicQuasarMessage::new("test.Id");
    bad_uuid.set(1, DynamicValue::Uint32(42));
    assert_eq!(extract_uuid(Some(&DynamicValue::Message(Box::new(bad_uuid)))), None);
    assert_eq!(extract_uuid(Some(&DynamicValue::Uint64(99))), None);

    assert_eq!(extract_currency_isk(Some(&DynamicValue::Uint64(u64::MAX))), None);
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Sint64(-777))), Some(-777));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Uint32(123))), Some(123));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Sint32(-45))), Some(-45));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Bool(true))), None);
    let mut cm_u64_ov = DynamicQuasarMessage::new("C");
    cm_u64_ov.set(1, DynamicValue::Uint64(u64::MAX));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Message(Box::new(cm_u64_ov)))), None);
    let mut cm_s32 = DynamicQuasarMessage::new("C");
    cm_s32.set(1, DynamicValue::Sint32(-19));
    assert_eq!(extract_currency_isk(Some(&DynamicValue::Message(Box::new(cm_s32)))), Some(-19));

    let items = vec![1, 2, 3, 4];
    let mut pm_bytes = DynamicQuasarMessage::new("P");
    pm_bytes.set(1, DynamicValue::Int32(0)); // falls back to 50
    pm_bytes.set(2, DynamicValue::Bytes(b" 2 ".to_vec()));
    let (tail, tok_end) = paginate_slice(Some(&DynamicValue::Message(Box::new(pm_bytes))), &items);
    assert_eq!(tail, vec![3, 4]);
    assert_eq!(tok_end, None);

    let mut pm_oob = DynamicQuasarMessage::new("P");
    pm_oob.set(1, DynamicValue::Int64(2));
    pm_oob.set(2, DynamicValue::String("999".into()));
    let (empty_p, tok_none) = paginate_slice(Some(&DynamicValue::Message(Box::new(pm_oob))), &items);
    assert!(empty_p.is_empty());
    assert_eq!(tok_none, None);

    let mut np_empty = Vec::new();
    encode_next_page(2, None, &mut np_empty);
    assert!(np_empty.is_empty());

    // 2. encode_single_freelance_def with full localization, icon, tags, and parameters
    let mut def_out = Vec::new();
    let sample_def = serde_json::json!({
        "titleID": 1001,
        "descriptionID": 1002,
        "progressDescriptionID": 1003,
        "targetValueDescriptionID": 1004,
        "rewardingDescriptionID": 1005,
        "icon": "res:/ui/icon.png",
        "contentTags": ["tag_a", "tag_b"],
        "parameters": [
            {
                "parameterKey": "type",
                "parameterType": "itemType",
                "icon": "res:/ui/param.png",
                "isRequired": 1,
                "maxEntries": 5
            },
            {
                "parameterKey": "opt",
                "parameterType": "solarSystem",
                "isRequired": 0
            }
        ]
    });
    encode_single_freelance_def("DeliverItem", &sample_def, &mut def_out);
    let def_wm = WireMessage::decode(&def_out).unwrap();
    assert_eq!(def_wm.fields.len(), 1);
    assert_eq!(def_wm.fields[0].field_number, 2);

    // 3. DeliverRequest & GetStatusRequest error, rejection, and completion paths
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let other_cid = 90_000_002i64;
    let corp_id = 98_000_001i64;
    let player = sess(Some(1), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);
    let unauth = sess(None, None, service_roles::ROLE_PLAYER_DEFAULT);

    // Subscribe player session to capture Gateway notices (ContributionFailedNotice, ContributionSucceededNotice, etc.)
    let mut rx = router.subscribe_session(&player, Vec::new(), now);

    // Unauthenticated & missing UUID
    assert_eq!(
        call(&router, &unauth, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", Vec::new(), now).status_code,
        STATUS_UNAUTHORIZED
    );
    assert_eq!(
        call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", Vec::new(), now).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.GetStatusRequest", Vec::new(), now).status_code,
        STATUS_BAD_REQUEST
    );

    let unknown_uuid = [0x99u8; 16];
    let unk_deliv = pb::message(
        "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest",
    )
    .set("project.uuid", unknown_uuid.to_vec())
    .must_encode();
    let unk_status = pb::message(
        "eve_public.freelance.contributionmethod.itemdelivery.api.GetStatusRequest",
    )
    .set("delivery.uuid", unknown_uuid.to_vec())
    .must_encode();
    assert_eq!(
        call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", unk_deliv, now).status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.GetStatusRequest", unk_status, now).status_code,
        STATUS_NOT_FOUND
    );

    // Non-DeliverItem project -> DeliverResponse field 12
    let pid_other = 601i64;
    let uuid_other = corp_projects::freelance_uuid(now, pid_other).to_vec();
    let station_id = db.with_snapshot(|s| s.characters[&cid].station_id.unwrap_or(60_003_760));
    db.with_mut_snapshot(|s| {
        s.corp_projects.insert(
            pid_other,
            CorpProjectRecord {
                project_id: pid_other,
                corporation_id: corp_id,
                name: "Destroy NPCs".into(),
                target_quantity: 50,
                current_quantity: 0,
                isk_per_unit: eve_db::IskCents(1_000_00),
                escrow_remaining: eve_db::IskCents(50_000_00),
            },
        );
        s.alliance_war_state
            .project_meta
            .insert(pid_other, ("DestroyNPCs".into(), true, false));
        let mut det = PersistedProjectDetails::default();
        det.creator_id = cid;
        det.created_unix = now;
        det.status = "Active".into();
        det.filter_id = Some(34);
        det.batch_size = 5;
        det.uuid_bytes = uuid_other.clone();
        det.committed_participants.push(cid);
        s.alliance_war_state.project_details.insert(pid_other, det);
    });
    let non_item_req = pb::message(
        "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest",
    )
    .set("project.uuid", uuid_other.clone())
    .set("quantity", 10u32)
    .must_encode();
    let non_item_rsp = call(
        &router,
        &player,
        "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest",
        non_item_req,
        now,
    );
    assert_eq!(non_item_rsp.status_code, STATUS_OK);
    assert_eq!(WireMessage::decode(&non_item_rsp.payload.unwrap().value).unwrap().fields[0].field_number, 12);

    // Active DeliverItem project with max_per_character = 40, batch_size = 10, target = 30
    let pid = 602i64;
    let uuid = corp_projects::freelance_uuid(now, pid).to_vec();
    db.with_mut_snapshot(|s| {
        s.corp_projects.insert(
            pid,
            CorpProjectRecord {
                project_id: pid,
                corporation_id: corp_id,
                name: "Deliver Pyerite".into(),
                target_quantity: 30,
                current_quantity: 0,
                isk_per_unit: eve_db::IskCents(500_00),
                escrow_remaining: eve_db::IskCents(50_000_00),
            },
        );
        s.alliance_war_state
            .project_meta
            .insert(pid, ("DeliverItem".into(), true, false));
        let mut det = PersistedProjectDetails::default();
        det.creator_id = cid;
        det.created_unix = now;
        det.status = "Active".into();
        det.filter_id = Some(999_123);
        det.batch_size = 10;
        det.max_per_character = Some(25);
        det.uuid_bytes = uuid.clone();
        det.committed_participants.push(cid);
        s.alliance_war_state.project_details.insert(pid, det);
    });

    // Helper to build DeliverRequest with optional delivery station (2.1), item_id (3), qty (4), item_loc (5)
    let make_deliver_req = |station: Option<i64>, iid: Option<i64>, qty: Option<u64>, cont_flag: Option<(i64, u32)>| {
        pb::message(
            "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest",
        )
        .set("project.uuid", uuid.clone())
        .with(|b| match station {
            Some(st) => b.set("delivery_location.station.sequential", st as u64),
            None => b,
        })
        .with(|b| match iid {
            Some(id) => b.set("item.sequential", id as u64),
            None => b,
        })
        .with(|b| match qty {
            Some(q) => b.set("quantity", q as u32),
            None => b,
        })
        .with(|b| match cont_flag {
            Some((c, fl)) => b
                .set("item_location.item.sequential", c as u64)
                .set("item_location.flag.value", u64::from(fl)),
            None => b,
        })
        .must_encode()
    };

    // Missing quantity -> STATUS_BAD_REQUEST
    assert_eq!(
        call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", make_deliver_req(None, None, None, None), now).status_code,
        STATUS_BAD_REQUEST
    );

    // No items available -> ContributionFailedNotice + STATUS_BAD_REQUEST
    assert_eq!(
        call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", make_deliver_req(None, None, Some(10), None), now).status_code,
        STATUS_BAD_REQUEST
    );
    let mut fail_urls = Vec::new();
    while let Ok(env) = rx.try_recv() {
        if let Some(p) = env.payload {
            fail_urls.push(p.type_url);
        }
    }
    assert!(fail_urls.iter().any(|u| u.ends_with("ContributionFailedNotice")));

    // Non-existent delivery station -> variant 3
    let r_bad_st = call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", make_deliver_req(Some(99_999_999), None, Some(10), None), now);
    assert_eq!(WireMessage::decode(&r_bad_st.payload.unwrap().value).unwrap().fields[0].field_number, 3);

    // Different existing station -> variant 5 (not docked at delivery location)
    let other_st = *sde.stations.keys().find(|&&sid| sid != station_id).unwrap();
    let r_diff_st = call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", make_deliver_req(Some(other_st), None, Some(10), None), now);
    assert_eq!(WireMessage::decode(&r_diff_st.payload.unwrap().value).unwrap().fields[0].field_number, 5);

    // Undocked character -> variant 5
    db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let r_undocked = call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", make_deliver_req(None, None, Some(10), None), now);
    assert_eq!(WireMessage::decode(&r_undocked.payload.unwrap().value).unwrap().fields[0].field_number, 5);
    db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.station_id = Some(station_id);
        }
    });

    // Seed items for specific item_id rejections: wrong owner (6), locked in contract (8), wrong type (9), wrong flag (7), valid exact stack (1)
    db.with_mut_snapshot(|s| {
        s.items.insert(910_001, InventoryItemRecord { item_id: 910_001, type_id: 999_123, owner_id: other_cid, location_id: station_id, flag: InventoryFlag::Hangar, quantity: 20, ..Default::default() });
        s.items.insert(910_002, InventoryItemRecord { item_id: 910_002, type_id: 999_123, owner_id: cid, location_id: station_id, flag: InventoryFlag::Hangar, quantity: 20, ..Default::default() });
        s.contracts.insert(7001, eve_db::ContractRecord { contract_id: 7001, issuer_id: cid, acceptor_id: None, contract_type: "ItemExchange".into(), status: "Outstanding".into(), start_station_id: station_id, end_station_id: station_id, price: eve_db::IskCents(0), reward: eve_db::IskCents(0), collateral: eve_db::IskCents(0), item_ids: vec![910_002], expiration_unix: now + 1000, issuer_corp_id: corp_id, ..Default::default() });
        s.items.insert(910_003, InventoryItemRecord { item_id: 910_003, type_id: 34, owner_id: cid, location_id: station_id, flag: InventoryFlag::Hangar, quantity: 20, ..Default::default() });
        s.items.insert(910_004, InventoryItemRecord { item_id: 910_004, type_id: 999_123, owner_id: cid, location_id: station_id, flag: InventoryFlag::Hangar, quantity: 20, ..Default::default() });
        s.items.insert(910_005, InventoryItemRecord { item_id: 910_005, type_id: 999_123, owner_id: cid, location_id: station_id, flag: InventoryFlag::Hangar, quantity: 10, ..Default::default() });
    });

    // Missing item_id -> 8
    let r_miss = call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", make_deliver_req(Some(station_id), Some(999_999), Some(10), None), now);
    assert_eq!(WireMessage::decode(&r_miss.payload.unwrap().value).unwrap().fields[0].field_number, 8);
    // Locked in contract -> 8
    let r_lock = call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", make_deliver_req(Some(station_id), Some(910_002), Some(10), None), now);
    assert_eq!(WireMessage::decode(&r_lock.payload.unwrap().value).unwrap().fields[0].field_number, 8);
    // Not owned -> 6
    let r_own = call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", make_deliver_req(Some(station_id), Some(910_001), Some(10), None), now);
    assert_eq!(WireMessage::decode(&r_own.payload.unwrap().value).unwrap().fields[0].field_number, 6);
    // Wrong type -> 9
    let r_typ = call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", make_deliver_req(Some(station_id), Some(910_003), Some(10), None), now);
    assert_eq!(WireMessage::decode(&r_typ.payload.unwrap().value).unwrap().fields[0].field_number, 9);
    // Invalid item_location container -> 7
    let r_loc = call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", make_deliver_req(Some(station_id), Some(910_004), Some(10), Some((station_id + 1, InventoryFlag::Hangar as u32))), now);
    assert_eq!(WireMessage::decode(&r_loc.payload.unwrap().value).unwrap().fields[0].field_number, 7);

    // Valid delivery of 20 units (max_per_character = 25 -> allowed 20)
    let r_ok1 = call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", make_deliver_req(Some(station_id), Some(910_004), Some(20), Some((station_id, InventoryFlag::Hangar as u32))), now);
    assert_eq!(WireMessage::decode(&r_ok1.payload.unwrap().value).unwrap().fields[0].field_number, 1);
    assert!(!db.with_snapshot(|s| s.items.contains_key(&910_004))); // exact stack removed

    // Next attempt by same character exceeds max_per_character (25 - 20 = 5 < batch_size 10) -> variant 10
    let r_cap = call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", make_deliver_req(Some(station_id), Some(910_005), Some(10), None), now);
    assert_eq!(WireMessage::decode(&r_cap.payload.unwrap().value).unwrap().fields[0].field_number, 10);

    // Second participant completes the project (remaining = 10) -> CompletedNotice emitted
    db.with_mut_snapshot(|s| {
        s.alliance_war_state.project_details.get_mut(&pid).unwrap().committed_participants.push(other_cid);
        if let Some(ch) = s.characters.get_mut(&other_cid) {
            ch.station_id = Some(station_id);
        } else {
            let mut ch = s.characters[&cid].clone();
            ch.character_id = other_cid;
            ch.user_id = 2;
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            s.characters.insert(other_cid, ch);
        }
        s.items.insert(910_006, InventoryItemRecord { item_id: 910_006, type_id: 999_123, owner_id: other_cid, location_id: station_id, flag: InventoryFlag::Hangar, quantity: 10, ..Default::default() });
    });
    let player2 = sess(Some(2), Some(other_cid), service_roles::ROLE_PLAYER_DEFAULT);
    while rx.try_recv().is_ok() {}
    let mut rx2 = router.subscribe_session(&player2, Vec::new(), now);
    let r_comp = call(&router, &player2, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", make_deliver_req(Some(station_id), Some(910_006), Some(10), None), now);
    assert_eq!(WireMessage::decode(&r_comp.payload.unwrap().value).unwrap().fields[0].field_number, 1);
    let mut comp_urls = Vec::new();
    while let Ok(env) = rx.try_recv() {
        if let Some(p) = env.payload {
            comp_urls.push(p.type_url);
        }
    }
    while let Ok(env) = rx2.try_recv() {
        if let Some(p) = env.payload {
            comp_urls.push(p.type_url);
        }
    }
    assert!(comp_urls.iter().any(|u| u.ends_with("ContributionSucceededNotice")));
    assert!(comp_urls.iter().any(|u| u.ends_with("ParticipantContributedNotice")));
    assert!(comp_urls.iter().any(|u| u.ends_with("ProgressedNotice")));
    assert!(comp_urls.iter().any(|u| u.ends_with("CompletedNotice")));

    // Delivering after completion returns STATUS_BAD_REQUEST
    assert_eq!(
        call(&router, &player2, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", make_deliver_req(None, None, Some(10), None), now).status_code,
        STATUS_BAD_REQUEST
    );

    // GetStatusRequest on an inactive zero-progress project -> field 3
    let pid_closed = 603i64;
    let uuid_closed = corp_projects::freelance_uuid(now, pid_closed).to_vec();
    db.with_mut_snapshot(|s| {
        s.corp_projects.insert(pid_closed, CorpProjectRecord { project_id: pid_closed, corporation_id: corp_id, name: "Closed".into(), target_quantity: 10, current_quantity: 0, isk_per_unit: eve_db::IskCents(0), escrow_remaining: eve_db::IskCents(0) });
        s.alliance_war_state.project_meta.insert(pid_closed, ("DeliverItem".into(), true, false));
        let mut det = PersistedProjectDetails::default();
        det.creator_id = cid;
        det.created_unix = now;
        det.status = "Closed".into();
        det.uuid_bytes = uuid_closed.clone();
        s.alliance_war_state.project_details.insert(pid_closed, det);
    });
    let closed_req = pb::message(
        "eve_public.freelance.contributionmethod.itemdelivery.api.GetStatusRequest",
    )
    .set("delivery.uuid", uuid_closed)
    .must_encode();
    let closed_rsp = call(&router, &player, "eve_public.freelance.contributionmethod.itemdelivery.api.GetStatusRequest", closed_req, now);
    assert_eq!(closed_rsp.status_code, STATUS_OK);
    assert_eq!(WireMessage::decode(&closed_rsp.payload.unwrap().value).unwrap().fields[0].field_number, 3);
}
