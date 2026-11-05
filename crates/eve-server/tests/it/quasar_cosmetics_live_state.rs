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
    // parity: feat:quasar.assetholding, quasar:eveProto/generated/eve_public/assetholding/entitlement/api/notices_pb2.pyj
    eve_testkit::attest(&[
        "feat:quasar.assetholding",
        "quasar:eveProto/generated/eve_public/assetholding/entitlement/api/notices_pb2.pyj",
    ]);
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
    assert_eq!(quasar_rpc(&ctx, &mut holder, "eve_public.assetholding.asset.api.GetRequest", wrap(1, &wrap(1, &asset))).await.status_code, STATUS_OK);
    let unred = decode(&quasar_rpc(&ctx, &mut holder, "eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest", Vec::new()).await);
    assert_eq!(unred.fields.iter().filter(|f| f.field_number == 1).count(), 1);

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

    // Foreign holder query on GetUnredeemedByHolderAndGroupNameRequest must be rejected with 403.
    let mut fh_inner = Vec::new();
    seq(1, 90_000_002, &mut fh_inner);
    let foreign_holder_req = wrap(1, &fh_inner);
    assert_eq!(
        quasar_rpc(&ctx, &mut other, "eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest", foreign_holder_req).await.status_code,
        403
    );

    // 2. Cosmetic character title holding: SpawnRequest(field 13 = character_title, finite = 2) -> DirectDelivery -> Redeem.
    let mut title_uuid = [0u8; 16];
    title_uuid[8..16].copy_from_slice(&777u64.to_be_bytes());
    let mut title_sp = wrap(1, &wrap(1, &title_uuid));
    encode_varint_field(3, 2, &mut title_sp); // finite = 2
    let mut spawn_title = Vec::new();
    encode_string_field(2, "Cosmetic Title 777", &mut spawn_title);
    encode_bytes_field(13, &title_sp, &mut spawn_title);
    let t_rsp = quasar_rpc(&ctx, &mut gm, "eve.assetholding.asset.api.SpawnRequest", spawn_title).await;
    assert_eq!(t_rsp.status_code, STATUS_OK);
    let t_asset = bytes(&sub(&decode(&t_rsp), 1), 1);
    let t_get = decode(&quasar_rpc(&ctx, &mut holder, "eve.assetholding.asset.api.GetRequest", wrap(1, &wrap(1, &t_asset))).await);
    assert_eq!(bytes(&sub(&sub(&sub(&sub(&t_get, 1), 4), 12), 1), 1), title_uuid.to_vec());
    assert_eq!(varint(&sub(&sub(&sub(&t_get, 1), 5), 1), 2), Some(2));

    let t_group = *b"TITLETITLETITLE1";
    let t_id = ident(90_000_002, &t_asset, &t_group);
    let mut t_entry = wrap(2, &t_id);
    encode_varint_field(10, 1, &mut t_entry);
    assert_eq!(quasar_rpc(&ctx, &mut gm, "eve.assetholding.entitlement.api.DirectDeliveryRequest", wrap(2, &t_entry)).await.status_code, STATUS_OK);
    let _ = drain(&mut rx, "eve_public.assetholding");
    assert_eq!(quasar_rpc(&ctx, &mut holder, "eve_public.assetholding.entitlement.api.RedeemRequest", wrap(1, &t_id)).await.status_code, STATUS_OK);
    let t_notices = drain(&mut rx, "eve_public.");
    let cap_n = t_notices.iter().find(|(n, _)| n == "eve_public.assetholding.asset.api.FiniteCapacityClaimedNotice").expect("FiniteCapacityClaimedNotice");
    assert_eq!(bytes(&sub(&cap_n.1, 1), 1), t_asset);
    assert_eq!((varint(&cap_n.1, 2), varint(&cap_n.1, 3)), (Some(2), Some(1)));
    let title_n = t_notices.iter().find(|(n, _)| n == "eve_public.cosmetic.character.title.api.GrantedNotice").expect("title GrantedNotice");
    assert_eq!(bytes(&sub(&title_n.1, 1), 1), title_uuid.to_vec());
    let owned_titles = decode(&quasar_rpc(&ctx, &mut holder, "eve_public.cosmetic.character.title.api.GetOwnedRequest", Vec::new()).await);
    assert_eq!(bytes(&sub(&owned_titles, 1), 1), title_uuid.to_vec());

    // 3. Ship SKIN license holding: HoldRequest requires admin (403 for non-admin), then DirectDelivery -> Redeem.
    let mut skin_id_msg = Vec::new();
    encode_string_field(1, "skin_hex_assethold_01", &mut skin_id_msg);
    let mut skin_hp = wrap(1, &wrap(1, &skin_id_msg));
    encode_varint_field(2, 3, &mut skin_hp);
    assert_eq!(quasar_rpc(&ctx, &mut holder, "eve.assetholding.asset.api.HoldRequest", wrap(11, &skin_hp)).await.status_code, 403);
    let h_rsp = quasar_rpc(&ctx, &mut gm, "eve.assetholding.asset.api.HoldRequest", wrap(11, &skin_hp)).await;
    assert_eq!(h_rsp.status_code, STATUS_OK, "{}", h_rsp.status_message);
    let s_asset = bytes(&sub(&decode(&h_rsp), 1), 1);
    let s_get = decode(&quasar_rpc(&ctx, &mut holder, "eve.assetholding.asset.api.GetRequest", wrap(1, &wrap(1, &s_asset))).await);
    assert_eq!(
        String::from_utf8(bytes(&sub(&sub(&sub(&sub(&s_get, 1), 4), 10), 1), 1)).unwrap(),
        "skin_hex_assethold_01"
    );
    let s_group = *b"SKINSKINSKINSKIN";
    let s_id = ident(90_000_002, &s_asset, &s_group);
    let mut s_entry = wrap(2, &s_id);
    encode_varint_field(10, 2, &mut s_entry);
    assert_eq!(quasar_rpc(&ctx, &mut gm, "eve.assetholding.entitlement.api.DirectDeliveryRequest", wrap(2, &s_entry)).await.status_code, STATUS_OK);
    let _ = drain(&mut rx, "eve_public.assetholding");
    let int_ent_get = decode(&quasar_rpc(&ctx, &mut holder, "eve.assetholding.entitlement.api.GetRequest", wrap(1, &s_id)).await);
    assert_eq!(varint(&sub(&sub(&int_ent_get, 1), 1), 1), Some(2));
    assert_eq!(quasar_rpc(&ctx, &mut gm, "eve.assetholding.entitlement.api.admin.RedeliverStaleEntitlementsRequest", Vec::new()).await.status_code, STATUS_OK);
    assert!(!drain(&mut rx, "eve_public.assetholding").is_empty());
    assert_eq!(quasar_rpc(&ctx, &mut gm, "eve_public.assetholding.entitlement.api.admin.AutoRedeemRequest", wrap(1, &s_id)).await.status_code, STATUS_OK);
    let s_notices = drain(&mut rx, "eve_public.");
    let s_cap = s_notices.iter().find(|(n, _)| n == "eve_public.assetholding.asset.api.FiniteCapacityClaimedNotice").expect("skin FiniteCapacityClaimedNotice");
    assert_eq!((varint(&s_cap.1, 2), varint(&s_cap.1, 3)), (Some(3), Some(1)));
    let lic_added = s_notices.iter().find(|(n, _)| n == "eve_public.cosmetic.ship.skin.thirdparty.license.api.AddedNotice").expect("skin AddedNotice");
    assert_eq!(varint(&sub(&lic_added.1, 2), 2), Some(2), "2 unactivated licenses granted");
    let mut lic_get_req = Vec::new();
    seq(1, 90_000_002, &mut lic_get_req);
    encode_bytes_field(2, &skin_id_msg, &mut lic_get_req);
    let lic_get = decode(&quasar_rpc(&ctx, &mut holder, "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest", wrap(1, &lic_get_req)).await);
    assert_eq!(varint(&sub(&lic_get, 1), 2), Some(2), "GetRequest returns 2 unactivated licenses");
    assert_eq!(quasar_rpc(&ctx, &mut gm, "eve_public.assetholding.entitlement.api.admin.ExpireRequest", wrap(1, &s_id)).await.status_code, STATUS_OK);
    assert_eq!(quasar_rpc(&ctx, &mut holder, "eve.assetholding.entitlement.api.GetRequest", wrap(1, &s_id)).await.status_code, 404);
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
    assert_eq!(quasar_rpc(&ctx, &mut a_q, "eve_public.cosmetic.ship.logo.DisplayRequest", logo(ship_a, 0, Some(2))).await.status_code, 403, "corporation logo requires hull emblem entitlement");
    let mut gm_ent = build_sess(62_099, 90_000_001, CORP);
    gm_ent.user_id = Some(62_099);
    let logo_ent = eve_server::services::quasar_entitlement_membership::encode_logo_identifier(90_000_001, u64::from(ship_type));
    assert_eq!(quasar_rpc(&ctx, &mut gm_ent, "eve_public.entitlement.character.ship.admin.corplogo.GrantRequest", wrap(1, &logo_ent)).await.status_code, STATUS_OK);
    let _ = drain(&mut rx_a, "eve_public.entitlement");
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

