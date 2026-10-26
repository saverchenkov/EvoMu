//! State-backed Quasar asset-holding entitlements, ship logos / SKIN state, corporation palettes
//! and structure paintwork licenses, driven end-to-end through Quasar requests and client RPCs.

use crate::quasar_goal_tale_notices::{bytes, drain, seq, sub, subscribe, varint};
use crate::quasar_progression_complete::{build_sess, quasar_rpc};
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field, WireMessage};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const CORP: u32 = 98_000_881;
const OTHER_CORP: u32 = 98_000_882;

fn player(sid: u64, cid: u32, corp: u32, corp_role: u64) -> SessionState {
    let mut s = build_sess(sid, cid, corp);
    s.user_id = Some(sid); // distinct accounts (avoids the multi-login lock on user 1)
    s.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    s.corp_role = corp_role;
    s
}

fn place_char(ctx: &ServerContext, cid: i64, corp: u32, system: Option<u32>) {
    ctx.db.with_mut_snapshot(|s| {
        let template = s.characters[&90_000_001].clone();
        let ch = s.characters.entry(cid).or_insert_with(|| template);
        ch.character_id = cid;
        ch.corporation_id = corp as i64;
        ch.alliance_id = None;
        if let Some(sys) = system {
            ch.solar_system_id = sys;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
}

fn give_ship(ctx: &ServerContext, cid: i64, ship_id: i64, type_id: u32, location: i64) {
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id, owner_id: cid, location_id: location, flag: InventoryFlag::Hangar,
            quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        s.characters.get_mut(&cid).unwrap().active_ship_id = ship_id;
    });
}

fn decode(rsp: &eve_proto::quasar::EvePublicResponse) -> WireMessage {
    WireMessage::decode(&rsp.payload.as_ref().expect("payload").value).expect("decode")
}

fn ident(holder: u32, asset: &[u8], group: &[u8; 16]) -> Vec<u8> {
    let (mut ch, mut h, mut a, mut g, mut out) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, holder as u64, &mut ch);
    encode_bytes_field(1, &ch, &mut h);
    encode_bytes_field(1, asset, &mut a);
    encode_string_field(1, "dailygoal", &mut g);
    encode_bytes_field(2, group, &mut g);
    encode_bytes_field(1, &h, &mut out);
    encode_bytes_field(2, &a, &mut out);
    encode_bytes_field(3, &g, &mut out);
    out
}

fn wrap(field: u32, inner: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(field, inner, &mut out);
    out
}

