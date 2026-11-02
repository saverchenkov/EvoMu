//! Batch 22 Quasar closure integration tests:
//! - Sovereignty Hub resources, fuel, upgrades, workforce, and star/planet resource configurations
//! - Mercenary Den ownership, evolution, and activity lifecycle
//! - Batch 22 Quasar services (`GetSharedRequest` for SKINR drafts, `GetRequest` / `GetAllRequest` for cosmetic market transactions)

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::{drain, id_req, rpc, sess, structure as make_structure, subscribe};
use eve_db::{corp_roles, InventoryItemRecord, PersistedMercenaryDen};
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field, WireMessage};
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
};
use eve_server::router::ServerContext;
use eve_server::services::sov_services::{
    TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB,
};

const PILOT: i64 = 90_000_001;
const OTHER: i64 = 90_000_002;
const CORP: i64 = 98_000_001;
const JITA: u32 = 30_000_142;
const PERIMETER: u32 = 30_000_144;
const HUB_A: i64 = 1_020_000_011;
const HUB_B: i64 = 1_020_000_012;
const PILOT_SHIP: i64 = 100_000_001;
const GAS_ITEM: i64 = 991_001;
const UPGRADE_ITEM: i64 = 991_003;
const MINOR_THREAT_1: u64 = 82_499;

fn wm(bytes: &[u8]) -> WireMessage {
    WireMessage::decode(bytes).expect("decode WireMessage")
}

fn sub(m: &WireMessage, field: u32) -> WireMessage {
    let b = m
        .fields
        .iter()
        .find(|f| f.field_number == field)
        .and_then(|f| f.value.as_bytes())
        .expect("submessage bytes");
    WireMessage::decode(b).expect("decode submessage")
}

fn varint(m: &WireMessage, field: u32) -> Option<u64> {
    m.fields.iter().find(|f| f.field_number == field).and_then(|f| f.value.as_varint())
}

fn fields(m: &WireMessage, field: u32) -> Vec<WireMessage> {
    m.fields
        .iter()
        .filter(|f| f.field_number == field)
        .filter_map(|f| f.value.as_bytes())
        .map(|b| WireMessage::decode(b).expect("decode repeated"))
        .collect()
}

fn bytes_field(m: &WireMessage, field: u32) -> Vec<u8> {
    m.fields
        .iter()
        .find(|f| f.field_number == field)
        .and_then(|f| f.value.as_bytes())
        .expect("bytes field")
        .to_vec()
}

#[tokio::test]
async fn test_b22_quasar_sov_hub_and_star_resources() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/fuel/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/planet/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/fuel/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(HUB_A, make_structure(HUB_A, CORP, JITA, TYPE_SOVEREIGNTY_HUB));
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = JITA;
            ch.active_ship_id = PILOT_SHIP;
        }
        snap.items.insert(
            GAS_ITEM,
            InventoryItemRecord {
                item_id: GAS_ITEM,
                type_id: TYPE_MAGMATIC_GAS,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut s = sess(61_001, PILOT, CORP, JITA, corp_roles::ROLE_DIRECTOR);
    let mut outsider = sess(61_002, OTHER, 98_000_002, JITA, 0);

    // 1. Star (DERIVED Equinox star_power fallback) & Planet resource configurations from SDE
    let star_rsp = rpc(&ctx, &mut s, "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest", Vec::new()).await;
    assert_eq!(star_rsp.status_code, STATUS_OK);
    let star_wm = wm(&star_rsp.payload.unwrap().value);
    let stars = fields(&star_wm, 1);
    assert!(!stars.is_empty(), "Jita star configuration must be returned from DERIVED Equinox star_power fallback");
    assert!(varint(&stars[0], 2).unwrap_or(0) > 0, "Star power output must be positive");

    let planet_rsp = rpc(&ctx, &mut s, "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest", Vec::new()).await;
    assert_eq!(planet_rsp.status_code, STATUS_OK);
    let planet_wm = wm(&planet_rsp.payload.unwrap().value);
    assert!(!fields(&planet_wm, 1).is_empty(), "Jita planet configurations must be returned from SDE");

    // 2. Hub resources & fuel state mutation
    let res_before = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, HUB_A as u64)).await;
    assert_eq!(res_before.status_code, STATUS_OK);
    let power_before = varint(&sub(&wm(&res_before.payload.unwrap().value), 1), 1).unwrap_or(0);
    assert_eq!(power_before, 800, "Jita star + planets provide 800 power");

    // Outsider cannot query or fuel another corp's hub
    let forbidden = rpc(&ctx, &mut outsider, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, HUB_A as u64)).await;
    assert_eq!(forbidden.status_code, STATUS_FORBIDDEN);

    let mut fuel_req = id_req(1, GAS_ITEM as u64);
    encode_varint_field(2, 200, &mut fuel_req);
    encode_bytes_field(3, &id_req(1, HUB_A as u64)[2..], &mut fuel_req);
    let add_rsp = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.AddRequest", fuel_req).await;
    assert_eq!(add_rsp.status_code, STATUS_OK, "{}", add_rsp.status_message);

    let fuel_get = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.GetRequest", id_req(1, HUB_A as u64)).await;
    assert_eq!(fuel_get.status_code, STATUS_OK);
    let fuel_qty = fields(&wm(&fuel_get.payload.unwrap().value), 1)
        .first()
        .and_then(|f| varint(f, 2));
    assert_eq!(fuel_qty, Some(200), "Hub fuel quantity updated to 200");
}

