//! Equinox Sovereignty Hub state tests: resources, fuel, upgrades and workforce transfers.
//!
//! Every notice asserted here is produced by a server-side state transition triggered through a
//! real Quasar `publicGatewaySvc.SendRequest` RPC (or the hourly fuel simulation those RPCs run),
//! never by calling a `publish_*` helper.

#![forbid(unsafe_code)]

use eve_db::{corp_roles, InventoryItemRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicNotice, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::sov_services::{TYPE_MAGMATIC_GAS, TYPE_SOVEREIGNTY_HUB};
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

const PILOT: i64 = 90_000_001;
const OTHER: i64 = 90_000_002;
const CORP: i64 = 98_000_001;
const JITA: u32 = 30_000_142;
const PERIMETER: u32 = 30_000_144;
const HUB_A: i64 = 1_020_000_001;
const HUB_B: i64 = 1_020_000_002;
const GAS_ITEM: i64 = 990_001;
const UPGRADE_ITEM: i64 = 990_003;
const STATION_GAS: i64 = 990_004;
const STATION_UPGRADE: i64 = 990_005;
/// Demo-seed active ship of `PILOT` and the Jita 4-4 station.
const PILOT_SHIP: i64 = 100_000_001;
const JITA_STATION: i64 = 60_003_760;
/// SDE `Minor Threat Detection Array 1` (group 4768 Sovereignty Hub Site Detection Upgrades).
const MINOR_THREAT_1: u64 = 82_499;

pub(crate) fn sess(sid: u64, cid: i64, corp: i64, sys: u32, corp_role: u64) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(1);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp);
    s.solar_system_id = Some(sys);
    s.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    s.corp_role = corp_role;
    s
}

pub(crate) async fn rpc(ctx: &ServerContext, s: &mut SessionState, ty: &str, value: Vec<u8>) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"equinox-hub".to_vec(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{ty}"), value }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-equinox".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(s, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None)
        .await
        .expect("SendRequest");
    let PyRep::KeyVal(entries) = &rep else { panic!("expected KeyVal, got {rep:?}") };
    let bytes = entries
        .iter()
        .find_map(|(k, v)| match (k.as_str(), v) {
            ("response_proto", PyRep::Bytes(b)) => Some(b.clone()),
            _ => None,
        })
        .expect("response_proto");
    EvePublicResponse::decode_proto(&bytes).expect("decode EvePublicResponse")
}

pub(crate) async fn subscribe(ctx: &ServerContext, s: &mut SessionState) -> Receiver<EvePublicNotice> {
    ctx.dispatch_rpc(s, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None).await.expect("Subscribe");
    ctx.quasar_gateway.take_stream_receiver(s.session_id).expect("stream rx")
}

pub(crate) fn drain(rx: &mut Receiver<EvePublicNotice>) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        if let Some(p) = n.payload {
            out.push((p.normalized_type_name().to_string(), p.value));
        }
    }
    out
}

pub(crate) fn idm(v: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, v, &mut b);
    b
}

pub(crate) fn id_req(field: u32, v: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_bytes_field(field, &idm(v), &mut b);
    b
}

pub(crate) fn field(bytes: &[u8], n: u32) -> Option<Vec<u8>> {
    let wm = WireMessage::decode(bytes).ok()?;
    wm.fields.iter().find(|f| f.field_number == n).and_then(|f| f.value.as_bytes().map(|b| b.to_vec()))
}

pub(crate) fn fields(bytes: &[u8], n: u32) -> Vec<Vec<u8>> {
    let wm = WireMessage::decode(bytes).expect("decode");
    wm.fields.iter().filter(|f| f.field_number == n).filter_map(|f| f.value.as_bytes().map(|b| b.to_vec())).collect()
}

pub(crate) fn varint(bytes: &[u8], n: u32) -> Option<u64> {
    let wm = WireMessage::decode(bytes).ok()?;
    wm.fields.iter().find(|f| f.field_number == n).and_then(|f| f.value.as_varint())
}

fn ok_payload(r: EvePublicResponse) -> Vec<u8> {
    assert_eq!(r.status_code, STATUS_OK, "unexpected status: {}", r.status_message);
    r.payload.map(|p| p.value).unwrap_or_default()
}

pub(crate) fn structure(id: i64, owner: i64, sys: u32, type_id: u32) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: id,
        type_id,
        owner_corp_id: owner,
        solar_system_id: sys,
        name: format!("Structure {id}"),
        position: [200_000.0, 0.0, 0.0],
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 0,
        shield_hp: 5_000.0,
        armor_hp: 5_000.0,
        hull_hp: 5_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