#[tokio::test]
async fn entitlement_notices_follow_delivery_and_redemption() {
    // parity: quasar:eveProto/generated/eve_public/assetholding/entitlement/api/notices_pb2.pyj
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    place_char(&ctx, 90_000_002, CORP, None);
    let mut gm = build_sess(61_001, 90_000_001, CORP);
    let mut holder = player(61_002, 90_000_002, CORP, 0);
    let mut other = player(61_003, 90_000_001, CORP, 0);
    let mut rx = subscribe(&ctx, &mut holder).await;

    // GM spawns an item-stack asset: 100 Tritanium per entitlement unit.
    let (mut stack, mut stacks) = (Vec::new(), Vec::new());
    seq(1, 34, &mut stack);
    encode_varint_field(2, 100, &mut stack);
    encode_bytes_field(1, &stack, &mut stacks);
    let mut spawn = Vec::new();
    encode_string_field(2, "Daily goal reward", &mut spawn);
    encode_bytes_field(6, &stacks, &mut spawn);
    assert_eq!(quasar_rpc(&ctx, &mut holder, "eve.assetholding.asset.api.SpawnRequest", spawn.clone()).await.status_code, 403);
    let rsp = quasar_rpc(&ctx, &mut gm, "eve.assetholding.asset.api.SpawnRequest", spawn).await;
    assert_eq!(rsp.status_code, STATUS_OK, "{}", rsp.status_message);
    let asset = bytes(&sub(&decode(&rsp), 1), 1);

    // GM direct-delivers 2 units to the holder -> GrantedNotice to the holder.
    let group = *b"GOALGOALGOALGOAL";
    let id = ident(90_000_002, &asset, &group);
    let mut entry = wrap(2, &id);
    encode_varint_field(10, 2, &mut entry);
    let rsp = quasar_rpc(&ctx, &mut gm, "eve.assetholding.entitlement.api.DirectDeliveryRequest", wrap(2, &entry)).await;
    assert_eq!(rsp.status_code, STATUS_OK, "{}", rsp.status_message);
    let granted = drain(&mut rx, "eve_public.assetholding");
    assert_eq!(granted.len(), 1);
    assert_eq!(granted[0].0, "eve_public.assetholding.entitlement.api.GrantedNotice");
    let gid = sub(&granted[0].1, 1);
    assert_eq!(varint(&sub(&sub(&gid, 1), 1), 1), Some(90_000_002), "holder character");
    assert_eq!(bytes(&sub(&gid, 2), 1), asset);
    assert_eq!(bytes(&sub(&gid, 3), 2), group.to_vec(), "group_by uuid the client keys rewards on");
    assert_eq!(varint(&granted[0].1, 2), Some(2));

    let get = |q: &WireMessage| (varint(&sub(&sub(q, 2), 1), 1), varint(&sub(&sub(q, 2), 1), 2));
    let rsp = quasar_rpc(&ctx, &mut holder, "eve_public.assetholding.entitlement.api.GetRequest", wrap(1, &id)).await;
    assert_eq!(get(&decode(&rsp)), (Some(2), Some(0)), "total 2, nothing redeemed");
    assert_eq!(quasar_rpc(&ctx, &mut other, "eve_public.assetholding.entitlement.api.RedeemRequest", wrap(1, &id)).await.status_code, 403);

    // Redeem -> 200 Tritanium materialize in the holder's station hangar + RedeemedNotice.
    let (location, before) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&90_000_002];
        let loc = ch.station_id.unwrap_or(ch.home_station_id);
        (loc, s.items.values().filter(|i| i.owner_id == 90_000_002 && i.type_id == 34 && i.location_id == loc).map(|i| i.quantity).sum::<i32>())
    });
    let rsp = quasar_rpc(&ctx, &mut holder, "eve_public.assetholding.entitlement.api.RedeemRequest", wrap(1, &id)).await;
    assert_eq!(rsp.status_code, STATUS_OK, "{}", rsp.status_message);
    let after = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.owner_id == 90_000_002 && i.type_id == 34 && i.location_id == location && i.flag == InventoryFlag::Hangar).map(|i| i.quantity).sum::<i32>()
    });
    assert_eq!(after - before, 200);
    let redeemed = drain(&mut rx, "eve_public.assetholding");
    assert_eq!(redeemed.len(), 1);
    assert_eq!(redeemed[0].0, "eve_public.assetholding.entitlement.api.RedeemedNotice");
    assert_eq!(bytes(&sub(&sub(&redeemed[0].1, 1), 3), 2), group.to_vec());
    assert_eq!(varint(&redeemed[0].1, 2), Some(2));
    assert_eq!(quasar_rpc(&ctx, &mut holder, "eve_public.assetholding.entitlement.api.RedeemRequest", wrap(1, &id)).await.status_code, 400);
    let rsp = quasar_rpc(&ctx, &mut holder, "eve_public.assetholding.entitlement.api.GetRequest", wrap(1, &id)).await;
    assert_eq!(get(&decode(&rsp)), (Some(2), Some(2)));
}