#[allow(dead_code, unused_imports)]
pub(crate) mod quasar_cosmetics_seasons {
    use super::*;

//  Part 1 of Quasar, Cosmetics & Seasons Closure Test Suite.
//  Covers:
//  - Season Manager RPCs: `get_season_data_for_character`, `get_season_end_time`, `select_season`
//  - Cosmetics Features: `apparel`, `cosmetics-broadcast`, `ship-emblems`, `skinr-designer`, `skinr-market`

use crate::common;
use crate::quasar_goal_tale_notices::{bytes, seq, sub};
use crate::quasar_progression_complete::{build_sess, quasar_rpc};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicNotice, WireMessage,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_FORBIDDEN, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::quasar_cosmetics_market::PARAGON_CORP_ID;
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const CORP_1: u32 = 98_000_001;
const CORP_2: u32 = 98_000_002;

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<EvePublicNotice> {
    let mut out = Vec::new();
    for _ in 0..512 {
        match rx.try_recv() {
            Ok(n) => out.push(n),
            Err(_) => break,
        }
    }
    out
}

async fn subscribe_notices(ctx: &ServerContext, sess: &mut SessionState) -> Receiver<EvePublicNotice> {
    let empty_kw = HashMap::new();
    ctx.dispatch_rpc(sess, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
        .await
        .expect("Subscribe");
    let mut rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("stream receiver");
    for _ in 0..256 {
        if rx.try_recv().is_err() {
            break;
        }
    }
    rx
}

async fn select_character(ctx: &ServerContext, sid: u64, char_id: i64, user: &str, pass: &str) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    let empty_kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &empty_kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

fn decode_response(rsp: &eve_proto::quasar::EvePublicResponse) -> WireMessage {
    WireMessage::decode(&rsp.payload.as_ref().expect("payload").value).expect("decode wire message")
}

#[tokio::test]
async fn test_seasons_manager_endpoints_and_selection() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = select_character(&ctx, 65_001, PILOT_1, "demo", "demo-secret").await;
    let empty_kw = HashMap::new();

    // 1. get_season_end_time returns active season end filetime ticks
    let end_time_rep = ctx
        .dispatch_rpc(&mut sess, "seasonManager", None, "get_season_end_time", &[], &empty_kw, None)
        .await
        .expect("get_season_end_time");
    let end_time_ft = match end_time_rep {
        PyRep::Integer(t) => t,
        other => panic!("expected Integer end filetime ticks, got {other:?}"),
    };
    let now_ft = ctx.now_blueos_filetime_ticks();
    assert!(end_time_ft > now_ft, "season end filetime must be in the future");
    assert_eq!(end_time_ft, now_ft + 30 * 86_400 * 10_000_000);

    // 2. get_season_data_for_character returns active season configuration and challenge definitions
    let season_data_rep = ctx
        .dispatch_rpc(&mut sess, "seasonManager", None, "get_season_data_for_character", &[], &empty_kw, None)
        .await
        .expect("get_season_data_for_character");
    match season_data_rep {
        PyRep::List(ref list) => {
            assert!(!list.is_empty(), "season catalog must be non-empty");
        }
        PyRep::KeyVal(ref kv) => {
            assert!(kv.get("seasonID").is_some() || kv.get("season_id").is_some());
        }
        other => panic!("expected List or KeyVal from get_season_data_for_character, got {other:?}"),
    }

    // 3. select_season with valid season ID (2) succeeds and emits OnSeasonSelected notice
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    let select_rep = ctx
        .dispatch_rpc(&mut sess, "seasonManager", None, "select_season", &[PyRep::Integer(2)], &empty_kw, None)
        .await
        .expect("select_season 2");
    assert_eq!(select_rep, PyRep::Bool(true));
    let notifs = ctx.drain_notifications_for_char(PILOT_1);
    assert!(
        notifs.iter().any(|n| n.event == "OnSeasonSelected"),
        "expected OnSeasonSelected notification"
    );

    // After select_season, get_season_data_for_character returns KeyVal
    let selected_data = ctx
        .dispatch_rpc(&mut sess, "seasonManager", None, "get_season_data_for_character", &[], &empty_kw, None)
        .await
        .expect("get_season_data_for_character selected");
    let PyRep::KeyVal(kv) = selected_data else {
        panic!("expected KeyVal after select_season, got {selected_data:?}");
    };
    assert_eq!(kv.get("season_id"), Some(&PyRep::Integer(2)));

    // 4. select_season with invalid/unknown season ID fails with error code
    let bad_res = ctx
        .dispatch_rpc(&mut sess, "seasonManager", None, "select_season", &[PyRep::Integer(999_999)], &empty_kw, None)
        .await;
    let err = bad_res.expect_err("unknown season");
    assert_eq!(err.code, "InvalidSeasonId"); // rpc-lint: allow error code assertion
}

#[tokio::test]
async fn test_cosmetics_apparel_broadcast_and_emblems() {
    // parity: feat:cosmetics.cosmetics-broadcast
    // parity: feat:cosmetics.ship-emblems
    eve_testkit::attest(&[
        "feat:cosmetics.cosmetics-broadcast",
        "feat:cosmetics.ship-emblems",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let empty_kw = HashMap::new();

    // 1. feat:cosmetics.apparel — wear clothing item via paperdoll update and persist
    let apparel_item_id = 850_101i64;
    let apparel_type_id = 3966u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            apparel_item_id,
            InventoryItemRecord {
                item_id: apparel_item_id,
                type_id: apparel_type_id,
                owner_id: PILOT_1,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess1 = select_character(&ctx, 65_002, PILOT_1, "demo", "demo-secret").await;
    let update_kv = PyRep::keyval([
        ("hair", PyRep::Integer(5)),
        ("apparel", PyRep::List(vec![PyRep::Integer(apparel_item_id)])),
    ]);
    let wear_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "paperDollServer",
            None,
            "UpdateExistingCharacterLimited",
            &[PyRep::Integer(PILOT_1), update_kv],
            &empty_kw,
            None,
        )
        .await
        .expect("UpdateExistingCharacterLimited wear");
    let PyRep::KeyVal(wear_kv) = wear_rep else { panic!("expected KeyVal") };
    assert_eq!(wear_kv.get("wornApparel"), Some(&PyRep::List(vec![PyRep::Integer(apparel_type_id as i64)])));
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&apparel_item_id).expect("apparel item in snapshot");
        assert_eq!(it.flag, InventoryFlag::Wardrobe, "worn apparel flag must persist as Wardrobe (3)");
        assert_eq!(it.flag as u16, 3);
    });