fn resources(ctx_payload: &[u8]) -> (u64, u64, u64, u64) {
    let p = field(ctx_payload, 1).expect("power");
    let w = field(ctx_payload, 2).expect("workforce");
    (varint(&p, 1).unwrap_or(0), varint(&p, 2).unwrap_or(0), varint(&w, 1).unwrap_or(0), varint(&w, 2).unwrap_or(0))
}

fn fuel_add(item: i64, amount: u64, hub: i64) -> Vec<u8> {
    let mut b = id_req(1, item as u64);
    encode_varint_field(2, amount, &mut b);
    encode_bytes_field(3, &idm(hub as u64), &mut b);
    b
}

fn configure(hub: i64, cfg: Vec<u8>) -> Vec<u8> {
    let mut b = id_req(1, hub as u64);
    encode_bytes_field(2, &cfg, &mut b);
    b
}

fn has(notices: &[(String, Vec<u8>)], name: &str) -> bool {
    notices.iter().any(|(t, _)| t == name)
}

#[tokio::test]
async fn test_equinox_hub_resources_fuel_and_upgrades_state_backed() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/fuel/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/hub/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/fuel/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/upgrade/api/requests_pb2.pyj",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s = sess(61_001, PILOT, CORP, JITA, corp_roles::ROLE_DIRECTOR);
    let mut rx = subscribe(&ctx, &mut s).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(HUB_A, structure(HUB_A, CORP, JITA, TYPE_SOVEREIGNTY_HUB));
        // Fuel in the pilot's active ship (pilot is in the hub's system), upgrade inside the hub,
        // plus decoy stacks left in a Jita station hangar that must not be consumable.
        for (id, ty, qty, loc) in [
            (GAS_ITEM, TYPE_MAGMATIC_GAS, 1_000, PILOT_SHIP),
            (UPGRADE_ITEM, MINOR_THREAT_1 as u32, 1, HUB_A),
            (STATION_GAS, TYPE_MAGMATIC_GAS, 1_000, JITA_STATION),
            (STATION_UPGRADE, MINOR_THREAT_1 as u32, 1, JITA_STATION),
        ] {
            snap.items.insert(id, InventoryItemRecord { item_id: id, type_id: ty, owner_id: PILOT, location_id: loc, quantity: qty, ..Default::default() });
        }
    });

    // Definitions enumerate SDE upgrade types.
    let defs = ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest", Vec::new()).await);
    let def = fields(&defs, 1).into_iter().find(|d| field(d, 1).and_then(|i| varint(&i, 1)) == Some(MINOR_THREAT_1)).expect("SDE upgrade 82499");
    assert_eq!((varint(&def, 2), varint(&def, 3)), (Some(200), Some(1_000)));

    // Resources: Jita star (Sun O1, 800) + 2 Gas planets (300) power; Temperate + 3 Barren (3000) workforce.
    let res = ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, HUB_A as u64)).await);
    assert_eq!(resources(&res), (1_100, 0, 3_000, 0));

    // Fuel: ownership, role and inventory are enforced; fuel is deducted from the caller's stack.
    let mut other = sess(61_002, OTHER, 98_000_002, JITA, corp_roles::ROLE_DIRECTOR);
    assert_eq!(rpc(&ctx, &mut other, "eve_public.sovereignty.hub.fuel.api.AddRequest", fuel_add(GAS_ITEM, 10, HUB_A)).await.status_code, STATUS_FORBIDDEN);
    let mut no_role = sess(61_003, PILOT, CORP, JITA, 0);
    assert_eq!(rpc(&ctx, &mut no_role, "eve_public.sovereignty.hub.fuel.api.AddRequest", fuel_add(GAS_ITEM, 10, HUB_A)).await.status_code, STATUS_FORBIDDEN);
    assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.AddRequest", fuel_add(GAS_ITEM, 5_000, HUB_A)).await.status_code, STATUS_BAD_REQUEST);
    assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.AddRequest", fuel_add(STATION_GAS, 10, HUB_A)).await.status_code, STATUS_BAD_REQUEST, "fuel in a station hangar is not at the hub");
    ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.AddRequest", fuel_add(GAS_ITEM, 500, HUB_A)).await);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.items.get(&GAS_ITEM).map(|i| i.quantity)), Some(500));
    let fuel = ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.GetRequest", id_req(1, HUB_A as u64)).await);
    assert_eq!(field(&fuel, 1).and_then(|f| varint(&f, 2)), Some(500));
    drain(&mut rx);

    // Install + online the upgrade item: consumes the item and 50 startup fuel, allocates budget.
    let mut remote = id_req(1, STATION_UPGRADE as u64);
    encode_bytes_field(3, &idm(HUB_A as u64), &mut remote);
    assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", remote).await.status_code, STATUS_BAD_REQUEST, "upgrade in a station hangar is not at the hub");
    let mut proc = id_req(1, UPGRADE_ITEM as u64);
    let mut cfg = Vec::new();
    encode_bytes_field(1, &idm(MINOR_THREAT_1), &mut cfg);
    encode_varint_field(2, 1, &mut cfg);
    encode_bytes_field(2, &cfg, &mut proc);
    encode_bytes_field(3, &idm(HUB_A as u64), &mut proc);
    let hu = ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc).await);
    let up = field(&field(&hu, 1).unwrap(), 2).expect("installed upgrade");
    assert_eq!(field(&up, 2).and_then(|a| varint(&a, 2)), Some(2), "POWER_STATE_ONLINE");
    let n = drain(&mut rx);
    assert!(has(&n, "eve_public.sovereignty.hub.upgrade.api.HubUpgradesConfiguredNotice"));
    assert!(has(&n, "eve_public.sovereignty.hub.api.ResourcesSimulatedNotice"));
    assert!(ctx.db.with_snapshot(|snap| !snap.items.contains_key(&UPGRADE_ITEM)));
    let res = ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, HUB_A as u64)).await);
    assert_eq!(resources(&res), (1_100, 200, 3_000, 1_000));

    // Hourly fuel burn (5/h): 10h burns 50; 100 more hours exhausts the bay -> POWER_STATE_LOW.
    // Reconfiguring at +59m and +118m must not reset the burn clock: one full hour still burns.
    let reconfig = || {
        let mut b = Vec::new();
        let mut c = Vec::new();
        encode_bytes_field(1, &idm(MINOR_THREAT_1), &mut c);
        encode_varint_field(2, 1, &mut c);
        encode_bytes_field(2, &c, &mut b);
        encode_bytes_field(3, &idm(HUB_A as u64), &mut b);
        b
    };
    for _ in 0..2 {
        ctx.advance_secs(59 * 60);
        ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", reconfig()).await);
    }
    let fuel = ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.GetRequest", id_req(1, HUB_A as u64)).await);
    assert_eq!(field(&fuel, 1).and_then(|f| varint(&f, 2)), Some(445), "60m of the 118m elapsed burned 5 units");
    ctx.advance_secs(36_000 - 2 * 59 * 60);
    let fuel = ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.GetRequest", id_req(1, HUB_A as u64)).await);
    assert_eq!(field(&fuel, 1).and_then(|f| varint(&f, 2)), Some(400));
    assert_eq!(field(&fuel, 1).and_then(|f| varint(&f, 3)), Some(5));
    ctx.advance_secs(360_000);
    let hu = ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.upgrade.api.GetHubUpgradesRequest", id_req(1, HUB_A as u64)).await);
    let up = field(&field(&hu, 1).unwrap(), 2).unwrap();
    assert_eq!(field(&up, 2).and_then(|a| varint(&a, 2)), Some(3), "POWER_STATE_LOW");
    let n = drain(&mut rx);
    assert!(has(&n, "eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice"));
    let sim = n.iter().find(|(t, _)| t == "eve_public.sovereignty.hub.api.ResourcesSimulatedNotice").expect("resources simulated");
    assert_eq!(field(&sim.1, 3).and_then(|p| varint(&p, 2)), Some(0), "low-power upgrade releases its power allocation");

    // Re-onlining a starved upgrade without fuel keeps it POWER_STATE_LOW.
    let hu = ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", reconfig()).await);
    let up = field(&field(&hu, 1).unwrap(), 2).unwrap();
    assert_eq!(field(&up, 2).and_then(|a| varint(&a, 2)), Some(3), "still POWER_STATE_LOW without fuel");
    drain(&mut rx);

    // Refuelling restores the upgrade (simulated notice again).
    ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.AddRequest", fuel_add(GAS_ITEM, 100, HUB_A)).await);
    assert!(has(&drain(&mut rx), "eve_public.sovereignty.hub.upgrade.api.HubUpgradesSimulatedNotice"));

    // Query installed upgrade attributes via GetRequest
    let mut ident = Vec::new();
    encode_bytes_field(1, &idm(HUB_A as u64), &mut ident);
    encode_bytes_field(2, &idm(MINOR_THREAT_1), &mut ident);
    let mut get_up = Vec::new();
    encode_bytes_field(1, &ident, &mut get_up);
    let up_resp = ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.upgrade.api.GetRequest", get_up).await);
    assert!(field(&up_resp, 1).is_some());

    // Uninstall returns the upgrade item and emits UninstalledNotice.
    let mut un = Vec::new();
    encode_bytes_field(1, &ident, &mut un);
    assert_eq!(rpc(&ctx, &mut no_role, "eve_public.sovereignty.hub.upgrade.api.UninstallRequest", un.clone()).await.status_code, STATUS_FORBIDDEN);
    ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.upgrade.api.UninstallRequest", un).await);
    assert!(has(&drain(&mut rx), "eve_public.sovereignty.hub.upgrade.api.UninstalledNotice"));
    assert!(ctx.db.with_snapshot(|snap| snap.items.values().any(|i| i.type_id == MINOR_THREAT_1 as u32 && i.owner_id == PILOT && i.location_id == HUB_A)));
}