#[tokio::test]
async fn ship_logos_and_skin_state_follow_owned_ships() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/logo/logo_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/api/request_pb2.pyj, quasar:eveProto/generated/eve_public/cosmetic/ship/api/notice_pb2.pyj
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw: HashMap<String, PyRep> = HashMap::new();
    let mut a = SessionState::new(62_001);
    ctx.dispatch_rpc(&mut a, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.expect("Login");
    ctx.dispatch_rpc(&mut a, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.expect("select");
    let (system, ship_a, ship_type) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&90_000_001];
        (ch.solar_system_id, ch.active_ship_id, s.items[&ch.active_ship_id].type_id)
    });
    // A pilot of another corporation already in space in the same system, flying a Rifter.
    place_char(&ctx, 90_000_002, OTHER_CORP, Some(system));
    give_ship(&ctx, 90_000_002, 720_500_001, 587, system as i64);
    let mut b = player(62_002, 90_000_002, OTHER_CORP, 0);
    b.solar_system_id = Some(system);
    let mut rx_b = subscribe(&ctx, &mut b).await;
    let mut a_q = player(62_003, 90_000_001, ctx.db.with_snapshot(|s| s.characters[&90_000_001].corporation_id as u32), 0);
    let mut rx_a = subscribe(&ctx, &mut a_q).await;

    // Logos: owner-only, real ship, non-negative slot, alliance logo needs an alliance.
    let logo = |ship: i64, index: i32, kind: Option<u32>| {
        let mut id = Vec::new();
        seq(1, ship as u64, &mut id);
        encode_varint_field(2, index as u64, &mut id);
        let mut out = wrap(1, &id);
        if let Some(k) = kind {
            encode_bytes_field(2, &wrap(k, &[]), &mut out);
        }
        out
    };
    assert_eq!(quasar_rpc(&ctx, &mut a_q, "eve_public.cosmetic.ship.logo.DisplayRequest", logo(ship_a, 0, Some(2))).await.status_code, STATUS_OK);
    assert_eq!(quasar_rpc(&ctx, &mut a_q, "eve_public.cosmetic.ship.logo.DisplayRequest", logo(ship_a, 1, Some(1))).await.status_code, 403);
    assert_eq!(quasar_rpc(&ctx, &mut b, "eve_public.cosmetic.ship.logo.DisplayRequest", logo(ship_a, 0, Some(2))).await.status_code, 403);
    assert_eq!(quasar_rpc(&ctx, &mut a_q, "eve_public.cosmetic.ship.logo.DisplayRequest", logo(999_999_999, 0, Some(2))).await.status_code, 404);
    let logos = |ctx: &ServerContext| ctx.db.with_snapshot(|s| {
        let v: serde_json::Value = serde_json::from_str(&s.quasar_state_blobs["quasar_cosmetics_live"]).unwrap();
        v["ship_logos"][ship_a.to_string()].clone()
    });
    assert_eq!(logos(&ctx), serde_json::json!({"0": 2}), "corporation logo persisted in slot 0");
    assert_eq!(quasar_rpc(&ctx, &mut a_q, "eve_public.cosmetic.ship.logo.ClearRequest", logo(ship_a, 0, None)).await.status_code, STATUS_OK);
    assert_eq!(logos(&ctx), serde_json::Value::Null);

    // Undocking puts A in space: A receives every ship SKIN state in the system.
    ctx.dispatch_rpc(&mut a, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let arrival = drain(&mut rx_a, "eve_public.cosmetic");
    let bubble = arrival.iter().find(|(n, _)| n == "eve_public.cosmetic.ship.api.SetAllInBubbleNotice").expect("bubble notice");
    let ships: Vec<u64> = bubble.1.fields.iter().filter(|f| f.field_number == 1)
        .map(|f| varint(&sub(&WireMessage::decode(f.value.as_bytes().unwrap()).unwrap(), 2), 1).unwrap()).collect();
    assert!(ships.contains(&720_500_001) && ships.contains(&(ship_a as u64)), "{ships:?}");
    assert!(arrival.iter().any(|(n, _)| n == "eve_public.cosmetic.structure.paintwork.api.SetAllInSolarSystemNotice"));

    // ApplySkinToShip (client RPC) -> observers in the system get SetNotice with the SKIN.
    let skin = ctx.sde.skins.values().find(|sk| sk.types.contains(&ship_type)).expect("an SDE SKIN for A's hull").skin_id;
    ctx.db.with_mut_snapshot(|s| s.licensed_skins.entry(90_000_001).or_default().push(skin));
    ctx.dispatch_rpc(&mut a, "shipCosmeticsMgr", None, "ApplySkinToShip", &[PyRep::Integer(ship_type as i64), PyRep::Integer(skin as i64)], &kw, None)
        .await
        .expect("ApplySkinToShip");
    let set = drain(&mut rx_b, "eve_public.cosmetic.ship");
    assert_eq!(set.len(), 1);
    assert_eq!(set[0].0, "eve_public.cosmetic.ship.api.SetNotice");
    let state = sub(&set[0].1, 1);
    assert_eq!(varint(&sub(&state, 1), 1), Some(90_000_001));
    assert_eq!(varint(&sub(&state, 2), 1), Some(ship_a as u64));
    assert_eq!(varint(&sub(&sub(&sub(&state, 3), 1), 1), 1), Some(skin as u64), "first-party SKIN id");

    // GetRequest / GetAllInBubbleRequest read the same authoritative state.
    let get = |ship: u64| { let mut b = Vec::new(); seq(1, ship, &mut b); b };
    let rsp = quasar_rpc(&ctx, &mut b, "eve_public.cosmetic.ship.api.GetRequest", get(ship_a as u64)).await;
    assert_eq!(varint(&sub(&sub(&sub(&sub(&decode(&rsp), 1), 3), 1), 1), 1), Some(skin as u64));
    let rsp = quasar_rpc(&ctx, &mut b, "eve_public.cosmetic.ship.api.GetRequest", get(720_500_001)).await;
    assert_eq!(varint(&sub(&sub(&decode(&rsp), 1), 3), 3), Some(1), "Rifter has no SKIN");
    assert_eq!(quasar_rpc(&ctx, &mut b, "eve_public.cosmetic.ship.api.GetRequest", get(999_999_999)).await.status_code, 404);
    let rsp = quasar_rpc(&ctx, &mut b, "eve_public.cosmetic.ship.api.GetAllInBubbleRequest", Vec::new()).await;
    assert_eq!(decode(&rsp).fields.iter().filter(|f| f.field_number == 1).count(), ships.len());
}

