//! Part 1 of Quasar, Cosmetics & Seasons Closure Test Suite.
//! Covers:
//! - Season Manager RPCs: `get_season_data_for_character`, `get_season_end_time`, `select_season`
//! - Cosmetics Features: `apparel`, `cosmetics-broadcast`, `ship-emblems`, `skinr-designer`, `skinr-market`

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
    // parity: rpc:seasonManager.get_season_end_time, rpc:seasonManager.select_season, rpc:seasonManager.get_season_data_for_character
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
        assert_eq!(it.flag, InventoryFlag::Pilot, "worn apparel flag must persist as Pilot");
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
    // parity: feat:cosmetics.skinr-designer
    // parity: feat:cosmetics.skinr-market
    eve_testkit::attest(&[
        "feat:cosmetics.skinr-designer",
        "feat:cosmetics.skinr-market",
    ]);

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