    // Unwear apparel: item flag resets to Hangar
    let unwear_kv = PyRep::keyval([("apparel", PyRep::List(vec![]))]);
    let _ = ctx
        .dispatch_rpc(
            &mut sess1,
            "paperDollServer",
            None,
            "UpdateExistingCharacterLimited",
            &[PyRep::Integer(PILOT_1), unwear_kv],
            &empty_kw,
            None,
        )
        .await
        .expect("UpdateExistingCharacterLimited unwear");
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&apparel_item_id).expect("apparel item in snapshot");
        assert_eq!(it.flag, InventoryFlag::Hangar, "unworn apparel flag must reset to Hangar");
    });

    // 2. feat:cosmetics.ship-emblems — logo entitlement check and GetEnabledCosmetics
    let (system_id, ship_id, ship_type) = ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&PILOT_1];
        (ch.solar_system_id, ch.active_ship_id, snap.items[&ch.active_ship_id].type_id)
    });
    sess1.solar_system_id = Some(system_id);

    let logo_req = |ship: i64, slot: i32, kind: u32| {
        let mut id = Vec::new();
        seq(1, ship as u64, &mut id);
        encode_varint_field(2, slot as u64, &mut id);
        let mut out = Vec::new();
        encode_bytes_field(1, &id, &mut out);
        let mut k_msg = Vec::new();
        encode_bytes_field(kind, &[], &mut k_msg);
        encode_bytes_field(2, &k_msg, &mut out);
        out
    };

    // Before grant: DisplayRequest is denied with 403
    let denied = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.logo.DisplayRequest", logo_req(ship_id, 0, 2)).await;
    assert_eq!(denied.status_code, STATUS_FORBIDDEN);

    // Grant corp logo entitlement, then DisplayRequest succeeds
    let mut gm_sess = build_sess(65_004, PILOT_1 as u32, CORP_1);
    gm_sess.user_id = Some(65_004);
    let logo_ent = eve_server::services::quasar_entitlement_membership::encode_logo_identifier(PILOT_1 as u64, u64::from(ship_type));
    let mut grant_bytes = Vec::new();
    encode_bytes_field(1, &logo_ent, &mut grant_bytes);
    let grant_rsp = quasar_rpc(&ctx, &mut gm_sess, "eve_public.entitlement.character.ship.admin.corplogo.GrantRequest", grant_bytes).await;
    assert_eq!(grant_rsp.status_code, STATUS_OK);

    let display_rsp = quasar_rpc(&ctx, &mut sess1, "eve_public.cosmetic.ship.logo.DisplayRequest", logo_req(ship_id, 0, 2)).await;
    assert_eq!(display_rsp.status_code, STATUS_OK);

    // Verify GetEnabledCosmetics returns corporation emblem enabled in slot 0
    let enabled_rep = ctx
        .dispatch_rpc(&mut sess1, "shipCosmeticsMgr", None, "GetEnabledCosmetics", &[PyRep::Integer(ship_id)], &empty_kw, None)
        .await
        .expect("GetEnabledCosmetics");
    let PyRep::Dict(enabled_dict) = enabled_rep else { panic!("expected Dict") };
    assert_eq!(enabled_dict, vec![(PyRep::Integer(0), PyRep::Integer(1))]);

    // 3. feat:cosmetics.cosmetics-broadcast:
    // ApplySkinToShip changes active ship's skin; observers receive OnShipCosmeticChanged and OnShipCosmeticsChanged
    let _sess2 = select_character(&ctx, 65_005, PILOT_2, "other", "other-secret").await;
    ctx.db.with_mut_snapshot(|snap| {
        let ch2 = snap.characters.get_mut(&PILOT_2).unwrap();
        ch2.solar_system_id = system_id;
        ch2.station_id = None;
        ch2.structure_id = None;
        let ch1 = snap.characters.get_mut(&PILOT_1).unwrap();
        ch1.solar_system_id = system_id;
        ch1.station_id = None;
        ch1.structure_id = None;
    });

    let skin_id = ctx.sde.skins.values().find(|sk| sk.types.contains(&ship_type)).expect("skin for ship").skin_id;
    ctx.db.with_mut_snapshot(|snap| {
        snap.licensed_skins.entry(PILOT_1).or_default().push(skin_id);
    });

    let _ = ctx.drain_notifications_for_char(PILOT_1);
    let _ = ctx.drain_notifications_for_char(PILOT_2);

    let apply_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "shipCosmeticsMgr",
            None,
            "ApplySkinToShip",
            &[PyRep::Integer(ship_type as i64), PyRep::Integer(skin_id as i64)],
            &empty_kw,
            None,
        )
        .await
        .expect("ApplySkinToShip");
    let PyRep::KeyVal(apply_kv) = apply_rep else { panic!("expected KeyVal") };
    assert_eq!(apply_kv.get("skinID"), Some(&PyRep::Integer(skin_id as i64)));

    // Verify notifications sent to observer (PILOT_2): skin broadcast emits OnShipCosmeticsChanged and OnSlimItemChange
    let p2_notifs = ctx.drain_notifications_for_char(PILOT_2);
    assert!(
        p2_notifs.iter().any(|n| n.event == "OnShipCosmeticsChanged"),
        "expected OnShipCosmeticsChanged notification to observer"
    );
    assert!(
        p2_notifs.iter().any(|n| n.event == "OnSlimItemChange"),
        "expected OnSlimItemChange notification to observer"
    );
}