#[tokio::test]
async fn corporation_palette_requires_editor_roles() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/corporation/palette/api/requests_pb2.pyj
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    for (cid, corp) in [(90_000_001, CORP), (90_000_002, CORP), (90_000_003, CORP), (90_000_004, OTHER_CORP)] {
        place_char(&ctx, cid, corp, None);
    }
    let mut director = player(63_001, 90_000_001, CORP, eve_db::corp_roles::ROLE_DIRECTOR);
    let mut member = player(63_002, 90_000_002, CORP, 0);
    let mut brand = player(63_003, 90_000_003, CORP, 1 << 35);
    let mut outsider = player(63_004, 90_000_004, OTHER_CORP, 0);
    let rgb = |r: u64, g: u64, b: u64| { let mut c = Vec::new(); encode_varint_field(1, r, &mut c); encode_varint_field(2, g, &mut c); encode_varint_field(3, b, &mut c); c };
    let set_req = |main: Vec<u8>| { let mut a = Vec::new(); encode_bytes_field(1, &main, &mut a); encode_bytes_field(2, &rgb(40, 50, 60), &mut a); encode_varint_field(5, 1, &mut a); wrap(1, &a) };
    let can = |rsp: eve_proto::quasar::EvePublicResponse| varint(&decode(&rsp), 1).unwrap_or(0);

    assert_eq!(quasar_rpc(&ctx, &mut member, "eve_public.cosmetic.corporation.palette.api.GetOwnRequest", Vec::new()).await.status_code, 404);
    assert_eq!(can(quasar_rpc(&ctx, &mut member, "eve_public.cosmetic.corporation.palette.api.CanEditRequest", Vec::new()).await), 0);
    assert_eq!(can(quasar_rpc(&ctx, &mut director, "eve_public.cosmetic.corporation.palette.api.CanEditRequest", Vec::new()).await), 1);
    assert_eq!(quasar_rpc(&ctx, &mut member, "eve_public.cosmetic.corporation.palette.api.SetRequest", set_req(rgb(10, 20, 30))).await.status_code, 403);
    assert_eq!(quasar_rpc(&ctx, &mut director, "eve_public.cosmetic.corporation.palette.api.SetRequest", set_req(rgb(300, 20, 30))).await.status_code, 400);
    assert_eq!(quasar_rpc(&ctx, &mut director, "eve_public.cosmetic.corporation.palette.api.SetRequest", set_req(rgb(10, 20, 30))).await.status_code, STATUS_OK);

    let mut corp_id = Vec::new();
    seq(1, CORP as u64, &mut corp_id);
    let rsp = quasar_rpc(&ctx, &mut outsider, "eve_public.cosmetic.corporation.palette.api.GetRequest", wrap(1, &corp_id)).await;
    let attrs = sub(&decode(&rsp), 1);
    assert_eq!((varint(&sub(&attrs, 1), 1), varint(&sub(&attrs, 1), 2), varint(&sub(&attrs, 1), 3)), (Some(10), Some(20), Some(30)));
    assert_eq!(varint(&sub(&attrs, 2), 3), Some(60));
    assert_eq!(varint(&attrs, 5), Some(1), "no tertiary color");

    // Brand managers may edit too; GetOwn reports the last modifier.
    assert_eq!(quasar_rpc(&ctx, &mut brand, "eve_public.cosmetic.corporation.palette.api.SetRequest", set_req(rgb(1, 2, 3))).await.status_code, STATUS_OK);
    let own = decode(&quasar_rpc(&ctx, &mut member, "eve_public.cosmetic.corporation.palette.api.GetOwnRequest", Vec::new()).await);
    assert_eq!(varint(&sub(&sub(&own, 1), 1), 1), Some(1));
    assert_eq!(varint(&sub(&own, 2), 1), Some(90_000_003), "last modifier");
    let mut other_id = Vec::new();
    seq(1, OTHER_CORP as u64, &mut other_id);
    assert_eq!(quasar_rpc(&ctx, &mut member, "eve_public.cosmetic.corporation.palette.api.GetRequest", wrap(1, &other_id)).await.status_code, 404);
}

