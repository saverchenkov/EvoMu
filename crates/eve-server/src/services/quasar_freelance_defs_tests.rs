//! Unit tests for `quasar_freelance_defs`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_OK};
use eve_db::{CorpProjectRecord, InventoryFlag, InventoryItemRecord, PersistedProjectDetails, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    DynamicQuasarMessage, EvePublicRequest, EvePublicResponse, GoogleAny, QuasarDescriptorRegistry,
    WireMessage,
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

    let mut maj1 = Vec::new();
    encode_varint_field(1, 1, &mut maj1);
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
    let mut maj2 = Vec::new();
    encode_varint_field(1, 2, &mut maj2);
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
    let mut status_req = Vec::new();
    encode_uuid_field(1, &uuid, &mut status_req);
    let st_rsp1 = call(
        &router,
        &player,
        "eve_public.freelance.contributionmethod.itemdelivery.api.GetStatusRequest",
        status_req.clone(),
        now,
    );
    assert_eq!(st_rsp1.status_code, STATUS_OK);

    // DeliverRequest (deliver 50 units of Tritanium)
    let mut deliv_req = Vec::new();
    encode_uuid_field(1, &uuid, &mut deliv_req);
    encode_varint_field(4, 50, &mut deliv_req);
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