#[tokio::test]
async fn test_cosmetics_skinr_designer_and_paragon_market() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let seller_cid = 90_000_821u32;
    let buyer_cid = 90_000_822u32;
    let ship_id = 890_001i64;
    let rifter_type = 587u32;

    ctx.db.with_mut_snapshot(|s| {
        let mut ch_s = s.characters.get(&PILOT_1).cloned().expect("template char");
        ch_s.character_id = seller_cid as i64;
        ch_s.corporation_id = CORP_1 as i64;
        ch_s.active_ship_id = ship_id;
        ch_s.balance = IskCents::from_isk(5_000_000);
        ch_s.plex_balance = 500;
        ch_s.station_id = Some(60_003_760);
        s.characters.insert(seller_cid as i64, ch_s);

        let mut ch_b = s.characters.get(&PILOT_1).cloned().expect("template char");
        ch_b.character_id = buyer_cid as i64;
        ch_b.corporation_id = CORP_2 as i64;
        ch_b.balance = IskCents::from_isk(10_000_000);
        ch_b.station_id = Some(60_003_760);
        ch_b.loyalty_points.insert(PARAGON_CORP_ID as i64, 50_000);
        s.characters.insert(buyer_cid as i64, ch_b);

        let sk_map = s.skills.entry(seller_cid as i64).or_default();
        sk_map.insert(81044, TrainedSkillRecord { type_id: 81044, level: 4, skill_points: 45_255.0 });
        sk_map.insert(81040, TrainedSkillRecord { type_id: 81040, level: 5, skill_points: 256_000.0 });

        s.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: rifter_type,
                owner_id: seller_cid as i64,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut seller_sess = build_sess(65_006, seller_cid, CORP_1);
    let mut buyer_sess = build_sess(65_007, buyer_cid, CORP_2);
    buyer_sess.role = 0;
    let mut rx_seller = subscribe_notices(&ctx, &mut seller_sess).await;

    // 1. feat:cosmetics.skinr-designer:
    // Ship SKINR: designs persisted; sequencing jobs consume components and produce licenses
    let skinr_cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
    let coat_comp = skinr_cat
        .components
        .values()
        .find(|c| !c.is_pattern() && !c.disallowed_slots.contains(&1))
        .expect("coating component");
    let coat_id = coat_comp.component_id;

    // Grant 10 finite coating component licenses to seller (via GM admin session)
    let mut comp_id_outer = Vec::new();
    let mut comp_id_inner = Vec::new();
    encode_varint_field(1, coat_id as u64, &mut comp_id_inner);
    encode_bytes_field(1, &comp_id_inner, &mut comp_id_outer);
    let mut grant_req = Vec::new();
    encode_bytes_field(1, &comp_id_outer, &mut grant_req);
    encode_varint_field(2, 10, &mut grant_req);
    let grant_rsp = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
        grant_req,
    )
    .await;
    assert_eq!(grant_rsp.status_code, STATUS_OK);
    let _ = drain_notices(&mut rx_seller);
    // Strip GM role from seller: sequencing and market operations run under standard non-GM player permissions
    seller_sess.role = 0;

    // Build sequencing job layout
    let mut slot_msg = Vec::new();
    let mut s_idx = Vec::new();
    encode_varint_field(1, 1, &mut s_idx);
    encode_bytes_field(1, &s_idx, &mut slot_msg);
    let mut coat_cfg = Vec::new();
    let mut c_idx = Vec::new();
    encode_varint_field(1, coat_id as u64, &mut c_idx);
    encode_bytes_field(1, &c_idx, &mut coat_cfg);
    let mut cfg_outer = Vec::new();
    encode_bytes_field(1, &coat_cfg, &mut cfg_outer);
    encode_bytes_field(2, &cfg_outer, &mut slot_msg);

    let mut layout = Vec::new();
    encode_bytes_field(1, &slot_msg, &mut layout);
    let mut r_tid = Vec::new();
    encode_varint_field(1, rifter_type as u64, &mut r_tid);
    encode_bytes_field(2, &r_tid, &mut layout);
    encode_varint_field(3, 1, &mut layout);

    let mut init_req = Vec::new();
    encode_string_field(1, "Closure Phoenix Rifter", &mut init_req);
    encode_string_field(2, "Fire and Iron", &mut init_req);
    encode_bytes_field(3, &layout, &mut init_req);
    encode_varint_field(5, 2, &mut init_req); // 2 licenses

    let init_rsp = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        init_req,
    )
    .await;
    assert_eq!(init_rsp.status_code, STATUS_OK, "{}", init_rsp.status_message);
    let init_wm = decode_response(&init_rsp);
    let job_id_sub = sub(&init_wm, 1);
    let job_uuid = bytes(&job_id_sub, 1);
    let skin_id_sub = sub(&sub(&init_wm, 2), 1);
    let skin_hex = String::from_utf8(bytes(&skin_id_sub, 1)).expect("skin hex");
    assert!(!skin_hex.is_empty());

    // Verify sequencing job consumed 2 finite component licenses (10 -> 8)
    let comp_cap = ctx.db.with_snapshot(|s| {
        let st: eve_server::services::quasar_cosmetics_skinr::SkinrCosmeticsData = s
            .quasar_state_blobs
            .get("quasar_cosmetics_skinr")
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        let key = eve_server::services::quasar_cosmetics_skinr::SkinrCosmeticsState::comp_key(seller_cid, false, coat_id);
        st.finite_licenses.get(&key).map(|l| l.capacity).unwrap_or(0)
    });
    assert_eq!(comp_cap, 8, "sequencing job must consume 2 finite component licenses");

    // Early complete job -> produces 2 licenses and persists custom design
    let mut job_uuid_inner = Vec::new();
    encode_bytes_field(1, &job_uuid, &mut job_uuid_inner);
    let mut job_outer = Vec::new();
    encode_bytes_field(1, &job_uuid_inner, &mut job_outer);
    let comp_rsp = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest",
        job_outer,
    )
    .await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);

    // Verify sequencing job produced 2 licenses for the seller
    let seller_lic = ctx.db.with_snapshot(|s| {
        let st: eve_server::services::quasar_cosmetics_skinr::SkinrCosmeticsData = s
            .quasar_state_blobs
            .get("quasar_cosmetics_skinr")
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        st.skin_licenses.get(&format!("{seller_cid}:{skin_hex}")).map(|l| l.unactivated).unwrap_or(0)
    });
    assert_eq!(seller_lic, 2, "sequencing job completion must produce 2 unactivated licenses");

    // Verify thirdparty.api.GetRequest returns custom design details
    let mut get_skin_req = Vec::new();
    let mut hex_bytes = Vec::new();
    encode_string_field(1, &skin_hex, &mut hex_bytes);
    encode_bytes_field(1, &hex_bytes, &mut get_skin_req);
    let get_skin_rsp = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest",
        get_skin_req,
    )
    .await;
    assert_eq!(get_skin_rsp.status_code, STATUS_OK);
    let skin_wm = decode_response(&get_skin_rsp);
    let skin_name = String::from_utf8(bytes(&sub(&skin_wm, 1), 1)).unwrap_or_default();
    assert_eq!(skin_name, "Closure Phoenix Rifter");

    // 2. feat:cosmetics.skinr-market:
    // List custom skin license on Paragon market and purchase
    let mut price_isk = Vec::new();
    encode_varint_field(1, 500_000 * 2, &mut price_isk);
    let mut skin_id = Vec::new();
    encode_string_field(1, &skin_hex, &mut skin_id);
    let mut lic_id = Vec::new();
    seq(1, seller_cid as u64, &mut lic_id);
    encode_bytes_field(2, &skin_id, &mut lic_id);
    let mut dur = Vec::new();
    encode_varint_field(1, 86_400, &mut dur);
    let mut target = Vec::new();
    encode_varint_field(1, 1, &mut target);
    let mut init_list_req = Vec::new();
    encode_bytes_field(1, &price_isk, &mut init_list_req);
    encode_bytes_field(3, &lic_id, &mut init_list_req);
    encode_bytes_field(4, &dur, &mut init_list_req);
    encode_varint_field(5, 1, &mut init_list_req);
    encode_bytes_field(6, &target, &mut init_list_req);

    let seller_pre_isk = ctx.db.with_snapshot(|s| {
        s.characters.get(&(seller_cid as i64)).unwrap().balance.0
    });

    let list_rsp = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        init_list_req,
    )
    .await;
    assert_eq!(list_rsp.status_code, STATUS_OK, "{}", list_rsp.status_message);

    // Verify seller paid 100,000 ISK broker fee and escrowed 1 license
    let (seller_unact, seller_escrow, seller_mid_isk) = ctx.db.with_snapshot(|s| {
        let st: eve_server::services::quasar_cosmetics_skinr::SkinrCosmeticsData = s
            .quasar_state_blobs
            .get("quasar_cosmetics_skinr")
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        let lic = st.skin_licenses.get(&format!("{seller_cid}:{skin_hex}"));
        let ch = s.characters.get(&(seller_cid as i64)).unwrap();
        (lic.map(|l| l.unactivated).unwrap_or(0), lic.map(|l| l.escrowed).unwrap_or(0), ch.balance.0)
    });
    assert_eq!(seller_unact, 1);
    assert_eq!(seller_escrow, 1);
    assert_eq!(seller_mid_isk, seller_pre_isk - 10_000_000, "seller must pay 100,000 ISK broker fee on listing");

    // Retrieve active listing UUID
    let rsp_owned = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(rsp_owned.status_code, STATUS_OK);
    let wm_owned = decode_response(&rsp_owned);
    let entry = sub(&wm_owned, 1);
    let listing_uuid = bytes(&sub(&entry, 1), 1);
    let listed_qty = eve_proto::quasar::WireMessage::decode(&rsp_owned.payload.as_ref().unwrap().value)
        .ok()
        .map(|w| w.fields.len())
        .unwrap_or(0);
    assert_eq!(listed_qty, 1);

    let (buyer_pre_isk, buyer_pre_lp) = ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&(buyer_cid as i64)).unwrap();
        (ch.balance.0, ch.loyalty_points.get(&(PARAGON_CORP_ID as i64)).copied().unwrap_or(0))
    });

    // Buyer purchases the listing
    let mut buy_req = Vec::new();
    let mut uuid_inner = Vec::new();
    encode_bytes_field(1, &listing_uuid, &mut uuid_inner);
    encode_bytes_field(1, &uuid_inner, &mut buy_req);
    encode_varint_field(2, 1, &mut buy_req);
    encode_string_field(3, eve_server::services::quasar_cosmetics_market::BUYER_FEES_VERSION, &mut buy_req);

    let buy_rsp = quasar_rpc(
        &ctx,
        &mut buyer_sess,
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
        buy_req,
    )
    .await;
    assert_eq!(buy_rsp.status_code, STATUS_OK, "{}", buy_rsp.status_message);

    // Verify ISK and fees on purchase:
    // - Buyer pays 500,000 ISK price + 1,000 EverMarks LP fee
    // - Seller receives 400,000 ISK net proceeds (500,000 minus 20% sales tax = 100,000 ISK fee)
    let (buyer_post_isk, buyer_post_lp, seller_post_isk) = ctx.db.with_snapshot(|s| {
        let b = s.characters.get(&(buyer_cid as i64)).unwrap();
        let sel = s.characters.get(&(seller_cid as i64)).unwrap();
        (b.balance.0, b.loyalty_points.get(&(PARAGON_CORP_ID as i64)).copied().unwrap_or(0), sel.balance.0)
    });
    assert_eq!(buyer_post_isk, buyer_pre_isk - 50_000_000, "buyer balance must be debited 500,000 ISK");
    assert_eq!(buyer_post_lp, buyer_pre_lp - 4_000, "buyer LP must be debited 4,000 EverMarks fee (tier 4 x 1,000)");
    assert_eq!(seller_post_isk, seller_mid_isk + 40_000_000, "seller balance must be credited 400,000 ISK net");

    // Verify buyer received license
    let mut lic_ident = Vec::new();
    seq(1, buyer_cid as u64, &mut lic_ident);
    let mut skin_ident = Vec::new();
    encode_string_field(1, &skin_hex, &mut skin_ident);
    encode_bytes_field(2, &skin_ident, &mut lic_ident);
    let mut get_lic_req = Vec::new();
    encode_bytes_field(1, &lic_ident, &mut get_lic_req);
    let get_lic_rsp = quasar_rpc(
        &ctx,
        &mut buyer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest",
        get_lic_req,
    )
    .await;
    assert_eq!(get_lic_rsp.status_code, STATUS_OK, "{}", get_lic_rsp.status_message);

    let buyer_lic = ctx.db.with_snapshot(|s| {
        let st: eve_server::services::quasar_cosmetics_skinr::SkinrCosmeticsData = s
            .quasar_state_blobs
            .get("quasar_cosmetics_skinr")
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        st.skin_licenses.get(&format!("{buyer_cid}:{skin_hex}")).map(|l| l.unactivated).unwrap_or(0)
    });
    assert_eq!(buyer_lic, 1);
}

}