#[tokio::test]
async fn test_equinox_hub_workforce_transfer_network_state_backed() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s = sess(61_011, PILOT, CORP, JITA, corp_roles::ROLE_STATION_MANAGER);
    let mut rx = subscribe(&ctx, &mut s).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(HUB_A, structure(HUB_A, CORP, JITA, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(HUB_B, structure(HUB_B, CORP, PERIMETER, TYPE_SOVEREIGNTY_HUB));
    });

    let net = ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest", id_req(1, HUB_A as u64)).await);
    let hubs = fields(&net, 1);
    assert_eq!(hubs.len(), 1);
    assert_eq!(field(&hubs[0], 1).and_then(|h| varint(&h, 1)), Some(HUB_B as u64));
    assert_eq!(field(&hubs[0], 2).and_then(|h| varint(&h, 1)), Some(PERIMETER as u64));

    let cfg = ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.GetConfigurationRequest", id_req(1, HUB_A as u64)).await);
    assert_eq!(field(&cfg, 1).and_then(|c| varint(&c, 1)), Some(1), "default mode inactive");
    let b_before = resources(&ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, HUB_B as u64)).await));

    // Export 1000 workforce from Jita to Perimeter: disconnected until Perimeter imports.
    let mut exp = Vec::new();
    encode_bytes_field(3, &idm(PERIMETER as u64), &mut exp);
    encode_varint_field(2, 1_000, &mut exp);
    let mut export_cfg = Vec::new();
    encode_bytes_field(3, &exp, &mut export_cfg);
    let mut too_much = Vec::new();
    encode_bytes_field(3, &idm(PERIMETER as u64), &mut too_much);
    encode_varint_field(2, 100_000, &mut too_much);
    let mut too_much_cfg = Vec::new();
    encode_bytes_field(3, &too_much, &mut too_much_cfg);
    assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", configure(HUB_A, too_much_cfg)).await.status_code, STATUS_BAD_REQUEST);
    let mut other = sess(61_012, OTHER, 98_000_002, JITA, corp_roles::ROLE_DIRECTOR);
    assert_eq!(rpc(&ctx, &mut other, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", configure(HUB_A, export_cfg.clone())).await.status_code, STATUS_FORBIDDEN);
    ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", configure(HUB_A, export_cfg)).await);
    let n = drain(&mut rx);
    let conf = n.iter().find(|(t, _)| t == "eve_public.sovereignty.resource.transfer.workforce.api.ConfiguredNotice").expect("ConfiguredNotice");
    assert_eq!(field(&conf.1, 4).and_then(|h| varint(&h, 1)), Some(HUB_A as u64));
    assert!(field(&conf.1, 3).and_then(|c| field(&c, 3)).is_some(), "new configuration is export");
    let st = ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.GetStateRequest", id_req(1, HUB_A as u64)).await);
    assert!(field(&st, 1).and_then(|s| field(&s, 3)).and_then(|e| field(&e, 6)).is_some(), "export disconnected");

    // Perimeter imports from Jita: Jita's export becomes connected (StateChangedNotice in Jita).
    let mut src = Vec::new();
    encode_bytes_field(1, &idm(JITA as u64), &mut src);
    let mut imp = Vec::new();
    encode_bytes_field(1, &src, &mut imp);
    let mut import_cfg = Vec::new();
    encode_bytes_field(2, &imp, &mut import_cfg);
    ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", configure(HUB_B, import_cfg)).await);
    let n = drain(&mut rx);
    let changed = n.iter().find(|(t, _)| t == "eve_public.sovereignty.resource.transfer.workforce.api.StateChangedNotice").expect("StateChangedNotice for Jita hub");
    assert_eq!(field(&changed.1, 4).and_then(|h| varint(&h, 1)), Some(HUB_A as u64));
    assert!(field(&changed.1, 3).and_then(|s| field(&s, 3)).and_then(|e| field(&e, 5)).is_some(), "export connected");
    let st_b = ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.GetStateRequest", id_req(1, HUB_B as u64)).await);
    let src_state = field(&field(&field(&st_b, 1).unwrap(), 2).unwrap(), 1).expect("import source");
    assert_eq!(varint(&src_state, 2), Some(1_000));
    let b_after = resources(&ok_payload(rpc(&ctx, &mut s, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, HUB_B as u64)).await));
    assert_eq!(b_after.2, b_before.2 + 1_000, "imported workforce raises Perimeter availability");
}