#[tokio::test]
async fn structure_paintwork_follows_license_issue_and_revoke() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/api/api_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/api/admin/admin_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/license/api/api_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/api/admin/admin_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/structure/paintwork/license/api/api_pb2.pyj",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let system = 30_000_142u32;
    let structure = 1_031_000_000_001i64;
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(structure, UpwellStructureRecord {
            structure_id: structure, type_id: 35832, owner_corp_id: CORP as i64, solar_system_id: system,
            name: "Paint Test Astrahus".into(), position: [1.0e6, 0.0, 0.0], state: "Online".into(),
            quantum_core_installed: true, fuel_blocks: 100, shield_hp: 5_000.0, armor_hp: 5_000.0, hull_hp: 5_000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        s.corp_lp_balances.insert((CORP as i64, 1_000_419), 5_000);
    });
    place_char(&ctx, 90_000_002, CORP, Some(system));
    place_char(&ctx, 90_000_003, OTHER_CORP, Some(system));
    place_char(&ctx, 90_000_004, CORP, Some(system));
    let mut gm = build_sess(64_001, 90_000_001, CORP);
    let mut member = player(64_002, 90_000_002, CORP, 0);
    let mut outsider = player(64_003, 90_000_003, OTHER_CORP, 0);
    let mut director = player(64_004, 90_000_004, CORP, eve_db::corp_roles::ROLE_DIRECTOR);
    let mut rx = subscribe(&ctx, &mut outsider).await;

    let issue = |structure: i64, secs: u64| {
        let (mut primary, mut secondary, mut cfg, mut dur, mut st) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        encode_varint_field(1, 11, &mut primary);
        encode_varint_field(1, 22, &mut secondary);
        encode_bytes_field(5, &primary, &mut cfg);
        encode_bytes_field(6, &secondary, &mut cfg);
        encode_varint_field(1, secs, &mut dur);
        encode_varint_field(1, structure as u64, &mut st);
        let mut out = wrap(1, &cfg);
        encode_bytes_field(2, &dur, &mut out);
        encode_bytes_field(3, &st, &mut out);
        out
    };
    let admin = "eve_public.cosmetic.structure.paintwork.license.api.admin";
    assert_eq!(quasar_rpc(&ctx, &mut member, &format!("{admin}.IssueRequest"), issue(structure, 2_592_000)).await.status_code, 403);
    assert_eq!(quasar_rpc(&ctx, &mut gm, &format!("{admin}.IssueRequest"), issue(1_031_999_999_999, 2_592_000)).await.status_code, 404);
    assert_eq!(quasar_rpc(&ctx, &mut gm, &format!("{admin}.IssueRequest"), issue(structure, 0)).await.status_code, 400);
    let rsp = quasar_rpc(&ctx, &mut gm, &format!("{admin}.IssueRequest"), issue(structure, 2_592_000)).await;
    assert_eq!(rsp.status_code, STATUS_OK, "{}", rsp.status_message);
    let lic = sub(&decode(&rsp), 1);
    let uuid = bytes(&sub(&lic, 1), 1);
    assert_eq!(varint(&sub(&sub(&lic, 2), 1), 1), Some(CORP as u64), "structure owner holds the license");
    assert_eq!(varint(&sub(&lic, 3), 1), Some(structure as u64));

    // Everyone in the system sees the new paintwork.
    let set = drain(&mut rx, "eve_public.cosmetic.structure");
    assert_eq!(set.len(), 1);
    assert_eq!(set[0].0, "eve_public.cosmetic.structure.paintwork.api.SetNotice");
    assert_eq!(varint(&sub(&set[0].1, 1), 1), Some(structure as u64));
    assert_eq!(varint(&sub(&sub(&set[0].1, 2), 5), 1), Some(11), "primary paint");

    let st_req = |id: i64| { let mut b = Vec::new(); seq(1, id as u64, &mut b); b };
    let got = decode(&quasar_rpc(&ctx, &mut outsider, "eve_public.cosmetic.structure.paintwork.api.GetRequest", st_req(structure)).await);
    assert_eq!(bytes(&sub(&got, 1), 1), uuid);
    assert_eq!(varint(&sub(&sub(&got, 2), 6), 1), Some(22), "secondary paint");
    assert_eq!(quasar_rpc(&ctx, &mut outsider, "eve_public.cosmetic.structure.paintwork.api.GetRequest", st_req(structure + 1)).await.status_code, 404);
    let all = decode(&quasar_rpc(&ctx, &mut outsider, "eve_public.cosmetic.structure.paintwork.api.GetAllInSolarSystemRequest", Vec::new()).await);
    assert_eq!(all.fields.iter().filter(|f| f.field_number == 1).count(), 1);

    // Corporation-scoped license views.
    let lic_api = "eve_public.cosmetic.structure.paintwork.license.api";
    let cat = decode(&quasar_rpc(&ctx, &mut member, &format!("{lic_api}.GetCatalogueRequest"), Vec::new()).await);
    assert!(!cat.fields.is_empty(), "paintwork licence catalogue is non-empty");
    let owned = decode(&quasar_rpc(&ctx, &mut member, &format!("{lic_api}.GetAllOwnedByCorporationRequest"), Vec::new()).await);
    assert_eq!(bytes(&sub(&sub(&owned, 1), 1), 1), uuid);
    let foreign = decode(&quasar_rpc(&ctx, &mut outsider, &format!("{lic_api}.GetAllOwnedByCorporationRequest"), Vec::new()).await);
    assert!(foreign.fields.is_empty(), "other corporations own no licenses");
    assert_eq!(quasar_rpc(&ctx, &mut outsider, &format!("{lic_api}.GetRequest"), wrap(1, &wrap(1, &uuid))).await.status_code, 404);
    let lic_get = decode(&quasar_rpc(&ctx, &mut member, &format!("{lic_api}.GetRequest"), wrap(1, &wrap(1, &uuid))).await);
    assert_eq!(varint(&sub(&sub(&lic_get, 2), 1), 1), Some(CORP as u64));
    assert_eq!(quasar_rpc(&ctx, &mut member, &format!("{lic_api}.RevokeRequest"), wrap(1, &wrap(1, &uuid))).await.status_code, 403);
    assert_eq!(quasar_rpc(&ctx, &mut member, &format!("{lic_api}.IssueRequest"), issue(structure, 2_592_000)).await.status_code, 403);

    // Non-catalogue duration returns 400 Bad Request.
    assert_eq!(quasar_rpc(&ctx, &mut director, &format!("{lic_api}.IssueRequest"), issue(structure, 12_345)).await.status_code, 400);

    // Insufficient EverMarks returns 403 Forbidden.
    ctx.db.with_mut_snapshot(|s| { s.corp_lp_balances.insert((CORP as i64, 1_000_419), 0); });
    assert_eq!(quasar_rpc(&ctx, &mut director, &format!("{lic_api}.IssueRequest"), issue(structure, 2_592_000)).await.status_code, 403);
    ctx.db.with_mut_snapshot(|s| { s.corp_lp_balances.insert((CORP as i64, 1_000_419), 5_000); });

    // Corporation director issue: public gateway IssueRequest works, re-issues license, and deducts catalogue EverMarks.
    let dir_issue = quasar_rpc(&ctx, &mut director, &format!("{lic_api}.IssueRequest"), issue(structure, 2_592_000)).await;
    assert_eq!(dir_issue.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|s| s.corp_lp_balances.get(&(CORP as i64, 1_000_419)).copied()), Some(4_000));
    let new_lic = sub(&decode(&dir_issue), 1);
    let new_uuid = bytes(&sub(&new_lic, 1), 1);

    // Corporation director revoke: the structure reverts to default paintwork.
    assert_eq!(quasar_rpc(&ctx, &mut director, &format!("{lic_api}.RevokeRequest"), wrap(1, &wrap(1, &new_uuid))).await.status_code, STATUS_OK);
    let cleared = drain(&mut rx, "eve_public.cosmetic.structure");
    assert_eq!(cleared.len(), 2, "SetNotice emitted on director issue and revoke");
    assert_eq!(quasar_rpc(&ctx, &mut outsider, "eve_public.cosmetic.structure.paintwork.api.GetRequest", st_req(structure)).await.status_code, 404);
}