#[tokio::test]
async fn test_b22_quasar_sov_hub_upgrades_and_workforce() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(HUB_A, make_structure(HUB_A, CORP, JITA, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(HUB_B, make_structure(HUB_B, CORP, PERIMETER, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(1_000_000_301, make_structure(1_000_000_301, CORP, JITA, eve_server::services::sov_services::TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(1_000_000_301, 40_009_080);
        snap.sov_runtime.skyhook_active.insert(1_000_000_301, true);
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = JITA;
            ch.active_ship_id = PILOT_SHIP;
        }
        snap.items.insert(
            GAS_ITEM,
            InventoryItemRecord {
                item_id: GAS_ITEM,
                type_id: TYPE_MAGMATIC_GAS,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            UPGRADE_ITEM,
            InventoryItemRecord {
                item_id: UPGRADE_ITEM,
                type_id: MINOR_THREAT_1 as u32,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut s = sess(61_003, PILOT, CORP, JITA, corp_roles::ROLE_DIRECTOR);
    let mut rx = subscribe(&ctx, &mut s).await;

    // Fuel Hub A first
    let mut fuel_req = id_req(1, GAS_ITEM as u64);
    encode_varint_field(2, 200, &mut fuel_req);
    encode_bytes_field(3, &id_req(1, HUB_A as u64)[2..], &mut fuel_req);
    assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.AddRequest", fuel_req).await.status_code, STATUS_OK);

    // Install upgrade via ProcessConfigurationRequest
    let mut cfg_req = id_req(1, UPGRADE_ITEM as u64);
    let mut upg_cfg = Vec::new();
    encode_bytes_field(1, &id_req(1, MINOR_THREAT_1)[2..], &mut upg_cfg);
    encode_varint_field(2, 1, &mut upg_cfg); // online = true
    encode_bytes_field(2, &upg_cfg, &mut cfg_req);
    encode_bytes_field(3, &id_req(1, HUB_A as u64)[2..], &mut cfg_req);
    let cfg_rsp = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", cfg_req).await;
    assert_eq!(cfg_rsp.status_code, STATUS_OK, "{}", cfg_rsp.status_message);

    let notices = drain(&mut rx);
    assert!(
        notices.iter().any(|(t, _)| t == "eve_public.sovereignty.hub.upgrade.api.HubUpgradesConfiguredNotice"),
        "Expected HubUpgradesConfiguredNotice"
    );

    // Configure workforce export from Hub A to Perimeter
    let mut exp = Vec::new();
    encode_bytes_field(3, &id_req(1, PERIMETER as u64)[2..], &mut exp);
    encode_varint_field(2, 500, &mut exp);
    let mut export_cfg = Vec::new();
    encode_bytes_field(3, &exp, &mut export_cfg);
    let mut wf_req = id_req(1, HUB_A as u64);
    encode_bytes_field(2, &export_cfg, &mut wf_req);
    let wf_rsp = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", wf_req).await;
    assert_eq!(wf_rsp.status_code, STATUS_OK, "{}", wf_rsp.status_message);

    let wf_state = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.GetStateRequest", id_req(1, HUB_A as u64)).await;
    assert_eq!(wf_state.status_code, STATUS_OK);
    let wf_wm = wm(&wf_state.payload.unwrap().value);
    assert!(!wf_wm.fields.is_empty(), "Workforce GetStateResponse must contain state payload");
    let upg_count = ctx.db.with_snapshot(|snap| snap.sov_runtime.sov_hubs.get(&HUB_A).map(|h| h.upgrades.len()));
    assert_eq!(upg_count, Some(1), "Hub A must have 1 installed upgrade");
}

#[tokio::test]
async fn test_b22_quasar_mercenary_den_and_b22_cosmetics() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (skyhook_id, den_id) = (1_000_000_050i64, 1_000_000_060i64);
    let mut s = sess(61_004, PILOT, CORP, JITA, 0);
    let mut rx = subscribe(&ctx, &mut s).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(skyhook_id, make_structure(skyhook_id, CORP, JITA, TYPE_ORBITAL_SKYHOOK));
        snap.structures.insert(den_id, make_structure(den_id, CORP, JITA, 85_230));
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                type_id: 85_230,
                owner_char_id: PILOT,
                skyhook_id,
                solar_system_id: JITA,
                planet_id: 40_009_077,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 15,
                anarchy_level: 0,
                infomorphs: 0,
                simulated_at_unix: ctx.now_unix(),
                next_activity_generation_unix: 0,
            },
        );
        snap.skills.entry(PILOT).or_default().insert(
            85_233,
            eve_db::TrainedSkillRecord { type_id: 85_233, level: 3, skill_points: 0.0 },
        );
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = JITA;
        }
    });

    // 1. Mercenary Den ownership & activity lifecycle
    let owned = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest", Vec::new()).await;
    assert_eq!(owned.status_code, STATUS_OK);
    let owned_list = fields(&wm(&owned.payload.unwrap().value), 1);
    assert_eq!(owned_list.len(), 1, "Pilot owns 1 mercenary den");
    let acts = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64)).await;
    assert_eq!(acts.status_code, STATUS_OK);
    let act_list = fields(&wm(&acts.payload.unwrap().value), 1);
    assert!(!act_list.is_empty(), "Mercenary den should generate activities");

    let act_uuid = bytes_field(&sub(&act_list[0], 1), 1);
    let mut start_req = Vec::new();
    let mut id_msg = Vec::new();
    encode_bytes_field(1, &act_uuid, &mut id_msg);
    encode_bytes_field(1, &id_msg, &mut start_req);
    let start_rsp = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req).await;
    assert_eq!(start_rsp.status_code, STATUS_OK, "{}", start_rsp.status_message);
    let start_wm = wm(&start_rsp.payload.unwrap().value);
    assert!(!start_wm.fields.is_empty(), "StartResponse must contain activity attributes");

    let notices = drain(&mut rx);
    assert!(
        notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.StartedNotice"),
        "Expected StartedNotice"
    );

    // 2. Batch 22 Quasar services: Save SKINR draft & enforce sharing on GetSharedRequest
    let mut save_req = Vec::new();
    encode_string_field(1, "B22 Test Skin", &mut save_req);
    encode_string_field(2, "B22 Line", &mut save_req);
    let save_rsp = rpc(&ctx, &mut s, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest", save_req).await;
    assert_eq!(save_rsp.status_code, STATUS_OK, "{}", save_rsp.status_message);
    let save_wm = wm(&save_rsp.payload.unwrap().value);
    let draft_uuid = bytes_field(&sub(&save_wm, 1), 1);

    let mut other = sess(61_005, OTHER, 98_000_002, JITA, 0);
    let mut shared_req = id_req(1, PILOT as u64);
    let mut d_id = Vec::new();
    encode_bytes_field(1, &draft_uuid, &mut d_id);
    encode_bytes_field(2, &d_id, &mut shared_req);

    // A draft shared by link (character + draft UUID) is readable by any authenticated holder of the link
    let owner_rsp = rpc(&ctx, &mut s, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest", shared_req.clone()).await;
    assert_eq!(owner_rsp.status_code, STATUS_OK);
    let shared_rsp = rpc(&ctx, &mut other, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest", shared_req).await;
    assert_eq!(shared_rsp.status_code, STATUS_OK, "{}", shared_rsp.status_message);

    let mut missing_draft_req = id_req(1, PILOT as u64);
    let mut missing_d_id = Vec::new();
    encode_bytes_field(1, &[0xABu8; 16], &mut missing_d_id);
    encode_bytes_field(2, &missing_d_id, &mut missing_draft_req);
    assert_eq!(
        rpc(&ctx, &mut other, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest", missing_draft_req)
            .await
            .status_code,
        STATUS_NOT_FOUND
    );

    // 3. Cosmetic market transactions: GetAllRequest starts empty (no fabricated next_seq),
    // records real SKIN listing creation transactions, and enforces participant authorization.
    let tx_all_empty = rpc(&ctx, &mut s, "eve_public.cosmetic.market.transaction.api.GetAllRequest", Vec::new()).await;
    assert_eq!(tx_all_empty.status_code, STATUS_OK);
    assert!(fields(&wm(&tx_all_empty.payload.unwrap().value), 1).is_empty());

    // Grant 1 unactivated SKIN license to PILOT and create a listing via InitiateCreateListingRequest
    let mut grant_req = Vec::new();
    let mut g_skin_id = Vec::new();
    encode_string_field(1, "deadbeefcafebabe", &mut g_skin_id);
    encode_bytes_field(1, &g_skin_id, &mut grant_req);
    encode_varint_field(2, 1, &mut grant_req);
    let mut gm = sess(61_006, PILOT, CORP, JITA, 0);
    gm.role = u64::MAX;
    assert_eq!(
        rpc(&ctx, &mut gm, "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest", grant_req)
            .await
            .status_code,
        STATUS_OK
    );

    let mut price_isk = Vec::new();
    encode_varint_field(1, 500_000 * 2, &mut price_isk);
    let mut skin_id = Vec::new();
    encode_string_field(1, "deadbeefcafebabe", &mut skin_id);
    let mut lic_id = Vec::new();
    encode_bytes_field(1, &id_req(1, PILOT as u64)[2..], &mut lic_id);
    encode_bytes_field(2, &skin_id, &mut lic_id);
    let mut dur = Vec::new();
    encode_varint_field(1, 86_400, &mut dur);
    let mut target = Vec::new();
    encode_varint_field(1, 1, &mut target);
    let mut create_req = Vec::new();
    encode_bytes_field(1, &price_isk, &mut create_req);
    encode_bytes_field(3, &lic_id, &mut create_req);
    encode_bytes_field(4, &dur, &mut create_req);
    encode_varint_field(5, 1, &mut create_req);
    encode_bytes_field(6, &target, &mut create_req);

    let create_rsp = rpc(
        &ctx,
        &mut s,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        create_req,
    )
    .await;
    assert_eq!(create_rsp.status_code, STATUS_OK, "{}", create_rsp.status_message);
    let tx_uuid = bytes_field(&sub(&wm(&create_rsp.payload.unwrap().value), 1), 1);

    let tx_all_pilot = rpc(&ctx, &mut s, "eve_public.cosmetic.market.transaction.api.GetAllRequest", Vec::new()).await;
    assert_eq!(tx_all_pilot.status_code, STATUS_OK);
    let pilot_txs = fields(&wm(&tx_all_pilot.payload.unwrap().value), 1);
    assert_eq!(pilot_txs.len(), 1);
    assert_eq!(bytes_field(&pilot_txs[0], 1), tx_uuid);

    let tx_all_other = rpc(&ctx, &mut other, "eve_public.cosmetic.market.transaction.api.GetAllRequest", Vec::new()).await;
    assert_eq!(tx_all_other.status_code, STATUS_OK);
    assert!(fields(&wm(&tx_all_other.payload.unwrap().value), 1).is_empty());

    let mut get_tx_req = Vec::new();
    let mut get_tx_id = Vec::new();
    encode_bytes_field(1, &tx_uuid, &mut get_tx_id);
    encode_bytes_field(1, &get_tx_id, &mut get_tx_req);
    let tx_rsp = rpc(&ctx, &mut s, "eve_public.cosmetic.market.transaction.api.GetRequest", get_tx_req.clone()).await;
    assert_eq!(tx_rsp.status_code, STATUS_OK);
    let tx_attrs = sub(&wm(&tx_rsp.payload.unwrap().value), 1);
    assert!(varint(&sub(&tx_attrs, 1), 1).unwrap_or(0) > 0, "last_updated seconds must be set");
    assert_eq!(varint(&tx_attrs, 2), Some(3), "status must be SUCCESSFUL (3)");

    let tx_other = rpc(&ctx, &mut other, "eve_public.cosmetic.market.transaction.api.GetRequest", get_tx_req).await;
    assert_eq!(tx_other.status_code, STATUS_FORBIDDEN);

    // Missing draft / transaction return STATUS_NOT_FOUND; malformed UUID returns STATUS_BAD_REQUEST
    let mut bad_req = id_req(1, PILOT as u64);
    let mut bad_id = Vec::new();
    encode_bytes_field(1, b"short", &mut bad_id);
    encode_bytes_field(2, &bad_id, &mut bad_req);
    assert_eq!(
        rpc(&ctx, &mut other, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest", bad_req).await.status_code,
        STATUS_BAD_REQUEST
    );
    let mut missing_tx = Vec::new();
    let mut tx_id = Vec::new();
    encode_bytes_field(1, &[0xFFu8; 16], &mut tx_id);
    encode_bytes_field(1, &tx_id, &mut missing_tx);
    assert_eq!(
        rpc(&ctx, &mut s, "eve_public.cosmetic.market.transaction.api.GetRequest", missing_tx).await.status_code,
        STATUS_NOT_FOUND
    );
}
