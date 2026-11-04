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

pub(crate) mod quasar_ship_unlock_starterspace_caster {
    // Live `publicGatewaySvc` integration tests for Batch 2 Quasar protobuf handlers.

    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
        EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::{
        STATUS_NOT_FOUND, STATUS_NOT_IMPLEMENTED, STATUS_OK, STATUS_UNAUTHORIZED,
    };
    use eve_server::router::ServerContext;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn seed_quasar_b2_fixtures(ctx: &ServerContext) {
        let now = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(
                1_030_000_000_001,
                eve_db::UpwellStructureRecord {
                    structure_id: 1_030_000_000_001,
                    type_id: 35832, // Astrahus
                    owner_corp_id: 98_000_001,
                    solar_system_id: 30_000_142,
                    name: "Jita Trade Citadel".to_string(),
                    position: [0.0, 0.0, 0.0],
                    state: "ShieldVulnerable".to_string(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    shield_hp: 100_000.0,
                    armor_hp: 100_000.0,
                    hull_hp: 100_000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            snap.zarzakh_gate_locks
                .insert(2_112_000_001, (50_001_248, now + 3_600));
            snap.system_states.insert(
                30_002_537,
                eve_db::SystemDynamicState {
                    solar_system_id: 30_002_537,
                    dbs_multiplier: 1.0,
                    sov_alliance_id: Some(99_000_001),
                    military_index: 5,
                    industrial_index: 4,
                    strategic_index: 5,
                    ..Default::default()
                },
            );
            snap.structures.insert(
                1_040_000_000_001,
                eve_db::UpwellStructureRecord {
                    structure_id: 1_040_000_000_001,
                    type_id: 32458, // Sovereignty Hub
                    owner_corp_id: 98_000_001,
                    solar_system_id: 30_002_537,
                    name: "Sov Hub".to_string(),
                    position: [0.0, 0.0, 0.0],
                    state: "ShieldVulnerable".to_string(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    shield_hp: 100_000.0,
                    armor_hp: 100_000.0,
                    hull_hp: 100_000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            snap.sov_runtime.sov_hubs.insert(
                1_040_000_000_001,
                eve_db::PersistedSovHubData {
                    hub_id: 1_040_000_000_001,
                    ..Default::default()
                },
            );
        });
    }

    static CORR_SEQ: AtomicU64 = AtomicU64::new(1);

    fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
        let mut sess = SessionState::new(sid);
        sess.user_id = Some(uid);
        sess.character_id = Some(cid);
        sess.corporation_id = Some(corp_id);
        sess.solar_system_id = Some(30_000_142);
        sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
        sess
    }

    fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
        if let PyRep::KeyVal(entries) = rep {
            for (k, v) in entries {
                if k == "response_proto" {
                    if let PyRep::Bytes(b) = v {
                        return EvePublicResponse::decode_proto(b)
                            .expect("decode EvePublicResponse from response_proto");
                    }
                }
            }
        }
        panic!("Expected KeyVal containing response_proto, got {rep:?}");
    }

    async fn quasar_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        type_name: &str,
        payload_bytes: Vec<u8>,
    ) -> EvePublicResponse {
        let seq = CORR_SEQ.fetch_add(1, Ordering::Relaxed);
        let mut corr = *b"quasar-b2-000000";
        corr[10..16].copy_from_slice(&seq.to_le_bytes()[..6]);
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: ctx.now_unix() as i64,
                nanos: 0,
            }),
            correlation_uuid: corr.to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
                value: payload_bytes,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"app-quasar-b2".to_vec(),
            authoritative_context: None,
            trace_context: None,
        };
        let empty_kw = HashMap::new();
        let rep = ctx
            .dispatch_rpc(
                sess,
                "publicGatewaySvc",
                None,
                "SendRequest",
                &[PyRep::Bytes(req.encode_proto())],
                &empty_kw,
                None,
            )
            .await
            .expect("SendRequest");
        extract_response_proto(&rep)
    }

    #[derive(Debug, Clone)]
    enum RawField {
        Varint(u64),
        Bytes(Vec<u8>),
    }

    fn parse_raw_proto(mut buf: &[u8]) -> Vec<(u32, RawField)> {
        let mut out = Vec::new();
        while !buf.is_empty() {
            let mut tag = 0u64;
            let mut shift = 0;
            loop {
                let b = buf[0];
                buf = &buf[1..];
                tag |= ((b & 0x7f) as u64) << shift;
                if b & 0x80 == 0 {
                    break;
                }
                shift += 7;
            }
            let field_num = (tag >> 3) as u32;
            let wire_type = (tag & 0x07) as u8;
            match wire_type {
                0 => {
                    let mut val = 0u64;
                    let mut s = 0;
                    loop {
                        let b = buf[0];
                        buf = &buf[1..];
                        val |= ((b & 0x7f) as u64) << s;
                        if b & 0x80 == 0 {
                            break;
                        }
                        s += 7;
                    }
                    out.push((field_num, RawField::Varint(val)));
                }
                2 => {
                    let mut len = 0u64;
                    let mut s = 0;
                    loop {
                        let b = buf[0];
                        buf = &buf[1..];
                        len |= ((b & 0x7f) as u64) << s;
                        if b & 0x80 == 0 {
                            break;
                        }
                        s += 7;
                    }
                    let len = len as usize;
                    out.push((field_num, RawField::Bytes(buf[..len].to_vec())));
                    buf = &buf[len..];
                }
                _ => panic!("Unexpected wire type {wire_type}"),
            }
        }
        out
    }

    fn encode_id_field(field_num: u32, id: u64, out: &mut Vec<u8>) {
        let mut sub = Vec::new();
        encode_varint_field(1, id, &mut sub);
        encode_bytes_field(field_num, &sub, out);
    }

    fn first_varint(fields: &[(u32, RawField)], field_num: u32) -> Option<u64> {
        fields.iter().find_map(|(f, v)| match v {
            RawField::Varint(n) if *f == field_num => Some(*n),
            _ => None,
        })
    }

    fn first_bytes(fields: &[(u32, RawField)], field_num: u32) -> Option<&[u8]> {
        fields.iter().find_map(|(f, v)| match v {
            RawField::Bytes(b) if *f == field_num => Some(b.as_slice()),
            _ => None,
        })
    }

    fn decode_nested_id(fields: &[(u32, RawField)], field_num: u32) -> Option<u64> {
        first_bytes(fields, field_num).and_then(|b| first_varint(&parse_raw_proto(b), 1))
    }

    #[tokio::test]
    async fn test_quasar_b2_ship_unlock_and_starterspace_caster() {
        // parity: quasar:eveProto/generated/eve/ship/api/requests_pb2.pyj, quasar:eveProto/generated/eve/starterspace/api/requests_pb2.pyj
        let ctx = ServerContext::new();
        seed_quasar_b2_fixtures(&ctx);
        let mut sess = make_session(1, 1001, 2_112_000_001, 98_000_001);

        // 1. eve_public.ship.unlock.* -> STATUS_NOT_IMPLEMENTED (DATA GAP)
        for req_type in [
            "eve_public.ship.unlock.GetPendingNotificationsRequest",
            "eve_public.ship.unlock.ClaimRewardRequest",
        ] {
            let rsp = quasar_rpc(&ctx, &mut sess, req_type, Vec::new()).await;
            assert_eq!(rsp.status_code, STATUS_NOT_IMPLEMENTED, "{req_type}");
        }

        // 3. CanJumpStarterSpaceShipCasterRequest & CanJumpShipCasterRequest
        // Seed CHAR_A in space (30_000_142) + starter_launchers; while zarzakh_gate_lock is active, both return false (0)
        ctx.seed_demo_world_fixtures();
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(mut ch) = snap.characters.values().next().cloned() {
                ch.character_id = 2_112_000_001;
                ch.user_id = 1001;
                ch.corporation_id = 98_000_001;
                ch.solar_system_id = 30_000_142;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = 1_010_000_001;
                ch.jump_cooldown_until_unix = 0;
                snap.characters.insert(2_112_000_001, ch);
            }
            snap.items.insert(1_010_000_001, eve_db::InventoryItemRecord {
                item_id: 1_010_000_001, type_id: 606, owner_id: 2_112_000_001,
                location_id: 30_000_142, quantity: 1, singleton: true, ..Default::default()
            });
            snap.service_state_blobs.insert("cluster_misc_state".to_string(), r#"{"starter_launchers":[1]}"#.to_string());
        });
        let mut caster_req = Vec::new();
        encode_id_field(1, 2_112_000_001, &mut caster_req);
        let r1 = quasar_rpc(&ctx, &mut sess, "eve.ship.api.CanJumpStarterSpaceShipCasterRequest", caster_req.clone()).await;
        assert_eq!(r1.status_code, STATUS_OK);
        assert_eq!(first_varint(&parse_raw_proto(&r1.payload.unwrap().value), 1), Some(0));

        let r2 = quasar_rpc(&ctx, &mut sess, "eve.starterspace.api.CanJumpShipCasterRequest", caster_req.clone()).await;
        assert_eq!(r2.status_code, STATUS_OK);
        assert_eq!(first_varint(&parse_raw_proto(&r2.payload.unwrap().value), 1), Some(0));

        // Clearing zarzakh_gate_lock enables both ShipCaster endpoints (allowed == 1)
        ctx.db.with_mut_snapshot(|snap| { snap.zarzakh_gate_locks.remove(&2_112_000_001); });
        let r1_ok = quasar_rpc(&ctx, &mut sess, "eve.ship.api.CanJumpStarterSpaceShipCasterRequest", caster_req.clone()).await;
        assert_eq!(r1_ok.status_code, STATUS_OK);
        assert_eq!(first_varint(&parse_raw_proto(&r1_ok.payload.unwrap().value), 1), Some(1));
        let r2_ok = quasar_rpc(&ctx, &mut sess, "eve.starterspace.api.CanJumpShipCasterRequest", caster_req.clone()).await;
        assert_eq!(r2_ok.status_code, STATUS_OK);
        assert_eq!(first_varint(&parse_raw_proto(&r2_ok.payload.unwrap().value), 1), Some(1));

        // Emptying starter_launchers in cluster_misc_state disables CanJumpStarterSpaceShipCasterRequest
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs.insert("cluster_misc_state".to_string(), r#"{"starter_launchers":[]}"#.to_string());
        });
        let r3 = quasar_rpc(&ctx, &mut sess, "eve.ship.api.CanJumpStarterSpaceShipCasterRequest", caster_req).await;
        assert_eq!(r3.status_code, STATUS_OK);
        assert_eq!(first_varint(&parse_raw_proto(&r3.payload.unwrap().value), 1), Some(0));

        // Unauthenticated session is rejected with STATUS_UNAUTHORIZED
        let mut unauth = SessionState::new(99);
        let unauth_rsp = quasar_rpc(&ctx, &mut unauth, "eve_public.ship.unlock.GetPendingNotificationsRequest", Vec::new()).await;
        assert_eq!(unauth_rsp.status_code, STATUS_UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_quasar_b2_structure_star_resources_and_space_tags() {
        // parity: quasar:eveProto/generated/eve/structure/api/requests_pb2.pyj, quasar:eveProto/generated/eve/space/tag/api/admin/requests_pb2.pyj
        let ctx = ServerContext::new();
        seed_quasar_b2_fixtures(&ctx);
        let mut sess = make_session(2, 1002, 2_112_000_002, 98_000_001);

        // 1. eve.structure.api.GetRequest -> wired to snap.structures
        let mut struct_req = Vec::new();
        encode_id_field(1, 1_030_000_000_001, &mut struct_req);
        let srsp = quasar_rpc(&ctx, &mut sess, "eve.structure.api.GetRequest", struct_req).await;
        assert_eq!(srsp.status_code, STATUS_OK);
        let sfields = parse_raw_proto(&srsp.payload.unwrap().value);
        let attrs = parse_raw_proto(first_bytes(&sfields, 1).unwrap());
        assert_eq!(decode_nested_id(&attrs, 1), Some(35832));
        assert_eq!(decode_nested_id(&attrs, 2), Some(30_000_142));
        assert_eq!(decode_nested_id(&attrs, 3), Some(98_000_001));
        assert_eq!(
            first_bytes(&attrs, 4),
            Some(b"Jita Trade Citadel".as_slice())
        );

        // Unknown, unauthorized private, and destroyed structures return NOT_FOUND
        let mut bad_struct = Vec::new();
        encode_id_field(1, 9_999_999, &mut bad_struct);
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.structure.api.GetRequest", bad_struct).await.status_code, STATUS_NOT_FOUND);
        let mut outsider = make_session(20, 1020, 2_112_000_020, 98_000_999);
        outsider.role = 0;
        let mut priv_req = Vec::new();
        encode_id_field(1, 1_030_000_000_001, &mut priv_req);
        assert_eq!(quasar_rpc(&ctx, &mut outsider, "eve.structure.api.GetRequest", priv_req.clone()).await.status_code, STATUS_NOT_FOUND);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&1_030_000_000_001) { st.state = "Destroyed".to_string(); }
        });
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.structure.api.GetRequest", priv_req).await.status_code, STATUS_NOT_FOUND);

        // 2. eve.star.resources.api.GetPowerProductionRequest -> STATUS_NOT_IMPLEMENTED (DATA GAP)
        let mut star_req = Vec::new();
        encode_id_field(1, 40_009_041, &mut star_req);
        let starrsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.star.resources.api.GetPowerProductionRequest",
            star_req,
        )
        .await;
        assert_eq!(starrsp.status_code, STATUS_NOT_IMPLEMENTED);

        // 3. eve.space.tag.admin.api.ApplyRequest & RemoveRequest (handled by quasar_space_domain)
        let jita = ctx.sde.solar_systems.get(&30_000_142).expect("Jita in SDE");
        let mut loc_id = Vec::new();
        encode_id_field(1, jita.region_id as u64, &mut loc_id);

        let mut tag_id = Vec::new();
        encode_string_field(1, "incursion_active", &mut tag_id);

        let mut apply_req = Vec::new();
        encode_bytes_field(2, &loc_id, &mut apply_req);
        let mut spec = Vec::new();
        encode_bytes_field(1, &tag_id, &mut spec);
        encode_varint_field(2, 1, &mut spec);
        encode_bytes_field(3, &spec, &mut apply_req);
        let apply_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.space.tag.admin.api.ApplyRequest",
            apply_req,
        )
        .await;
        assert_eq!(apply_rsp.status_code, STATUS_OK, "{apply_rsp:?}");
        let mut get_locs_req = Vec::new();
        encode_bytes_field(1, &tag_id, &mut get_locs_req);
        let get_locs_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.space.tag.api.GetLocationsRequest",
            get_locs_req.clone(),
        )
        .await;
        assert_eq!(get_locs_rsp.status_code, STATUS_OK);
        let locs_bytes = get_locs_rsp.payload.unwrap().value;
        assert!(locs_bytes.windows(loc_id.len()).any(|w| w == loc_id.as_slice()));

        let mut remove_req = Vec::new();
        encode_bytes_field(2, &loc_id, &mut remove_req);
        encode_bytes_field(3, &tag_id, &mut remove_req);
        let remove_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.space.tag.admin.api.RemoveRequest",
            remove_req,
        )
        .await;
        assert_eq!(remove_rsp.status_code, STATUS_OK);
        let get_locs_after = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.space.tag.api.GetLocationsRequest",
            get_locs_req,
        )
        .await;
        assert_eq!(get_locs_after.status_code, STATUS_OK);
        let locs_after_bytes = get_locs_after.payload.unwrap().value;
        assert!(!locs_after_bytes.windows(loc_id.len()).any(|w| w == loc_id.as_slice()));
    }

    #[tokio::test]
    async fn test_quasar_b2_stargate_locks_and_delete_command() {
        // parity: quasar:eveProto/generated/eve/stargate/lock/api/requests_pb2.pyj, quasar:eveProto/generated/eve/stargate/lock/api/commands_pb2.pyj
        let ctx = ServerContext::new();
        seed_quasar_b2_fixtures(&ctx);
        let mut sess = make_session(3, 1003, 2_112_000_001, 98_000_001);

        // 1. GetRestrictedSystemsRequest -> Zarzakh (30_100_000)
        let restr_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.stargate.lock.api.GetRestrictedSystemsRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(restr_rsp.status_code, STATUS_OK);
        let restr_fields = parse_raw_proto(&restr_rsp.payload.unwrap().value);
        assert_eq!(decode_nested_id(&restr_fields, 1), Some(30_100_000));

        // 2. GetRequest on non-restricted Jita (30_000_142) returns STATUS_NOT_FOUND; Zarzakh (30_100_000) returns active lock
        let mut jita_get_req = Vec::new();
        encode_id_field(1, 2_112_000_001, &mut jita_get_req);
        encode_id_field(2, 30_000_142, &mut jita_get_req);
        let jita_get_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.stargate.lock.api.GetRequest",
            jita_get_req,
        )
        .await;
        assert_eq!(jita_get_rsp.status_code, STATUS_NOT_FOUND);

        let mut mismatch_get_req = Vec::new();
        encode_id_field(1, 2_112_999_999, &mut mismatch_get_req);
        encode_id_field(2, 30_100_000, &mut mismatch_get_req);
        let mismatch_get_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.stargate.lock.api.GetRequest",
            mismatch_get_req,
        )
        .await;
        assert_eq!(mismatch_get_rsp.status_code, 403);

        let mut get_req = Vec::new();
        encode_id_field(1, 2_112_000_001, &mut get_req);
        encode_id_field(2, 30_100_000, &mut get_req);
        let get_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.stargate.lock.api.GetRequest",
            get_req.clone(),
        )
        .await;
        assert_eq!(get_rsp.status_code, STATUS_OK);
        let gfields = parse_raw_proto(&get_rsp.payload.unwrap().value);
        assert_eq!(decode_nested_id(&gfields, 1), Some(50_001_248));
        assert!(decode_nested_id(&gfields, 2).unwrap_or(0) > ctx.now_unix());

        // 3. ValidateJumpRequest on Jita (30_000_142) returns allowed = true (1); Zarzakh (30_100_000) on a different gate returns allowed = false (0) + lock details
        let mut jita_val_req = Vec::new();
        encode_id_field(1, 2_112_000_001, &mut jita_val_req);
        encode_id_field(2, 30_000_142, &mut jita_val_req);
        encode_id_field(3, 50_001_999, &mut jita_val_req);
        let jita_val_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.stargate.lock.api.ValidateJumpRequest",
            jita_val_req,
        )
        .await;
        assert_eq!(jita_val_rsp.status_code, STATUS_OK);
        assert_eq!(
            first_varint(&parse_raw_proto(&jita_val_rsp.payload.unwrap().value), 1),
            Some(1)
        );

        let mut val_req = Vec::new();
        encode_id_field(1, 2_112_000_001, &mut val_req);
        encode_id_field(2, 30_100_000, &mut val_req);
        encode_id_field(3, 50_001_999, &mut val_req);
        let val_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.stargate.lock.api.ValidateJumpRequest",
            val_req.clone(),
        )
        .await;
        assert_eq!(val_rsp.status_code, STATUS_OK);
        let vfields = parse_raw_proto(&val_rsp.payload.unwrap().value);
        assert_eq!(first_varint(&vfields, 1), Some(0));
        let lock_sub = parse_raw_proto(first_bytes(&vfields, 2).unwrap());
        assert_eq!(decode_nested_id(&lock_sub, 1), Some(50_001_248));

        // 4. DeleteCommanded requires admin role, returns STATUS_NOT_FOUND for Jita, then removes Zarzakh lock
        let mut jita_del_req = Vec::new();
        encode_id_field(2, 2_112_000_001, &mut jita_del_req);
        encode_id_field(3, 30_000_142, &mut jita_del_req);
        let jita_del_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.stargate.lock.api.DeleteCommanded",
            jita_del_req,
        )
        .await;
        assert_eq!(jita_del_rsp.status_code, STATUS_NOT_FOUND);

        let mut del_req = Vec::new();
        encode_id_field(2, 2_112_000_001, &mut del_req);
        encode_id_field(3, 30_100_000, &mut del_req);

        let mut non_admin = make_session(33, 1033, 2_112_000_001, 98_000_001);
        non_admin.role = 0;
        let unauth_del = quasar_rpc(
            &ctx,
            &mut non_admin,
            "eve.stargate.lock.api.DeleteCommanded",
            del_req.clone(),
        )
        .await;
        assert_eq!(unauth_del.status_code, 403);

        let del_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.stargate.lock.api.DeleteCommanded",
            del_req,
        )
        .await;
        assert_eq!(del_rsp.status_code, STATUS_OK);

        let val_after = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.stargate.lock.api.ValidateJumpRequest",
            val_req.clone(),
        )
        .await;
        assert_eq!(val_after.status_code, STATUS_OK);
        assert_eq!(
            first_varint(&parse_raw_proto(&val_after.payload.unwrap().value), 1),
            Some(1)
        );

        // 5. Expired stargate lock is treated as inactive by GetRequest and ValidateJumpRequest
        seed_quasar_b2_fixtures(&ctx);
        ctx.advance_time_seconds(3_601);
        let expired_get = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.stargate.lock.api.GetRequest",
            get_req,
        )
        .await;
        assert_eq!(expired_get.status_code, STATUS_NOT_FOUND);
        let expired_val = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.stargate.lock.api.ValidateJumpRequest",
            val_req,
        )
        .await;
        assert_eq!(expired_val.status_code, STATUS_OK);
        assert_eq!(
            first_varint(&parse_raw_proto(&expired_val.payload.unwrap().value), 1),
            Some(1)
        );
    }

    #[tokio::test]
    async fn test_quasar_b2_sovereignty_claims_adm_and_resources() {
        // parity: quasar:eveProto/generated/eve/sovereignty/api/requests_pb2.pyj, quasar:eveProto/generated/eve/sovereignty/activitydefensemodifier/api/requests_pb2.pyj
        let ctx = ServerContext::new();
        seed_quasar_b2_fixtures(&ctx);
        let mut sess = make_session(4, 1004, 2_112_000_004, 98_000_001);

        // 1. GetClaimRequest (claimed system 30_002_537)
        let mut claim_req = Vec::new();
        encode_id_field(1, 30_002_537, &mut claim_req);
        let crsp = quasar_rpc(&ctx, &mut sess, "eve.sovereignty.api.GetClaimRequest", claim_req).await;
        assert_eq!(crsp.status_code, STATUS_OK);
        let cfields = parse_raw_proto(&crsp.payload.unwrap().value);
        assert_eq!(first_varint(&cfields, 1), Some(0));
        let cbuf = parse_raw_proto(first_bytes(&cfields, 2).unwrap());
        assert_eq!(decode_nested_id(&cbuf, 1), Some(99_000_001));
        assert_eq!(decode_nested_id(&cbuf, 3), Some(1_040_000_000_001));
        assert_eq!(decode_nested_id(&cbuf, 4), Some(98_000_001));

        // GetVersionManifestRequest
        let mrsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.sovereignty.api.GetVersionManifestRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(mrsp.status_code, STATUS_OK);
        let mfields = parse_raw_proto(&mrsp.payload.unwrap().value);
        let manifest = parse_raw_proto(first_bytes(&mfields, 1).unwrap());
        assert_eq!(manifest.len(), 6);

        // GetAllClaimsRequest
        let all_claims = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.sovereignty.api.GetAllClaimsRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(all_claims.status_code, STATUS_OK);
        let all_fields = parse_raw_proto(&all_claims.payload.unwrap().value);
        let sc = parse_raw_proto(first_bytes(&all_fields, 1).unwrap());
        assert_eq!(decode_nested_id(&sc, 1), Some(30_002_537));
        let ac = parse_raw_proto(first_bytes(&sc, 3).unwrap());
        assert_eq!(decode_nested_id(&ac, 1), Some(99_000_001));
        assert_eq!(decode_nested_id(&ac, 2), Some(98_000_001));

        // 2. GetLevelRequest (ADM) -> wired to snap.system_states via calculate_adm
        let mut adm_req = Vec::new();
        encode_id_field(1, 30_002_537, &mut adm_req);
        let adm_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.sovereignty.activitydefensemodifier.api.GetLevelRequest",
            adm_req.clone(),
        )
        .await;
        assert_eq!(adm_rsp.status_code, STATUS_OK);
        assert_eq!(
            first_varint(&parse_raw_proto(&adm_rsp.payload.unwrap().value), 1),
            Some(60)
        );

        // Non-saturated index combination (1, 2, 1) -> 1.0 + 0.6 + 1.2 + 0.4 = 3.2 -> 32
        ctx.db.with_mut_snapshot(|snap| {
            let st = snap.system_states.get_mut(&30_002_537).unwrap();
            st.military_index = 1;
            st.industrial_index = 2;
            st.strategic_index = 1;
        });
        let adm_non_sat = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.sovereignty.activitydefensemodifier.api.GetLevelRequest",
            adm_req,
        )
        .await;
        assert_eq!(adm_non_sat.status_code, STATUS_OK);
        assert_eq!(
            first_varint(&parse_raw_proto(&adm_non_sat.payload.unwrap().value), 1),
            Some(32)
        );

        // 3. Equinox star & planet resource handlers -> STATUS_NOT_IMPLEMENTED (DATA GAP)
        for req_type in [
            "eve.sovereignty.resource.star.api.GetAllConfigurationsRequest",
            "eve.sovereignty.resource.planet.api.GetDefinitionsConfigurationRequest",
            "eve.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
            "eve.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
            "eve.sovereignty.resource.planet.api.GetAllConfigurationsRequest",
        ] {
            let rsp = quasar_rpc(&ctx, &mut sess, req_type, Vec::new()).await;
            assert_eq!(rsp.status_code, STATUS_NOT_IMPLEMENTED, "{req_type}");
        }
        let mut overflow_sys = Vec::new();
        encode_id_field(1, 30_002_537 + (1u64 << 32), &mut overflow_sys);
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.sovereignty.api.GetClaimRequest", overflow_sys).await.status_code, eve_server::quasar_gateway::STATUS_BAD_REQUEST);
    }
}

pub(crate) mod quasar_sov_hub_star_resources {
    // Batch 22 Quasar closure integration tests:
    // - Sovereignty Hub resources, fuel, upgrades, workforce, and star/planet resource configurations
    // - Mercenary Den ownership, evolution, and activity lifecycle
    // - Batch 22 Quasar services (`GetSharedRequest` for SKINR drafts, `GetRequest` / `GetAllRequest` for cosmetic market transactions)


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
    const NULLSEC_A: u32 = 30_004_759;
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
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(HUB_A, make_structure(HUB_A, CORP, NULLSEC_A, TYPE_SOVEREIGNTY_HUB));
            if let Some(ch) = snap.characters.get_mut(&PILOT) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = NULLSEC_A;
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

        let mut s = sess(61_001, PILOT, CORP, NULLSEC_A, corp_roles::ROLE_DIRECTOR);
        let mut outsider = sess(61_002, OTHER, 98_000_002, NULLSEC_A, 0);

        // 1. Star & Planet resource configurations from SDE
        let star_rsp = rpc(&ctx, &mut s, "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest", Vec::new()).await;
        assert_eq!(star_rsp.status_code, STATUS_OK);
        let star_wm = wm(&star_rsp.payload.unwrap().value);
        let stars = fields(&star_wm, 1);
        assert!(!stars.is_empty(), "Star configuration must be returned");
        assert!(varint(&stars[0], 2).unwrap_or(0) > 0, "Star power output must be positive");

        let planet_rsp = rpc(&ctx, &mut s, "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest", Vec::new()).await;
        assert_eq!(planet_rsp.status_code, STATUS_OK);
        let planet_wm = wm(&planet_rsp.payload.unwrap().value);
        assert!(!fields(&planet_wm, 1).is_empty(), "Planet configurations must be returned from SDE");

        // 2. Hub resources & fuel state mutation
        let res_before = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, HUB_A as u64)).await;
        assert_eq!(res_before.status_code, STATUS_OK);
        let power_before = varint(&sub(&wm(&res_before.payload.unwrap().value), 1), 1).unwrap_or(0);
        assert!(power_before > 0, "Null-sec star + planets provide positive power");

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
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let nullsec_b = eve_server::services::quasar_sov_workforce::systems_within_jumps(&ctx.sde, NULLSEC_A, 1)
            .into_iter()
            .find_map(|(id, _)| (id != NULLSEC_A).then_some(id))
            .unwrap();
        let planet_id = ctx
            .sde
            .celestials_in_system(NULLSEC_A)
            .into_iter()
            .find(|c| c.group_id == 7)
            .map(|c| c.celestial_id)
            .unwrap_or(40_009_080);
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(HUB_A, make_structure(HUB_A, CORP, NULLSEC_A, TYPE_SOVEREIGNTY_HUB));
            snap.structures.insert(HUB_B, make_structure(HUB_B, CORP, nullsec_b, TYPE_SOVEREIGNTY_HUB));
            snap.structures.insert(1_000_000_301, make_structure(1_000_000_301, CORP, NULLSEC_A, eve_server::services::sov_services::TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(1_000_000_301, planet_id);
            snap.sov_runtime.skyhook_active.insert(1_000_000_301, true);
            if let Some(ch) = snap.characters.get_mut(&PILOT) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = NULLSEC_A;
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

        let mut s = sess(61_003, PILOT, CORP, NULLSEC_A, corp_roles::ROLE_DIRECTOR);
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

        // Configure workforce export from Hub A to neighbor null-sec system
        let mut exp = Vec::new();
        encode_bytes_field(3, &id_req(1, nullsec_b as u64)[2..], &mut exp);
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
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let (skyhook_id, den_id) = (1_000_000_050i64, 1_000_000_060i64);
        let mut s = sess(61_004, PILOT, CORP, NULLSEC_A, 0);
        let mut rx = subscribe(&ctx, &mut s).await;

        let planet_id = ctx
            .sde
            .celestials_in_system(NULLSEC_A)
            .into_iter()
            .find(|c| c.group_id == 7)
            .map(|c| c.celestial_id)
            .unwrap_or(40_009_077);
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(skyhook_id, make_structure(skyhook_id, CORP, NULLSEC_A, TYPE_ORBITAL_SKYHOOK));
            snap.structures.insert(den_id, make_structure(den_id, CORP, NULLSEC_A, 85_230));
            snap.sov_runtime.mercenary_dens.insert(
                den_id,
                PersistedMercenaryDen {
                    den_id,
                    type_id: 85_230,
                    owner_char_id: PILOT,
                    skyhook_id,
                    solar_system_id: NULLSEC_A,
                    planet_id,
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
                ch.solar_system_id = NULLSEC_A;
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

        // Link-based SKINR draft sharing: non-owner fetches owner's saved draft via GetSharedRequest(character, draft)
        let owner_rsp = rpc(&ctx, &mut s, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest", shared_req.clone()).await;
        assert_eq!(owner_rsp.status_code, STATUS_OK);
        let shared_rsp = rpc(&ctx, &mut other, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSharedRequest", shared_req).await;
        assert_eq!(shared_rsp.status_code, STATUS_OK, "{}", shared_rsp.status_message);

        // Owner-scoped GetRequest(draft) does not expose another character's draft
        let mut private_get_req = Vec::new();
        let mut priv_id = Vec::new();
        encode_bytes_field(1, &draft_uuid, &mut priv_id);
        encode_bytes_field(1, &priv_id, &mut private_get_req);
        assert_eq!(
            rpc(&ctx, &mut other, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest", private_get_req)
                .await
                .status_code,
            STATUS_FORBIDDEN
        );

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
}

pub(crate) mod orchestrator_version_deployment_content_instances {
    // Integration tests for Batch 29 Quasar Public Gateway services.

    use eve_db::{AccountRecord, CharacterRecord, WorldRepository};
    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
        DynamicValue, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
        TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::{
        STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
    };
    use eve_server::router::ServerContext;
    use eve_server::services::quasar_admin_orchestrator::{
        ContentGroupRecord, ContentTypeRecord, DeploymentInstanceRecord, DeploymentRecord,
        FailedFulfillmentRecord, OrchestratorState, TargetedOfferAdminRecord,
    };
    use eve_server::services::quasar_cosmetics_market_types::{
        CosmeticsMarketData, ListingTargetRecord, SkinListingRecord,
    };
    use eve_server::services::quasar_payment_store_types::get_plex_transactions;
    use std::collections::{BTreeMap, HashMap};
    use std::sync::atomic::{AtomicU64, Ordering};

    static CORR_SEQ: AtomicU64 = AtomicU64::new(1);

    fn make_session(sid: u64, uid: u64, cid: i64, role: u64) -> SessionState {
        let mut sess = SessionState::new(sid);
        sess.user_id = Some(uid);
        sess.character_id = Some(cid);
        sess.corporation_id = Some(98_000_001);
        sess.solar_system_id = Some(30_000_142);
        sess.role = role;
        sess
    }

    fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
        if let PyRep::KeyVal(entries) = rep {
            for (k, v) in entries {
                if k == "response_proto" {
                    if let PyRep::Bytes(b) = v {
                        return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                    }
                }
            }
        }
        panic!("Expected KeyVal containing response_proto, got {rep:?}");
    }

    async fn quasar_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        type_name: &str,
        payload_bytes: Vec<u8>,
    ) -> EvePublicResponse {
        let seq = CORR_SEQ.fetch_add(1, Ordering::Relaxed);
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
            correlation_uuid: format!("b29-quasar-{seq:05}").into_bytes(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
                value: payload_bytes,
            }),
            external_origin: "eve-client".into(),
            ..Default::default()
        };
        let rep = ctx
            .dispatch_rpc(sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None)
            .await
            .expect("publicGatewaySvc.SendRequest");
        extract_response_proto(&rep)
    }

    fn decode_dyn(ctx: &ServerContext, type_name: &str, bytes: &[u8]) -> DynamicQuasarMessage {
        ctx.quasar_gateway
            .descriptors
            .decode_dynamic(type_name, bytes)
            .unwrap_or_else(|e| panic!("failed to decode {type_name}: {e:?}"))
    }

    fn seq_bytes(id: u64) -> Vec<u8> {
        let mut sub = Vec::new();
        encode_varint_field(1, id, &mut sub);
        let mut out = Vec::new();
        encode_bytes_field(1, &sub, &mut out);
        out
    }

    fn sint32_user_bytes(id: i32) -> Vec<u8> {
        let zz = ((id << 1) ^ (id >> 31)) as u32;
        let mut sub = Vec::new();
        encode_varint_field(1, zz as u64, &mut sub);
        let mut out = Vec::new();
        encode_bytes_field(1, &sub, &mut out);
        out
    }

    fn uuid_bytes(uuid: &[u8]) -> Vec<u8> {
        let mut sub = Vec::new();
        encode_bytes_field(1, uuid, &mut sub);
        let mut out = Vec::new();
        encode_bytes_field(1, &sub, &mut out);
        out
    }

    fn encode_semver(field_num: u32, major: u64, minor: u64, patch: u64, out: &mut Vec<u8>) {
        let mut sub = Vec::new();
        if major > 0 { encode_varint_field(1, major, &mut sub); }
        if minor > 0 { encode_varint_field(2, minor, &mut sub); }
        if patch > 0 { encode_varint_field(3, patch, &mut sub); }
        encode_bytes_field(field_num, &sub, out);
    }

    fn seed_test_orchestrator(ctx: &ServerContext) {
        let mut st = OrchestratorState::default();
        st.deployments.insert(101, DeploymentRecord {
            id: 101,
            name: "pirate_insurgency_deployment".into(),
            short_description: "Pirate insurgency forward operating sites".into(),
            content_group_id: 201,
            enabled: true,
            current_content_count: 1,
            no_region_limit: true,
        });
        st.content_groups.insert(201, ContentGroupRecord {
            id: 201,
            name: "insurgency_sites".into(),
            content_type_id: 301,
            weight: 1.0,
        });
        st.content_types.insert(301, ContentTypeRecord {
            id: 301,
            name: "guristas_outpost".into(),
            short_description: "Guristas forward outpost".into(),
            dungeon_id: 8001,
        });
        st.instances.push(DeploymentInstanceRecord {
            uuid: (*b"TESTORCHINST0001").to_vec(),
            deployment_id: 101,
            content_type_id: 301,
            solar_system_id: 30_000_142,
            constellation_id: 20_000_020,
            created_unix: 1_700_000_000,
        });
        let json = serde_json::to_string(&st).unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs.insert("content_orchestrator_state".to_string(), json);
        });
    }

    #[tokio::test]
    // parity: quasar:eveProto/generated/eve/content/orchestrator/admin/api/requests_pb2.pyj, quasar:eveProto/generated/eve/content/orchestrator/api/requests_pb2.pyj, quasar:eveProto/generated/eve/content/orchestrator/deployment/api/requests_pb2.pyj
    async fn test_b29_orchestrator_version_and_deployment() {
        let ctx = ServerContext::new();
        seed_test_orchestrator(&ctx);
        let mut sess = make_session(1, 1001, 2112000001, eve_machonet::service_roles::ROLE_GM_DEFAULT);
        let mut player = make_session(10, 2001, 2112000099, eve_machonet::service_roles::ROLE_PLAYER_DEFAULT);

        // 1. SetDefaultTenantVersionRequest & GetDefaultTenantVersionRequest
        let mut set_def = Vec::new();
        encode_semver(1, 2, 3, 4, &mut set_def);
        assert_eq!(quasar_rpc(&ctx, &mut player, "eve.content.orchestrator.admin.api.SetDefaultTenantVersionRequest", set_def.clone()).await.status_code, STATUS_FORBIDDEN);
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.admin.api.SetDefaultTenantVersionRequest", set_def).await.status_code, STATUS_OK);

        let r2 = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.api.GetDefaultTenantVersionRequest", Vec::new()).await;
        assert_eq!(r2.status_code, STATUS_OK);
        let dyn_ver = decode_dyn(&ctx, "eve.content.orchestrator.api.GetDefaultTenantVersionResponse", &r2.payload.unwrap().value);
        if let Some(DynamicValue::Message(v)) = dyn_ver.get(1) {
            assert_eq!((v.get(1), v.get(2), v.get(3)), (Some(&DynamicValue::Uint32(2)), Some(&DynamicValue::Uint32(3)), Some(&DynamicValue::Uint32(4))));
        } else {
            panic!("expected default version message");
        }

        // 2. SetTenantVersionRequest & GetTenantVersionRequest (override + reset to default)
        let mut set_ten = Vec::new();
        encode_semver(2, 3, 1, 2, &mut set_ten);
        assert_eq!(quasar_rpc(&ctx, &mut player, "eve.content.orchestrator.admin.api.SetTenantVersionRequest", set_ten.clone()).await.status_code, STATUS_FORBIDDEN);
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.admin.api.SetTenantVersionRequest", set_ten).await.status_code, STATUS_OK);

        let r_ten = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.api.GetTenantVersionRequest", Vec::new()).await;
        assert_eq!(r_ten.status_code, STATUS_OK);
        let dyn_ten = decode_dyn(&ctx, "eve.content.orchestrator.api.GetTenantVersionResponse", &r_ten.payload.unwrap().value);
        if let Some(DynamicValue::Message(v)) = dyn_ten.get(1) {
            assert_eq!((v.get(1), v.get(2), v.get(3)), (Some(&DynamicValue::Uint32(3)), Some(&DynamicValue::Uint32(1)), Some(&DynamicValue::Uint32(2))));
        } else {
            panic!("expected tenant version message");
        }

        let mut reset_ten = Vec::new();
        encode_varint_field(1, 1, &mut reset_ten);
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.admin.api.SetTenantVersionRequest", reset_ten).await.status_code, STATUS_OK);
        let r_ten_reset = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.api.GetTenantVersionRequest", Vec::new()).await;
        assert_eq!(r_ten_reset.status_code, STATUS_OK);
        let dyn_ten_reset = decode_dyn(&ctx, "eve.content.orchestrator.api.GetTenantVersionResponse", &r_ten_reset.payload.unwrap().value);
        if let Some(DynamicValue::Message(v)) = dyn_ten_reset.get(1) {
            assert_eq!((v.get(1), v.get(2), v.get(3)), (Some(&DynamicValue::Uint32(2)), Some(&DynamicValue::Uint32(3)), Some(&DynamicValue::Uint32(4))));
        } else {
            panic!("expected reset tenant version message");
        }

        // 3. DryRunRulesetRequest (evaluated against real SDE solar systems)
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.api.DryRunRulesetRequest", Vec::new()).await.status_code, STATUS_BAD_REQUEST);
        let mut tag_hs = Vec::new();
        encode_string_field(1, "highsec", &mut tag_hs);
        let mut tag_wh = Vec::new();
        encode_string_field(1, "wormhole", &mut tag_wh);
        let mut ruleset_buf = Vec::new();
        encode_bytes_field(1, &tag_hs, &mut ruleset_buf);
        encode_bytes_field(4, &tag_wh, &mut ruleset_buf);
        let mut dry_req = Vec::new();
        encode_bytes_field(1, &ruleset_buf, &mut dry_req);
        let dry = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.api.DryRunRulesetRequest", dry_req).await;
        assert_eq!(dry.status_code, STATUS_OK);
        let dyn_dry = decode_dyn(&ctx, "eve.content.orchestrator.api.DryRunRulesetResponse", &dry.payload.unwrap().value);
        match dyn_dry.get(1) {
            Some(DynamicValue::Repeated(evals)) => assert!(!evals.is_empty()),
            other => panic!("expected evaluation_result list, got {other:?}"),
        }
        match dyn_dry.get(2) {
            Some(DynamicValue::Repeated(tags_eval)) => assert_eq!(tags_eval.len(), 2),
            other => panic!("expected ruleset_tags_evaluated list, got {other:?}"),
        }

        // 4. Deployment ListRequest, GetRequest, DisableRequest, EnableRequest + role checks
        let dlist = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.api.ListRequest", Vec::new()).await;
        assert_eq!(dlist.status_code, STATUS_OK);
        let dyn_dlist = decode_dyn(&ctx, "eve.content.orchestrator.deployment.api.ListResponse", &dlist.payload.unwrap().value);
        match dyn_dlist.get(1) {
            Some(DynamicValue::Repeated(deps)) => {
                assert_eq!(deps.len(), 1);
                let DynamicValue::Message(dep_msg) = &deps[0] else { panic!("expected Deployment message"); };
                assert_eq!(dep_msg.get(2), Some(&DynamicValue::String("pirate_insurgency_deployment".into())));
            }
            other => panic!("expected deployments list, got {other:?}"),
        }

        let dep_id = seq_bytes(101);
        assert_eq!(quasar_rpc(&ctx, &mut player, "eve.content.orchestrator.deployment.api.DisableRequest", dep_id.clone()).await.status_code, STATUS_FORBIDDEN);
        assert_eq!(quasar_rpc(&ctx, &mut player, "eve.content.orchestrator.deployment.api.EnableRequest", dep_id.clone()).await.status_code, STATUS_FORBIDDEN);

        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.api.DisableRequest", dep_id.clone()).await.status_code, STATUS_OK);
        let dget_dis = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.api.GetRequest", dep_id.clone()).await;
        assert_eq!(dget_dis.status_code, STATUS_OK);
        let dyn_dis = decode_dyn(&ctx, "eve.content.orchestrator.deployment.api.GetResponse", &dget_dis.payload.unwrap().value);
        if let Some(DynamicValue::Message(attr)) = dyn_dis.get(1) {
            assert_eq!(attr.get(7), Some(&DynamicValue::Bool(true)));
        } else {
            panic!("expected deployment attributes message");
        }
        ctx.db.with_snapshot(|s| {
            let st: OrchestratorState = serde_json::from_str(&s.service_state_blobs["content_orchestrator_state"]).unwrap();
            assert!(!st.deployments[&101].enabled);
        });

        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.api.EnableRequest", dep_id.clone()).await.status_code, STATUS_OK);
        let dget_en = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.api.GetRequest", dep_id).await;
        assert_eq!(dget_en.status_code, STATUS_OK);
        let dyn_en = decode_dyn(&ctx, "eve.content.orchestrator.deployment.api.GetResponse", &dget_en.payload.unwrap().value);
        assert!(matches!(dyn_en.get(1), Some(DynamicValue::Message(_))));
        ctx.db.with_snapshot(|s| {
            let st: OrchestratorState = serde_json::from_str(&s.service_state_blobs["content_orchestrator_state"]).unwrap();
            assert!(st.deployments[&101].enabled);
        });

        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.api.GetRequest", seq_bytes(9999)).await.status_code, STATUS_NOT_FOUND);
    }

    #[tokio::test]
    // parity: quasar:eveProto/generated/eve/content/orchestrator/deployment/contentgroup/api/requests_pb2.pyj, quasar:eveProto/generated/eve/content/orchestrator/deployment/contenttype/api/requests_pb2.pyj, quasar:eveProto/generated/eve/content/orchestrator/deployment/instance/api/requests_pb2.pyj, quasar:eveProto/generated/eve/content/orchestrator/deployment/instance/admin/api/requests_pb2.pyj
    async fn test_b29_orchestrator_content_and_instances() {
        let ctx = ServerContext::new();
        seed_test_orchestrator(&ctx);
        let mut sess = make_session(2, 1002, 2112000002, eve_machonet::service_roles::ROLE_GM_DEFAULT);
        let mut player = make_session(20, 2002, 2112000099, eve_machonet::service_roles::ROLE_PLAYER_DEFAULT);

        assert_eq!(quasar_rpc(&ctx, &mut player, "eve.content.orchestrator.deployment.contentgroup.api.GetRequest", seq_bytes(201)).await.status_code, STATUS_FORBIDDEN);
        let cg_resp = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.contentgroup.api.GetRequest", seq_bytes(201)).await;
        assert_eq!(cg_resp.status_code, STATUS_OK);
        let dyn_cg = decode_dyn(&ctx, "eve.content.orchestrator.deployment.contentgroup.api.GetResponse", &cg_resp.payload.unwrap().value);
        if let Some(DynamicValue::Message(attr)) = dyn_cg.get(1) {
            assert_eq!(attr.get(2), Some(&DynamicValue::String("insurgency_sites".into())));
        } else {
            panic!("expected content group attributes");
        }

        let ct_resp = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.contenttype.api.GetRequest", seq_bytes(301)).await;
        assert_eq!(ct_resp.status_code, STATUS_OK);
        let dyn_ct = decode_dyn(&ctx, "eve.content.orchestrator.deployment.contenttype.api.GetResponse", &ct_resp.payload.unwrap().value);
        if let Some(DynamicValue::Message(attr)) = dyn_ct.get(1) {
            assert_eq!(attr.get(1), Some(&DynamicValue::String("guristas_outpost".into())));
        } else {
            panic!("expected content type attributes");
        }

        let ilist = quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.instance.api.ListRequest", seq_bytes(101)).await;
        assert_eq!(ilist.status_code, STATUS_OK);
        let dyn_ilist = decode_dyn(&ctx, "eve.content.orchestrator.deployment.instance.api.ListResponse", &ilist.payload.unwrap().value);
        match dyn_ilist.get(1) {
            Some(DynamicValue::Repeated(insts)) => {
                assert_eq!(insts.len(), 1);
                let DynamicValue::Message(inst_id) = &insts[0] else { panic!("expected instance Identifier"); };
                assert_eq!(inst_id.get(1), Some(&DynamicValue::Bytes(b"TESTORCHINST0001".to_vec())));
            }
            other => panic!("expected instances list, got {other:?}"),
        }

        let inst_req = uuid_bytes(b"TESTORCHINST0001");
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.instance.api.GetRequest", inst_req.clone()).await.status_code, STATUS_OK);
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.instance.admin.api.RemoveRequest", inst_req.clone()).await.status_code, STATUS_OK);
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.content.orchestrator.deployment.instance.api.GetRequest", inst_req).await.status_code, STATUS_NOT_FOUND);
    }

    #[tokio::test]
    // parity: quasar:eveProto/generated/eve/cosmetic/market/component/listing/api/requests_pb2.pyj, quasar:eveProto/generated/eve/cosmetic/market/skin/listing/api/admin/requests_pb2.pyj
    async fn test_b29_cosmetic_market_and_payment_admin() {
        let ctx = ServerContext::new();
        let seller_cid = 90_000_001u32;
        let listing_uuid = *b"TESTSKINLIST0001";
        ctx.db.with_mut_snapshot(|snap| {
            let mut cosm = CosmeticsMarketData::default();
            cosm.skin_listings.insert("54455354534b494e4c49535430303031".into(), SkinListingRecord {
                uuid: listing_uuid,
                seller_id: seller_cid,
                created_unix: 1_700_000_000,
                expires_unix: 1_800_000_000,
                price_isk: Some(1_000_000),
                price_plex_cents: None,
                skin_hex: "deadbeef".into(),
                quantity: 1,
                target: ListingTargetRecord::Public,
                branded: false,
            });
            snap.quasar_state_blobs.insert("quasar_cosmetics_market".into(), serde_json::to_string(&cosm).unwrap());
            snap.accounts.insert("admin_b29".into(), AccountRecord {
                user_id: 1003,
                username: "admin_b29".into(),
                password_token: WorldRepository::hash_password_token("admin_b29", "pw"),
                role: eve_db::service_roles::ROLE_GM_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            });
            let mut ch: CharacterRecord = snap.characters.values().next().unwrap().clone();
            ch.character_id = seller_cid as i64;
            ch.user_id = 1003;
            ch.name = "Valeria Vance".into();
            ch.region_id = 10_000_002;
            snap.characters.insert(seller_cid as i64, ch);
        });

        let mut sess = make_session(3, 1003, seller_cid as i64, eve_machonet::service_roles::ROLE_GM_DEFAULT);
        let mut player = make_session(30, 2003, seller_cid as i64, eve_machonet::service_roles::ROLE_PLAYER_DEFAULT);

        // 1. Component listing GetAllRequest
        assert_eq!(quasar_rpc(&ctx, &mut player, "eve.cosmetic.market.component.listing.api.GetAllRequest", Vec::new()).await.status_code, STATUS_FORBIDDEN);
        let comp_resp = quasar_rpc(&ctx, &mut sess, "eve.cosmetic.market.component.listing.api.GetAllRequest", Vec::new()).await;
        assert_eq!(comp_resp.status_code, STATUS_OK);
        let dyn_comp = decode_dyn(&ctx, "eve.cosmetic.market.component.listing.api.GetAllResponse", &comp_resp.payload.unwrap().value);
        match dyn_comp.get(1) {
            Some(DynamicValue::Repeated(items)) => assert_eq!(items.len(), 4),
            other => panic!("expected 4 SDE component listings, got {other:?}"),
        }

        // 2. Skin listing admin SetExpirationRequest & GetAllListedByCharacterRequest
        let mut exp_req = uuid_bytes(&listing_uuid);
        let mut ts_buf = Vec::new();
        encode_varint_field(1, 1_900_000_000, &mut ts_buf);
        encode_bytes_field(2, &ts_buf, &mut exp_req);
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.cosmetic.market.skin.listing.api.admin.SetExpirationRequest", exp_req).await.status_code, STATUS_OK);

        let by_char_resp = quasar_rpc(&ctx, &mut sess, "eve.cosmetic.market.skin.listing.api.admin.GetAllListedByCharacterRequest", seq_bytes(seller_cid as u64)).await;
        assert_eq!(by_char_resp.status_code, STATUS_OK);
        let dyn_by_char = decode_dyn(&ctx, "eve.cosmetic.market.skin.listing.api.admin.GetAllListedByCharacterResponse", &by_char_resp.payload.unwrap().value);
        let listings = match dyn_by_char.get(1) {
            Some(DynamicValue::Repeated(v)) => v,
            other => panic!("expected skin listings, got {other:?}"),
        };
        assert_eq!(listings.len(), 1);
        if let DynamicValue::Message(entry) = &listings[0] {
            if let Some(DynamicValue::Message(attr)) = entry.get(2) {
                if let Some(DynamicValue::Message(ts)) = attr.get(3) {
                    assert_eq!(ts.get(1), Some(&DynamicValue::Int64(1_900_000_000)));
                } else { panic!("expected expiration timestamp"); }
            } else { panic!("expected listing attributes"); }
        }

        // Verify public cosmetics market GetAllRequest (which triggers CosmeticsMarketState::save()) preserves the admin expiration
        assert_eq!(quasar_rpc(&ctx, &mut player, "eve_public.cosmetic.market.component.listing.api.GetAllRequest", Vec::new()).await.status_code, STATUS_OK);
        let pub_get = quasar_rpc(&ctx, &mut player, "eve_public.cosmetic.market.skin.listing.api.GetRequest", uuid_bytes(&listing_uuid)).await;
        assert_eq!(pub_get.status_code, STATUS_OK);
        let dyn_pub = decode_dyn(&ctx, "eve_public.cosmetic.market.skin.listing.api.GetResponse", &pub_get.payload.unwrap().value);
        if let Some(DynamicValue::Message(listing)) = dyn_pub.get(1) {
            if let Some(DynamicValue::Message(attr)) = listing.get(2) {
                if let Some(DynamicValue::Message(ts)) = attr.get(4) {
                    assert_eq!(ts.get(1), Some(&DynamicValue::Int64(1_900_000_000)));
                } else { panic!("expected public listing expiration timestamp"); }
            } else { panic!("expected public listing attributes"); }
        } else { panic!("expected public listing"); }
    }

    #[tokio::test]
    // parity: quasar:eveProto/generated/eve/store/sale/admin/requests_pb2.pyj, quasar:eveProto/generated/eve/store/targetaudience/offer/api/admin/requests_pb2.pyj
    async fn test_b29_store_sale_and_targeted_offer_admin() {
        let ctx = ServerContext::new();
        ctx.db.with_mut_snapshot(|snap| {
            let mut ff = BTreeMap::new();
            ff.insert(5001u32, FailedFulfillmentRecord { id: 5001, user_id: 1004, plex_units: 250, retries: 0, fulfilled: false });
            ff.insert(5002u32, FailedFulfillmentRecord { id: 5002, user_id: 1004, plex_units: 100, retries: 0, fulfilled: false });
            snap.service_state_blobs.insert("store_failed_fulfillments".into(), serde_json::to_string(&ff).unwrap());
        });
        let mut sess = make_session(4, 1004, 2112000004, eve_machonet::service_roles::ROLE_GM_DEFAULT);

        // 1. Store sale admin RetryFailedFulfillmentRequest, RetryAll, DropFailedFulfillmentRequest
        let fid_req = seq_bytes(5001);
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.store.sale.admin.RetryFailedFulfillmentRequest", fid_req.clone()).await.status_code, STATUS_OK);
        assert_eq!(ctx.db.with_snapshot(|s| s.account_plex_vault.get(&1004).copied()), Some(250));

        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.store.sale.admin.RetryAllFailedFulfillmentsRequest", Vec::new()).await.status_code, STATUS_OK);
        ctx.db.with_snapshot(|s| {
            assert_eq!(s.account_plex_vault.get(&1004).copied(), Some(350));
            assert!(get_plex_transactions(s).len() >= 2);
        });

        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.store.sale.admin.DropFailedFulfillmentRequest", fid_req.clone()).await.status_code, STATUS_OK);
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.store.sale.admin.DropFailedFulfillmentRequest", fid_req).await.status_code, STATUS_NOT_FOUND);

        // 2. Targeted offer admin PromoteRequest, RetractRequest, BackfillPartnerCodeRequest
        let mut user_sub = Vec::new();
        encode_varint_field(1, (((1004i32 << 1) ^ (1004i32 >> 31)) as u32) as u64, &mut user_sub);
        let mut cat_sub = Vec::new();
        encode_string_field(1, "cat-entry-7001", &mut cat_sub);
        let mut offer_id = Vec::new();
        encode_bytes_field(1, &user_sub, &mut offer_id);
        encode_bytes_field(2, &cat_sub, &mut offer_id);

        let mut prom_req = Vec::new();
        encode_bytes_field(1, &offer_id, &mut prom_req);
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.store.targetaudience.offer.api.admin.PromoteRequest", prom_req.clone()).await.status_code, STATUS_OK);
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.store.targetaudience.offer.api.admin.RetractRequest", prom_req).await.status_code, STATUS_OK);

        let mut bf_req = sint32_user_bytes(1004);
        encode_bytes_field(3, &offer_id, &mut bf_req);
        encode_string_field(4, "PARTNER-B29", &mut bf_req);
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.store.targetaudience.offer.api.admin.BackfillPartnerCodeRequest", bf_req).await.status_code, STATUS_OK);
        ctx.db.with_snapshot(|s| {
            let offers: BTreeMap<String, TargetedOfferAdminRecord> = s.service_state_blobs.get("store_targeted_offers").and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default();
            let rec = offers.get("1004:cat-entry-7001").expect("targeted offer persisted");
            assert!(rec.promoted && rec.retracted && rec.partner_code == "PARTNER-B29");
        });

        let bad_bf = quasar_rpc(&ctx, &mut sess, "eve.store.targetaudience.offer.api.admin.BackfillPartnerCodeRequest", Vec::new()).await;
        assert_eq!((bad_bf.status_code, bad_bf.status_message.as_str()), (STATUS_BAD_REQUEST, "user and partner_code required"));
    }
}

pub(crate) mod faction_activity_admin_payment_schedule {
    // Live `publicGatewaySvc` integration tests for Batch 33 Quasar services.

    use eve_db::{
        corp_roles, FwSystemOccupationRecord, PersistedMercenaryDen, PersistedSovHubData,
        UpwellStructureRecord,
    };
    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        encode_bytes_field, encode_varint_field, DynamicQuasarMessage, DynamicValue, EvePublicRequest,
        EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::{
        STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK, STATUS_UNAUTHORIZED,
    };
    use eve_server::router::ServerContext;
    use eve_server::services::quasar_payment_store_types::PaymentScheduleRecord;
    use eve_server::services::sov_services::{
        TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB,
    };
    use std::collections::HashMap;

    fn create_user_payment_schedule(
        db: &eve_db::WorldRepository,
        user_id: u64,
        schedule_type: u32,
        offer_id: &str,
        interval_days: u32,
        now_unix: u64,
    ) -> PaymentScheduleRecord {
        db.with_mut_snapshot(|snap| {
            let mut schedules: Vec<PaymentScheduleRecord> = snap
                .service_state_blobs
                .get("payment_schedules")
                .and_then(|json| serde_json::from_str(json).ok())
                .unwrap_or_default();
            let next_id = schedules.iter().map(|s| s.id).max().unwrap_or(1000) + 1;
            let rec = PaymentScheduleRecord {
                id: next_id,
                user_id,
                schedule_type,
                offer_id: offer_id.to_string(),
                interval_days,
                next_payment_unix: now_unix + u64::from(interval_days) * 86_400,
                created_unix: now_unix,
            };
            schedules.push(rec.clone());
            if let Ok(json) = serde_json::to_string(&schedules) {
                snap.service_state_blobs.insert("payment_schedules".to_string(), json);
            }
            rec
        })
    }

    fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64, admin: bool) -> SessionState {
        let mut sess = SessionState::new(sid);
        sess.user_id = Some(uid);
        sess.character_id = Some(cid);
        sess.corporation_id = Some(corp_id);
        sess.corp_role = corp_roles::ROLE_DIRECTOR;
        sess.solar_system_id = Some(30_000_142);
        sess.role = if admin {
            eve_machonet::service_roles::ROLE_GM_DEFAULT
        } else {
            eve_machonet::service_roles::ROLE_PLAYER
        };
        sess
    }

    fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
        if let PyRep::KeyVal(entries) = rep {
            for (k, v) in entries {
                if k == "response_proto" {
                    if let PyRep::Bytes(b) = v {
                        return EvePublicResponse::decode_proto(b)
                            .expect("decode EvePublicResponse from response_proto");
                    }
                }
            }
        }
        panic!("Expected KeyVal containing response_proto, got {rep:?}");
    }

    async fn quasar_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        type_name: &str,
        payload_bytes: Vec<u8>,
    ) -> EvePublicResponse {
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
            correlation_uuid: b"b33-closure-corr".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
                value: payload_bytes,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"app-b33-test".to_vec(),
            authoritative_context: None,
            trace_context: None,
        };
        let rep = ctx
            .dispatch_rpc(
                sess,
                "publicGatewaySvc",
                None,
                "SendRequest",
                &[PyRep::Bytes(req.encode_proto())],
                &HashMap::new(),
                None,
            )
            .await
            .expect("SendRequest");
        extract_response_proto(&rep)
    }

    fn encode_u64_id(field_num: u32, id: u64, out: &mut Vec<u8>) {
        let mut sub = Vec::new();
        encode_varint_field(1, id, &mut sub);
        encode_bytes_field(field_num, &sub, out);
    }

    fn encode_uuid_id(field_num: u32, uuid: &[u8], out: &mut Vec<u8>) {
        let mut sub = Vec::new();
        encode_bytes_field(1, uuid, &mut sub);
        encode_bytes_field(field_num, &sub, out);
    }

    fn encode_ach_uuid_ident(field_no: u32, id: u32, out: &mut Vec<u8>) {
        let mut uuid = [0u8; 16];
        uuid[12..16].copy_from_slice(&id.to_be_bytes());
        encode_uuid_id(field_no, &uuid, out);
    }

    fn decode_dyn(ctx: &ServerContext, rsp: &EvePublicResponse) -> DynamicQuasarMessage {
        let any = rsp.payload.as_ref().expect("response payload");
        ctx.quasar_gateway
            .descriptors
            .decode_dynamic(any.normalized_type_name(), &any.value)
            .expect("decode DynamicQuasarMessage")
    }

    fn seed_char(
        snap: &mut eve_db::WorldStateSnapshot,
        cid: i64,
        uid: u64,
        corp_id: i64,
        sys_id: u32,
    ) {
        let mut ch = snap
            .characters
            .values()
            .next()
            .cloned()
            .expect("at least one seeded character");
        ch.character_id = cid;
        ch.user_id = uid;
        ch.name = format!("Pilot_{cid}");
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(cid, ch);
    }

    async fn subscribe_notices(
        ctx: &ServerContext,
        sess: &mut SessionState,
    ) -> tokio::sync::mpsc::Receiver<eve_proto::quasar::EvePublicNotice> {
        ctx.dispatch_rpc(sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
            .await
            .expect("Subscribe");
        ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("stream rx")
    }

    fn drain_notice_names(
        rx: &mut tokio::sync::mpsc::Receiver<eve_proto::quasar::EvePublicNotice>,
    ) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(n) = rx.try_recv() {
            if let Some(p) = n.payload {
                out.push(p.normalized_type_name().to_string());
            }
        }
        out
    }

    fn seed_structure(snap: &mut eve_db::WorldStateSnapshot, id: i64, owner: i64, sys: u32, type_id: u32) {
        snap.structures.insert(id, UpwellStructureRecord {
            structure_id: id, type_id, owner_corp_id: owner, solar_system_id: sys,
            name: format!("Structure {id}"), position: [200_000.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(), quantum_core_installed: true, fuel_blocks: 0,
            shield_hp: 5_000.0, armor_hp: 5_000.0, hull_hp: 5_000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
    }

    #[tokio::test]
    async fn test_b33_faction_activity_admin_and_payment_schedule() {
        // parity: quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj, quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj
        let ctx = ServerContext::new();
        let mut admin_sess = make_session(1, 10, 90_000_010, 1_000_009, true);
        let mut player_sess = make_session(2, 11, 90_000_011, 1_000_009, false);
        let mut unauth_sess = SessionState::new(3);

        let fw_sys = 30_002_056u32; // Huola
        admin_sess.solar_system_id = Some(fw_sys);
        ctx.db.with_mut_snapshot(|snap| {
            snap.fw_system_occupations.insert(
                fw_sys,
                FwSystemOccupationRecord {
                    solar_system_id: fw_sys,
                    owner_faction_id: 500_003,
                    occupier_faction_id: 500_003,
                    victory_points: 100,
                    victory_points_threshold: 3_000,
                },
            );
        });

        // 1. Unauthenticated & non-admin error cases
        let mut inc_req = Vec::new();
        encode_u64_id(1, u64::from(fw_sys), &mut inc_req);
        encode_u64_id(2, 500_003, &mut inc_req);
        encode_varint_field(3, 250, &mut inc_req);

        let r_unauth = quasar_rpc(&ctx, &mut unauth_sess, "eve_public.faction.activity.admin.IncrementContributionScoreRequest", inc_req.clone()).await;
        assert_eq!(r_unauth.status_code, STATUS_UNAUTHORIZED);

        let r_forbid = quasar_rpc(&ctx, &mut player_sess, "eve_public.faction.activity.admin.IncrementContributionScoreRequest", inc_req.clone()).await;
        assert_eq!(r_forbid.status_code, STATUS_FORBIDDEN);

        // 2. Unknown / non-warzone system -> 400 or 404
        let mut bad_sys = Vec::new();
        encode_u64_id(1, 99_999_999, &mut bad_sys);
        let r_not_found = quasar_rpc(&ctx, &mut admin_sess, "eve_public.faction.activity.admin.IncrementContributionScoreRequest", bad_sys).await;
        assert!(matches!(r_not_found.status_code, STATUS_BAD_REQUEST | STATUS_NOT_FOUND));

        // 3. Valid increment mutates victory points & emits notices
        let mut rx = subscribe_notices(&ctx, &mut admin_sess).await;

        let r_inc = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
            inc_req,
        )
        .await;
        assert_eq!(r_inc.status_code, STATUS_OK);
        let vp_after = ctx
            .db
            .with_snapshot(|s| s.fw_system_occupations.get(&fw_sys).unwrap().victory_points);
        assert_eq!(vp_after, 350);

        let notices = drain_notice_names(&mut rx);
        assert!(notices.contains(&"eve_public.faction.activity.ScoreContributionNotice".to_string()));
        assert!(notices.contains(&"eve_public.faction.activity.SolarSystemScoresNotice".to_string()));

        // 4. SetContributionScoreRequest overrides victory points
        let mut set_req = Vec::new();
        encode_u64_id(1, u64::from(fw_sys), &mut set_req);
        encode_u64_id(2, 500_002, &mut set_req);
        encode_varint_field(3, 1_500, &mut set_req);
        let r_set = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve_public.faction.activity.admin.SetContributionScoreRequest",
            set_req,
        )
        .await;
        assert_eq!(r_set.status_code, STATUS_OK);
        let (vp_set, occ_fac) = ctx.db.with_snapshot(|s| {
            let o = s.fw_system_occupations.get(&fw_sys).unwrap();
            (o.victory_points, o.occupier_faction_id)
        });
        assert_eq!(vp_set, 1_500);
        assert_eq!(occ_fac, 500_002);

        // 5. Payment schedule GetRequest & GetActiveByTypeRequest
        let sched = create_user_payment_schedule(&ctx.db, 10, 1, "omega-30d", 30, ctx.now_unix());
        let mut get_sched = Vec::new();
        encode_u64_id(1, sched.id, &mut get_sched);
        let r_sched = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve_public.payment.schedule.api.GetRequest",
            get_sched,
        )
        .await;
        assert_eq!(r_sched.status_code, STATUS_OK);
        let dyn_sched = decode_dyn(&ctx, &r_sched);
        match dyn_sched.get(1) {
            Some(DynamicValue::Message(attr)) => {
                assert_eq!(attr.get(4), Some(&DynamicValue::Uint32(30)));
            }
            other => panic!("Expected attributes message, got {other:?}"),
        }

        let mut by_type = Vec::new();
        encode_varint_field(1, 1, &mut by_type);
        let r_type = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve_public.payment.schedule.api.GetActiveByTypeRequest",
            by_type,
        )
        .await;
        assert_eq!(r_type.status_code, STATUS_OK);
        let dyn_type = decode_dyn(&ctx, &r_type);
        match dyn_type.get(1) {
            Some(DynamicValue::Message(ident)) => {
                assert_eq!(ident.get(1), Some(&DynamicValue::Uint64(sched.id)));
            }
            other => panic!("Expected schedule identifier, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_b33_achievement_and_goal_admin_closure() {
        // parity: quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj, quasar:eveProto/generated/eve_public/career/goal/api/admin/requests_pb2.pyj, quasar:eveProto/generated/eve_public/dailygoal/api/admin/admin_pb2.pyj
        let ctx = ServerContext::new();
        let cid = 90_000_020i64;
        let mut admin_sess = make_session(4, 20, cid, 1_000_009, true);
        let mut player_sess = make_session(5, 21, 90_000_021, 1_000_009, false);

        // 1. Achievement admin: validation + AddProgressRequest + CompleteRequest + ResetRequest
        let aid = ctx
            .quasar_gateway
            .progression
            .achievements
            .definitions
            .first()
            .expect("at least one achievement definition")
            .id;

        let mut add_ach = Vec::new();
        encode_ach_uuid_ident(1, aid, &mut add_ach);
        encode_varint_field(2, 5, &mut add_ach);

        let r_forbid = quasar_rpc(
            &ctx,
            &mut player_sess,
            "eve_public.achievement.api.admin.AddProgressRequest",
            add_ach.clone(),
        )
        .await;
        assert_eq!(r_forbid.status_code, STATUS_FORBIDDEN);

        let mut bad_ach = Vec::new();
        encode_ach_uuid_ident(1, 0, &mut bad_ach);
        let r_bad = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve_public.achievement.api.admin.AddProgressRequest",
            bad_ach,
        )
        .await;
        assert_eq!(r_bad.status_code, STATUS_BAD_REQUEST);

        let r_ok = quasar_rpc(&ctx, &mut admin_sess, "eve_public.achievement.api.admin.AddProgressRequest", add_ach).await;
        assert_eq!(r_ok.status_code, STATUS_OK);
        let ach_prog = ctx.quasar_gateway.progression.achievements.get_or_init_achievements(cid as u32)
            .into_iter().find(|a| a.def.id == aid).unwrap().progress;
        assert_eq!(ach_prog, 5);

        let mut comp_ach = Vec::new();
        encode_ach_uuid_ident(1, aid, &mut comp_ach);
        let r_comp = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve_public.achievement.api.admin.CompleteRequest",
            comp_ach.clone(),
        )
        .await;
        assert_eq!(r_comp.status_code, STATUS_OK);

        let r_reset = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve_public.achievement.api.admin.ResetRequest",
            comp_ach,
        )
        .await;
        assert_eq!(r_reset.status_code, STATUS_OK);

        let r_reset_all = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve_public.achievement.api.admin.ResetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(r_reset_all.status_code, STATUS_OK);

        // 2. Career goal admin: ProgressRequest + CompleteRequest
        let career_goals = ctx
            .quasar_gateway
            .progression
            .career_goals
            .get_or_init_goals(cid as u32);
        let cg_uuid = career_goals.first().expect("career goal").uuid;

        let mut prog_cg = Vec::new();
        encode_uuid_id(1, &cg_uuid, &mut prog_cg);
        encode_varint_field(2, 10, &mut prog_cg);
        let r_cg = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve_public.career.goal.api.admin.ProgressRequest",
            prog_cg,
        )
        .await;
        assert_eq!(r_cg.status_code, STATUS_OK);

        let mut comp_cg = Vec::new();
        encode_uuid_id(1, &cg_uuid, &mut comp_cg);
        let r_cg_comp = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve_public.career.goal.api.admin.CompleteRequest",
            comp_cg,
        )
        .await;
        assert_eq!(r_cg_comp.status_code, STATUS_OK);

        // 3. Daily goal admin: ProgressRequest
        let daily_goals = ctx
            .quasar_gateway
            .progression
            .daily_goals
            .get_or_init_goals_at(cid as u32, ctx.now_unix());
        let dg_uuid = daily_goals.first().expect("daily goal").uuid;

        let mut prog_dg = Vec::new();
        encode_uuid_id(1, &dg_uuid, &mut prog_dg);
        encode_u64_id(2, cid as u64, &mut prog_dg);
        encode_varint_field(3, 25, &mut prog_dg);
        let r_dg = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve_public.dailygoal.api.admin.ProgressRequest",
            prog_dg,
        )
        .await;
        assert_eq!(r_dg.status_code, STATUS_OK);
    }

    #[tokio::test]
    async fn test_b33_sovereignty_hub_resources_and_fuel_closure() {
        // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/hub/fuel/api/requests_pb2.pyj
        let ctx = ServerContext::new();
        let cid = 90_000_030i64;
        let corp_id = 1_000_009i64;
        let hub_id = 1_050_000_001i64;
        let null_sys = 30_004_759u32; // 1DQ1-A
        let mut sess = make_session(6, 30, cid, corp_id, false);
        sess.solar_system_id = Some(null_sys);

        let fuel_item_id = 9_900_001i64;
        ctx.db.with_mut_snapshot(|snap| {
            seed_char(snap, cid, 30, corp_id, null_sys);
            seed_structure(snap, hub_id, corp_id, null_sys, TYPE_SOVEREIGNTY_HUB);
            snap.sov_runtime
                .sov_hubs
                .insert(hub_id, PersistedSovHubData::default());
            snap.items.insert(
                fuel_item_id,
                eve_db::InventoryItemRecord {
                    item_id: fuel_item_id,
                    type_id: TYPE_MAGMATIC_GAS,
                    owner_id: corp_id,
                    location_id: hub_id,
                    quantity: 1_000,
                    singleton: false,
                    ..Default::default()
                },
            );
        });

        // 1. GetResourcesRequest
        let mut res_req = Vec::new();
        encode_u64_id(1, hub_id as u64, &mut res_req);
        let r_res = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.sovereignty.hub.api.GetResourcesRequest",
            res_req.clone(),
        )
        .await;
        assert_eq!(r_res.status_code, STATUS_OK);
        let dyn_res = decode_dyn(&ctx, &r_res);
        assert!(dyn_res.get(1).is_some());
        assert!(dyn_res.get(2).is_some());

        // 2. Fuel AddRequest & GetRequest
        let mut add_fuel = Vec::new();
        encode_u64_id(1, fuel_item_id as u64, &mut add_fuel);
        encode_varint_field(2, 500, &mut add_fuel);
        encode_u64_id(3, hub_id as u64, &mut add_fuel);
        let r_add = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.sovereignty.hub.fuel.api.AddRequest",
            add_fuel,
        )
        .await;
        assert_eq!(r_add.status_code, STATUS_OK);

        let r_fuel = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.sovereignty.hub.fuel.api.GetRequest",
            res_req,
        )
        .await;
        assert_eq!(r_fuel.status_code, STATUS_OK);
        let fuel_qty = ctx.db.with_snapshot(|s| {
            s.sov_runtime
                .sov_hubs
                .get(&hub_id)
                .and_then(|h| h.fuel.get(&TYPE_MAGMATIC_GAS).copied())
                .unwrap_or(0)
        });
        assert_eq!(fuel_qty, 500);
    }

    #[tokio::test]
    async fn test_b33_mercenary_den_activities_and_notices_closure() {
        // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj
        let ctx = ServerContext::new();
        let cid = 90_000_040i64;
        let corp_id = 1_000_009i64;
        let den_id = 1_060_000_001i64;
        let null_sys = 30_004_759u32;
        let mut sess = make_session(7, 40, cid, corp_id, false);
        sess.solar_system_id = Some(null_sys);

        let now = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            seed_char(snap, cid, 40, corp_id, null_sys);
            snap.sov_runtime.mercenary_dens.insert(
                den_id,
                PersistedMercenaryDen {
                    den_id,
                    type_id: 85_230,
                    owner_char_id: cid,
                    skyhook_id: 1_070_000_099,
                    solar_system_id: null_sys,
                    planet_id: 40_301_001,
                    enabled: true,
                    cargo_extraction_enabled: true,
                    development_level: 10,
                    anarchy_level: 0,
                    infomorphs: 50,
                    simulated_at_unix: now,
                    next_activity_generation_unix: 0,
                },
            );
        });

        // 1. GetCapacityRequest
        let r_cap = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(r_cap.status_code, STATUS_OK);
        let dyn_cap = decode_dyn(&ctx, &r_cap);
        assert!(matches!(dyn_cap.get(1), Some(DynamicValue::Uint32(c)) if *c > 0));

        // 2. GetForMercenaryDenRequest generates activities & emits AddedNotice
        let mut rx = subscribe_notices(&ctx, &mut sess).await;

        let mut get_den = Vec::new();
        encode_u64_id(1, den_id as u64, &mut get_den);
        let r_acts = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest",
            get_den,
        )
        .await;
        assert_eq!(r_acts.status_code, STATUS_OK);

        let act_uuid = ctx.db.with_snapshot(|s| {
            s.sov_runtime
                .mercenary_activities
                .values()
                .find(|a| a.den_id == den_id)
                .expect("generated activity")
                .uuid
                .clone()
        });
        let notices = drain_notice_names(&mut rx);
        assert!(notices.contains(
            &"eve_public.sovereignty.mercenaryden.activity.api.AddedNotice".to_string()
        ));

        // 3. GetAllRequest
        let r_all = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(r_all.status_code, STATUS_OK);

        // 4. StartRequest mutates activity state & emits StartedNotice + ExpiryChangedNotice
        let mut start_req = Vec::new();
        encode_uuid_id(1, &act_uuid, &mut start_req);
        let r_start = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.sovereignty.mercenaryden.activity.api.StartRequest",
            start_req,
        )
        .await;
        assert_eq!(r_start.status_code, STATUS_OK);
        let started = ctx.db.with_snapshot(|s| {
            s.sov_runtime
                .mercenary_activities
                .values()
                .any(|a| a.den_id == den_id && a.started)
        });
        assert!(started);
    }

    #[tokio::test]
    async fn test_b33_skyhook_admin_closure() {
        // parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj
        let ctx = ServerContext::new();
        let (cid, corp_id, skyhook_id, sys_id) = (90_000_050i64, 1_000_009i64, 1_070_000_001i64, 30_000_142u32);
        let mut player_sess = make_session(9, 50, cid, corp_id, false);
        let mut admin_sess = make_session(8, 50, cid, corp_id, true);

        ctx.db.with_mut_snapshot(|snap| {
            seed_char(snap, cid, 50, corp_id, sys_id);
            seed_structure(snap, skyhook_id, corp_id, sys_id, TYPE_ORBITAL_SKYHOOK);
        });

        // Non-admin player Director must be rejected with STATUS_FORBIDDEN on both admin endpoints
        let mut mod_req = Vec::new();
        encode_u64_id(1, skyhook_id as u64, &mut mod_req);
        let r_mod_forbidden = quasar_rpc(&ctx, &mut player_sess, "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest", mod_req.clone()).await;
        assert_eq!(r_mod_forbidden.status_code, STATUS_FORBIDDEN);

        let now = ctx.now_unix();
        let mut ts_start = Vec::new();
        encode_varint_field(1, now + 60, &mut ts_start);
        let mut ts_end_ok = Vec::new();
        encode_varint_field(1, now + 3_660, &mut ts_end_ok);
        let mut good_win = Vec::new();
        encode_u64_id(1, skyhook_id as u64, &mut good_win);
        encode_bytes_field(2, &ts_start, &mut good_win);
        encode_bytes_field(3, &ts_end_ok, &mut good_win);

        let r_win_forbidden = quasar_rpc(&ctx, &mut player_sess, "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest", good_win.clone()).await;
        assert_eq!(r_win_forbidden.status_code, STATUS_FORBIDDEN);

        // Admin caller succeeds on ModifyReagentsRequest & ScheduleTheftVulnerabilityWindowRequest
        let r_mod_ok = quasar_rpc(&ctx, &mut admin_sess, "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest", mod_req).await;
        assert_eq!(r_mod_ok.status_code, STATUS_OK);
        assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagents.get(&skyhook_id).copied()), Some((TYPE_MAGMATIC_GAS, 1200)));

        let mut bad_win = Vec::new();
        encode_u64_id(1, skyhook_id as u64, &mut bad_win);
        encode_bytes_field(2, &ts_start, &mut bad_win);
        let mut ts_end_bad = Vec::new();
        encode_varint_field(1, now + 200_000, &mut ts_end_bad);
        encode_bytes_field(3, &ts_end_bad, &mut bad_win);
        assert_eq!(quasar_rpc(&ctx, &mut admin_sess, "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest", bad_win).await.status_code, STATUS_BAD_REQUEST);

        let r_good_win = quasar_rpc(&ctx, &mut admin_sess, "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest", good_win).await;
        assert_eq!(r_good_win.status_code, STATUS_OK);
        assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_theft_windows.get(&skyhook_id).copied()), Some((now + 60, now + 3_660)));
    }
}

pub(crate) mod daily_goal_admin_events {
    // Integration tests for Batch 34 Quasar Gateway Services:
    // - `eve.dailygoal.api` & `eve.dailygoal.api.admin`
    // - `eve.loyaltypoints.wallet` & `eve.loyaltypoints.assetholding.operation.api`
    // - `eve.solarsystem.bounty.npc` & `eve.solarsystem.bounty.bank.*`


    use crate::common;
    use eve_db::IskCents;
    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        encode_bytes_field, encode_varint_field, DynamicValue, EvePublicNotice, EvePublicRequest,
        EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage,
        TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::{
        STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_NOT_IMPLEMENTED, STATUS_OK,
        STATUS_PRECONDITION_FAILED,
    };
    use eve_server::router::ServerContext;
    use std::collections::HashMap;
    use tokio::sync::mpsc::Receiver;

    fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
        if let PyRep::KeyVal(entries) = rep {
            for (k, v) in entries {
                if k == "response_proto" {
                    if let PyRep::Bytes(b) = v {
                        return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                    }
                }
            }
        }
        panic!("Expected KeyVal with response_proto, got {rep:?}");
    }

    async fn quasar_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        type_name: &str,
        value: Vec<u8>,
    ) -> EvePublicResponse {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: ctx.now_unix() as i64,
                nanos: 0,
            }),
            correlation_uuid: format!("b34-req-{n:06}").into_bytes(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
                value,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"b34-instance-01".to_vec(),
            authoritative_context: None,
            trace_context: None,
        };
        let rep = ctx
            .dispatch_rpc(
                sess,
                "publicGatewaySvc",
                None,
                "SendRequest",
                &[PyRep::Bytes(req.encode_proto())],
                &HashMap::new(),
                None,
            )
            .await
            .expect("SendRequest");
        extract_response_proto(&rep)
    }

    fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<GoogleAny> {
        let mut out = Vec::new();
        while let Ok(n) = rx.try_recv() {
            if let Some(p) = n.payload {
                out.push(p);
            }
        }
        out
    }

    fn make_uuid_req(uuid: [u8; 16]) -> Vec<u8> {
        let mut id_bytes = Vec::new();
        encode_bytes_field(1, &uuid, &mut id_bytes);
        let mut out = Vec::new();
        encode_bytes_field(1, &id_bytes, &mut out);
        out
    }

    fn make_admin_progress_req(uuid: [u8; 16], cid: u32, delta: u64) -> Vec<u8> {
        let mut out = Vec::new();
        let mut id_bytes = Vec::new();
        encode_bytes_field(1, &uuid, &mut id_bytes);
        encode_bytes_field(1, &id_bytes, &mut out);
        let mut c_bytes = Vec::new();
        encode_varint_field(1, u64::from(cid), &mut c_bytes);
        encode_bytes_field(2, &c_bytes, &mut out);
        encode_varint_field(3, delta, &mut out);
        out
    }

    fn make_admin_expire_req(uuid: [u8; 16], cid: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut id_bytes = Vec::new();
        encode_bytes_field(1, &uuid, &mut id_bytes);
        encode_bytes_field(1, &id_bytes, &mut out);
        let mut c_bytes = Vec::new();
        encode_varint_field(1, u64::from(cid), &mut c_bytes);
        encode_bytes_field(2, &c_bytes, &mut out);
        out
    }

    fn make_lp_op_req(op_uuid: [u8; 16], cid: Option<u32>, corp_id: Option<u32>, issuer: u32, amount: u64) -> Vec<u8> {
        let mut out = Vec::new();
        let mut id_bytes = Vec::new();
        encode_bytes_field(1, &op_uuid, &mut id_bytes);
        encode_bytes_field(1, &id_bytes, &mut out);

        let mut wallet_bytes = Vec::new();
        if let Some(c) = cid {
            let mut c_bytes = Vec::new();
            encode_varint_field(1, u64::from(c), &mut c_bytes);
            encode_bytes_field(1, &c_bytes, &mut wallet_bytes);
        } else if let Some(corp) = corp_id {
            let mut c_bytes = Vec::new();
            encode_varint_field(1, u64::from(corp), &mut c_bytes);
            encode_bytes_field(2, &c_bytes, &mut wallet_bytes);
        }
        let mut issuer_bytes = Vec::new();
        encode_varint_field(1, u64::from(issuer), &mut issuer_bytes);
        encode_bytes_field(3, &issuer_bytes, &mut wallet_bytes);

        let mut curr_bytes = Vec::new();
        encode_varint_field(1, amount, &mut curr_bytes);

        let mut attr_bytes = Vec::new();
        encode_bytes_field(1, &wallet_bytes, &mut attr_bytes);
        encode_bytes_field(2, &curr_bytes, &mut attr_bytes);
        encode_bytes_field(2, &attr_bytes, &mut out);
        out
    }

    // ============================================================================
    // 1. Daily Goal Admin, API & Events Closure
    // ============================================================================

    #[tokio::test]
    async fn test_b34_daily_goal_admin_and_events_closure() {
        // parity: quasar:eveProto/generated/eve/dailygoal/api/admin/requests_pb2.pyj, quasar:eveProto/generated/eve/dailygoal/api/api_pb2.pyj, quasar:eveProto/generated/eve/dailygoal/api/events_pb2.pyj
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");

        let mut unpriv_sess = SessionState::new(1001);
        unpriv_sess.user_id = Some(1);
        unpriv_sess.character_id = Some(90_000_001);

        let mut admin_sess = SessionState::new(1002);
        admin_sess.user_id = Some(2);
        admin_sess.character_id = Some(90_000_001);
        admin_sess.role = eve_machonet::service_roles::ROLE_GM_MASK;
        let mut rx = ctx
            .quasar_gateway
            .subscribe_session(&admin_sess, Vec::new(), admin_sess.session_id);

        // Unprivileged admin call must be rejected with STATUS_FORBIDDEN
        let forb = quasar_rpc(
            &ctx,
            &mut unpriv_sess,
            "eve.dailygoal.api.admin.RegenerateCurrentGoalsRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(forb.status_code, STATUS_FORBIDDEN);

        // Admin session with no character_id must be rejected without touching character 1 (B34-F-08)
        let mut no_char_admin = SessionState::new(1003);
        no_char_admin.user_id = Some(3);
        no_char_admin.role = eve_machonet::service_roles::ROLE_GM_MASK;
        let r = quasar_rpc(
            &ctx,
            &mut no_char_admin,
            "eve.dailygoal.api.admin.RegenerateCurrentGoalsRequest",
            Vec::new(),
        )
        .await;
        assert!(r.status_code == STATUS_BAD_REQUEST || r.status_code == STATUS_FORBIDDEN);

        // Admin RegenerateCurrentGoalsRequest
        let regen_rsp = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve.dailygoal.api.admin.RegenerateCurrentGoalsRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(regen_rsp.status_code, STATUS_OK);
        let regen_dyn = reg
            .decode_dynamic(
                "eve.dailygoal.api.admin.RegenerateCurrentGoalsResponse",
                &regen_rsp.payload.unwrap().value,
            )
            .expect("decode RegenerateCurrentGoalsResponse");
        assert!(regen_dyn.get(1).is_some(), "RegenerateCurrentGoalsResponse must contain goals");

        let notices = drain_notices(&mut rx);
        assert!(
            notices.iter().any(|n| n.normalized_type_name() == "eve.dailygoal.api.PeriodRegenerated"),
            "Expected PeriodRegenerated notice"
        );
        assert!(
            notices.iter().any(|n| n.normalized_type_name() == "eve.dailygoal.api.Created"),
            "Expected Created notice"
        );

        // Monthly regeneration returns STATUS_NOT_IMPLEMENTED
        let m_rsp = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve.dailygoal.api.admin.RegenerateCurrentMonthlyGoalsRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(m_rsp.status_code, STATUS_NOT_IMPLEMENTED);

        // Query goals via eve.dailygoal.api.GetAllRequest & GetRequest
        let all_rsp = quasar_rpc(&ctx, &mut admin_sess, "eve.dailygoal.api.GetAllRequest", Vec::new()).await;
        assert_eq!(all_rsp.status_code, STATUS_OK);
        let all_dyn = reg
            .decode_dynamic("eve.dailygoal.api.GetAllResponse", &all_rsp.payload.unwrap().value)
            .expect("decode GetAllResponse");
        let uuids: Vec<[u8; 16]> = match all_dyn.get(1) {
            Some(DynamicValue::Repeated(items)) => items
                .iter()
                .filter_map(|v| match v {
                    DynamicValue::Message(id_msg) => match id_msg.get(1) {
                        Some(DynamicValue::Bytes(b)) => b.as_slice().try_into().ok(),
                        _ => None,
                    },
                    DynamicValue::Bytes(b) => {
                        let id_msg = reg.decode_dynamic("eve.dailygoal.Identifier", b).ok()?;
                        match id_msg.get(1) {
                            Some(DynamicValue::Bytes(b)) => b.as_slice().try_into().ok(),
                            _ => None,
                        }
                    }
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        assert_eq!(uuids.len(), 6, "Expected exact 6 canonical daily goals (5 daily + 1 daily bonus)");
        let g1 = uuids[0];
        let g2 = uuids[1];

        let get_rsp = quasar_rpc(&ctx, &mut admin_sess, "eve.dailygoal.api.GetRequest", make_uuid_req(g1)).await;
        assert_eq!(get_rsp.status_code, STATUS_OK);

        // Admin ProgressRequest -> Progressed + Completed -> AutoRedeemRequest -> Redeemed
        let prog_rsp = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve.dailygoal.api.admin.ProgressRequest",
            make_admin_progress_req(g1, 90_000_001, 10_000),
        )
        .await;
        assert_eq!(prog_rsp.status_code, STATUS_OK);
        let prog_notices = drain_notices(&mut rx);
        assert!(prog_notices.iter().any(|n| n.normalized_type_name() == "eve.dailygoal.api.Progressed"));
        assert!(prog_notices.iter().any(|n| n.normalized_type_name() == "eve.dailygoal.api.Completed"));

        let gp_rsp = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve.dailygoal.api.GetProgressRequest",
            make_uuid_req(g1),
        )
        .await;
        assert_eq!(gp_rsp.status_code, STATUS_OK);
        let gp_dyn = reg
            .decode_dynamic("eve.dailygoal.api.GetProgressResponse", &gp_rsp.payload.unwrap().value)
            .expect("decode GetProgressResponse");
        assert!(gp_dyn.get(4).is_some(), "Completed goal must have completed timestamp (field 4)");

        let auto_rsp = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve.dailygoal.api.admin.AutoRedeemRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(auto_rsp.status_code, STATUS_OK);
        let auto_notices = drain_notices(&mut rx);
        assert!(auto_notices.iter().any(|n| n.normalized_type_name() == "eve.dailygoal.api.Redeemed"));

        // Complete g2, then Admin ExpireRequest -> PrematurelyDeactivated + Removed, then RedeemRequest must fail
        let p2_rsp = quasar_rpc(&ctx, &mut admin_sess, "eve.dailygoal.api.admin.ProgressRequest", make_admin_progress_req(g2, 90_000_001, 10_000)).await;
        assert_eq!(p2_rsp.status_code, STATUS_OK);
        let exp_rsp = quasar_rpc(
            &ctx,
            &mut admin_sess,
            "eve.dailygoal.api.admin.ExpireRequest",
            make_admin_expire_req(g2, 90_000_001),
        )
        .await;
        assert_eq!(exp_rsp.status_code, STATUS_OK);
        let redeem_exp = quasar_rpc(&ctx, &mut admin_sess, "eve.dailygoal.api.RedeemRequest", make_uuid_req(g2)).await;
        assert_eq!(redeem_exp.status_code, STATUS_NOT_FOUND);
        let exp_notices = drain_notices(&mut rx);
        assert!(exp_notices
            .iter()
            .any(|n| n.normalized_type_name() == "eve.dailygoal.api.PrematurelyDeactivated"));
        assert!(exp_notices.iter().any(|n| n.normalized_type_name() == "eve.dailygoal.api.Removed"));

        let gone_rsp = quasar_rpc(&ctx, &mut admin_sess, "eve.dailygoal.api.GetRequest", make_uuid_req(g2)).await;
        assert_eq!(gone_rsp.status_code, STATUS_NOT_FOUND);
        let after_exp_rsp = quasar_rpc(&ctx, &mut admin_sess, "eve.dailygoal.api.GetAllRequest", Vec::new()).await;
        let after_exp_dyn = reg.decode_dynamic("eve.dailygoal.api.GetAllResponse", &after_exp_rsp.payload.unwrap().value).unwrap();
        let Some(DynamicValue::Repeated(rem_items)) = after_exp_dyn.get(1) else { panic!("expected repeated goals"); };
        assert_eq!(rem_items.len(), 5, "Expired standard template goal must not be resurrected");
    }

    // ============================================================================
    // 2. Loyalty Points Wallet & AssetHolding Operation Closure
    // ============================================================================

    #[tokio::test]
    async fn test_b34_loyalty_points_wallet_assetholding_closure() {
        // parity: quasar:eveProto/generated/eve/loyaltypoints/wallet/wallet_pb2.pyj, quasar:eveProto/generated/eve/loyaltypoints/wallet/api/api_pb2.pyj, quasar:eveProto/generated/eve/loyaltypoints/wallet/assetholding/operation/operation_pb2.pyj, quasar:eveProto/generated/eve/loyaltypoints/wallet/assetholding/operation/api/requests_pb2.pyj, quasar:eveProto/generated/eve/loyaltypoints/wallet/assetholding/operation/api/events_pb2.pyj
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");

        let cid: u32 = 90_000_001;
        let issuer_corp: u32 = 1_000_035;
        let mut unpriv_sess = SessionState::new(1001);
        unpriv_sess.user_id = Some(1);
        unpriv_sess.character_id = Some(i64::from(cid));

        let mut sess = SessionState::new(1002);
        sess.user_id = Some(2);
        sess.character_id = Some(i64::from(cid));
        sess.role = eve_machonet::service_roles::ROLE_GM_MASK;
        let mut rx = ctx
            .quasar_gateway
            .subscribe_session(&sess, Vec::new(), sess.session_id);

        let dep_uuid = [0x11u8; 16];
        let dep_req = make_lp_op_req(dep_uuid, Some(cid), None, issuer_corp, 2_500);

        // Out-of-range u64 amounts (1u64 << 63, u64::MAX) must be rejected with STATUS_BAD_REQUEST (B34-P1-01)
        for bad_amt in [1u64 << 63, u64::MAX] {
            let bad_req = make_lp_op_req([0x10u8; 16], Some(cid), None, issuer_corp, bad_amt);
            assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.loyaltypoints.assetholding.operation.api.DepositRequest", bad_req).await.status_code, STATUS_BAD_REQUEST);
        }

        // Non-GM session must be rejected with STATUS_FORBIDDEN (B34-P0-01)
        let forb_dep = quasar_rpc(
            &ctx,
            &mut unpriv_sess,
            "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
            dep_req.clone(),
        )
        .await;
        assert_eq!(forb_dep.status_code, STATUS_FORBIDDEN);

        let dep_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
            dep_req.clone(),
        )
        .await;
        assert_eq!(dep_rsp.status_code, STATUS_OK);

        // Idempotent duplicate deposit must not double-credit
        let dep_dup = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
            dep_req,
        )
        .await;
        assert_eq!(dep_dup.status_code, STATUS_OK);

        let notices = drain_notices(&mut rx);
        assert_eq!(notices.len(), 3, "Duplicate op_uuid must suppress duplicate notices (B34-P1-01)");
        let bal_added = notices
            .iter()
            .find(|n| n.normalized_type_name() == "eve.loyaltypoints.wallet.api.BalanceAdded")
            .unwrap_or_else(|| panic!("BalanceAdded notice not found in {notices:?}"));
        let bal_dyn = reg
            .decode_dynamic(
                "eve.loyaltypoints.wallet.api.BalanceAdded",
                &bal_added.value,
            )
            .expect("decode BalanceAdded");
        assert_eq!(bal_dyn.get(2), Some(&DynamicValue::Uint64(2_500)));
        assert!(notices
            .iter()
            .any(|n| n.normalized_type_name() == "eve.loyaltypoints.assetholding.operation.api.Deposited"));
        assert!(notices
            .iter()
            .any(|n| n.normalized_type_name() == "eve.loyaltypoints.assetholding.operation.api.Ended"));

        // Withdraw more than balance -> STATUS_PRECONDITION_FAILED
        let over_wd = make_lp_op_req([0x22u8; 16], Some(cid), None, issuer_corp, 9_999);
        let over_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest",
            over_wd,
        )
        .await;
        assert_eq!(over_rsp.status_code, STATUS_PRECONDITION_FAILED);

        // Valid withdraw of 1_000 LP (non-GM rejected first, B34-P0-02)
        let wd_req = make_lp_op_req([0x33u8; 16], Some(cid), None, issuer_corp, 1_000);
        let forb_wd = quasar_rpc(
            &ctx,
            &mut unpriv_sess,
            "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest",
            wd_req.clone(),
        )
        .await;
        assert_eq!(forb_wd.status_code, STATUS_FORBIDDEN);

        let wd_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest",
            wd_req,
        )
        .await;
        assert_eq!(wd_rsp.status_code, STATUS_OK);

        let wd_notices = drain_notices(&mut rx);
        assert!(wd_notices
            .iter()
            .any(|n| n.normalized_type_name() == "eve.loyaltypoints.wallet.api.BalanceRemoved"));
        assert!(wd_notices
            .iter()
            .any(|n| n.normalized_type_name() == "eve.loyaltypoints.assetholding.operation.api.Withdrawn"));
        assert!(wd_notices
            .iter()
            .any(|n| n.normalized_type_name() == "eve.loyaltypoints.assetholding.operation.api.Ended"));

        // Corporation Quasar LP deposit & withdrawal round-trip through LPSvc.GetAllMyCorporationWalletLPBalances (B34-P1-02)
        let corp_id: u32 = 98_000_001;
        sess.corporation_id = Some(i64::from(corp_id));
        sess.corp_role = u64::MAX;
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.loyaltypoints.assetholding.operation.api.DepositRequest", make_lp_op_req([0x44u8; 16], None, Some(corp_id), issuer_corp, 4_000)).await.status_code, STATUS_OK);
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest", make_lp_op_req([0x55u8; 16], None, Some(corp_id), issuer_corp, 1_500)).await.status_code, STATUS_OK);
        let corp_lp_rep = ctx.dispatch_rpc(&mut sess, "LPSvc", None, "GetAllMyCorporationWalletLPBalances", &[eve_proto::PyRep::Integer(i64::from(corp_id))], &std::collections::HashMap::new(), None).await.expect("GetAllMyCorporationWalletLPBalances");
        let eve_proto::PyRep::List(entries) = corp_lp_rep else { panic!("expected List"); };
        let lp_val = entries.iter().find_map(|e| e.dict_get("loyaltyPoints").and_then(|v| v.as_i64()));
        assert_eq!(lp_val, Some(2_500));
    }

    // ============================================================================
    // 3. SolarSystem Bounty NPC Multiplier & ESS Bank Notices Closure
    // ============================================================================

    #[tokio::test]
    async fn test_b34_solarsystem_bounty_npc_and_ess_bank_closure() {
        // parity: quasar:eveProto/generated/eve/solarsystem/bounty/npc_pb2.pyj, quasar:eveProto/generated/eve/solarsystem/bounty/bank/bank_pb2.pyj, quasar:eveProto/generated/eve/solarsystem/bounty/bank/mainbank_pb2.pyj, quasar:eveProto/generated/eve/solarsystem/bounty/bank/reservebank_pb2.pyj
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");

        let sys_id: u32 = 30_000_142;
        let cid: u32 = 9001;
        let mut unpriv_sess = SessionState::new(1001);
        unpriv_sess.user_id = Some(1);
        unpriv_sess.character_id = Some(i64::from(cid));
        unpriv_sess.solar_system_id = Some(sys_id);

        let mut sess = SessionState::new(1002);
        sess.user_id = Some(2);
        sess.character_id = Some(i64::from(cid));
        sess.solar_system_id = Some(sys_id);
        sess.role = eve_machonet::service_roles::ROLE_GM_MASK;
        let mut rx = ctx
            .quasar_gateway
            .subscribe_session(&sess, Vec::new(), sess.session_id);

        // Non-GM session must be rejected with STATUS_FORBIDDEN (B34-P0-03)
        let forb_mult = quasar_rpc(
            &ctx,
            &mut unpriv_sess,
            "eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(forb_mult.status_code, STATUS_FORBIDDEN);

        // Invalid solar_system_id 0 -> STATUS_BAD_REQUEST
        let bad_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(bad_rsp.status_code, STATUS_BAD_REQUEST);

        // Out-of-range multiplier (11.0 > 10.0) -> STATUS_BAD_REQUEST
        let mut oor_req = Vec::new();
        let mut s_bytes = Vec::new();
        encode_varint_field(1, u64::from(sys_id), &mut s_bytes);
        encode_bytes_field(1, &s_bytes, &mut oor_req);
        encode_varint_field(2, 1100, &mut oor_req);
        let mut oor_det = Vec::new();
        encode_varint_field(1, 11, &mut oor_det);
        encode_bytes_field(3, &oor_det, &mut oor_req);
        let oor_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest",
            oor_req,
        )
        .await;
        assert_eq!(oor_rsp.status_code, STATUS_BAD_REQUEST);

        // Valid ApplyMultiplierFactorRequest (135% -> 1.35)
        let mut req = Vec::new();
        encode_bytes_field(1, &s_bytes, &mut req);
        encode_varint_field(2, 135, &mut req);
        let mut det = Vec::new();
        encode_varint_field(1, 1, &mut det);
        encode_varint_field(2, 350_000_000, &mut det);
        encode_bytes_field(3, &det, &mut req);

        let ok_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest",
            req,
        )
        .await;
        assert_eq!(ok_rsp.status_code, STATUS_OK);

        let notices = drain_notices(&mut rx);
        let calc_n = notices
            .iter()
            .find(|n| n.normalized_type_name() == "eve.solarsystem.bounty.npc.MultiplierFactorCalculated")
            .expect("MultiplierFactorCalculated notice");
        let calc_dyn = reg
            .decode_dynamic(
                "eve.solarsystem.bounty.npc.MultiplierFactorCalculated",
                &calc_n.value,
            )
            .expect("decode MultiplierFactorCalculated");
        assert_eq!(calc_dyn.get(2), Some(&DynamicValue::Uint32(135)));
        assert!(notices
            .iter()
            .any(|n| n.normalized_type_name() == "eve.solarsystem.bounty.bank.SolarSystemPersisted"));

        // Sub-1.0 ApplyMultiplierFactorRequest (50% -> 0.50 with proto3-omitted units == 0)
        let mut sub_req = Vec::new();
        encode_bytes_field(1, &s_bytes, &mut sub_req);
        encode_varint_field(2, 50, &mut sub_req);
        let mut sub_det = Vec::new();
        encode_varint_field(2, 500_000_000, &mut sub_det);
        encode_bytes_field(3, &sub_det, &mut sub_req);
        let sub_rsp = quasar_rpc(&ctx, &mut sess, "eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest", sub_req).await;
        assert_eq!(sub_rsp.status_code, STATUS_OK);
        let sub_notices = drain_notices(&mut rx);
        let sub_calc = sub_notices.iter().find(|n| n.normalized_type_name() == "eve.solarsystem.bounty.npc.MultiplierFactorCalculated").expect("sub-1.0 MultiplierFactorCalculated");
        let sub_dyn = reg.decode_dynamic("eve.solarsystem.bounty.npc.MultiplierFactorCalculated", &sub_calc.value).expect("decode sub-1.0");
        assert_eq!(sub_dyn.get(2), Some(&DynamicValue::Uint32(50)));
        if let Some(DynamicValue::Message(det_m)) = sub_dyn.get(3) {
            assert_eq!(det_m.get(1), None);
            assert_eq!(det_m.get(2), Some(&DynamicValue::Uint32(500_000_000)));
        } else {
            panic!("expected determined submessage");
        }

        // Test ESS MainBank & ReserveBank notices through real essMgr.AttemptLinkToMainBank flow (B34-F-07)
        let nullsec_sys: u32 = 30_004_759;
        let ess_ball_id: i64 = 800_001;
        let ship_id: i64 = 700_001;
        let ess_cid: i64 = 94_999;
        let mut ess_sess = crate::modules_advanced_tactical::seed_pilot(
            &ctx,
            ess_cid,
            ship_id,
            587,
            nullsec_sys,
            false,
        );
        ess_sess.station_id = None;
        ctx.db.with_mut_snapshot(|snap| {
            let st = snap.system_states.entry(nullsec_sys).or_insert_with(|| eve_db::SystemDynamicState {
                solar_system_id: nullsec_sys,
                ..Default::default()
            });
            st.ess_main_bank = IskCents::from_isk(5_000_000);
            st.ess_reserve_bank = IskCents::from_isk(20_000_000);
        });
        eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
            rt.safety_levels.insert(ess_cid, eve_server::services::combat_crimewatch::SafetyLevel::Yellow);
        });
        let sys_handle = ctx.systems.get_or_spawn(nullsec_sys);
        let _ = sys_handle
            .spawn_ball(eve_destiny::Ball {
                id: ess_ball_id,
                type_id: 32882,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;
        crate::modules_advanced_tactical::spawn_in_space(
            &ctx,
            ess_cid,
            ship_id,
            587,
            nullsec_sys,
            glam::DVec3::new(1000.0, 0.0, 0.0),
        )
        .await;

        let mut ess_rx = ctx
            .quasar_gateway
            .subscribe_session(&ess_sess, Vec::new(), ess_sess.session_id);

        let kw = HashMap::new();
        ctx.dispatch_rpc(&mut ess_sess, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)], &kw, None).await.expect("AttemptLinkToMainBank start");
        for _ in 0..5 { let _ = sys_handle.step_manual(1.0).await; }
        ctx.dispatch_rpc(&mut ess_sess, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)], &kw, None).await.expect("AttemptLinkToMainBank complete");

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(950_001, eve_db::InventoryItemRecord {
                item_id: 950_001, type_id: 57720, owner_id: ess_cid, location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1,
                dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            });
        });
        ctx.dispatch_rpc(&mut ess_sess, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)], &kw, None).await.expect("AttemptLinkToReserveBank start");
        for _ in 0..5 { let _ = sys_handle.step_manual(1.0).await; }
        ctx.dispatch_rpc(&mut ess_sess, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)], &kw, None).await.expect("AttemptLinkToReserveBank complete");

        let ess_notices = drain_notices(&mut ess_rx);
        let link_est = ess_notices
            .iter()
            .find(|n| n.normalized_type_name() == "eve.solarsystem.bounty.bank.main.MainBankLinkEstablished")
            .expect("MainBankLinkEstablished notice");
        let link_dyn = reg
            .decode_dynamic(
                "eve.solarsystem.bounty.bank.main.MainBankLinkEstablished",
                &link_est.value,
            )
            .expect("decode MainBankLinkEstablished");
        // Field 3 is main_bank_balance (non-zero 5_000_000 ISK), Field 5 is ship_id
        assert!(link_dyn.get(3).is_some(), "MainBankLinkEstablished must carry real main_bank balance");
        assert!(link_dyn.get(5).is_some(), "MainBankLinkEstablished must carry real ship_id");

        for expected in [
            "eve.solarsystem.bounty.bank.main.MainBankLinkEstablished",
            "eve.solarsystem.bounty.bank.main.MainBankPrimed",
            "eve.solarsystem.bounty.bank.main.MainBankTheftCompleted",
            "eve.solarsystem.bounty.bank.reserve.ReserveBankPrimed",
            "eve.solarsystem.bounty.bank.SolarSystemPersisted",
        ] {
            let n = ess_notices
                .iter()
                .find(|n| n.normalized_type_name() == expected)
                .unwrap_or_else(|| panic!("Missing ESS notice {expected}"));
            reg.decode_dynamic(expected, &n.value)
                .unwrap_or_else(|e| panic!("Failed to decode {expected}: {e}"));
        }
    }
}

pub(crate) mod payment_purchase_store_catalog {
    // Integration tests for behavioral verification Quasar endpoints and store/payment/cosmetic/sovereignty services.

    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
        EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::{
        STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_NOT_IMPLEMENTED, STATUS_OK, STATUS_UNAUTHORIZED,
    };
    use eve_server::router::ServerContext;
    use eve_server::services::quasar_achievements::AchievementManager;
    use eve_server::services::quasar_payment_store_types::{
        save_store_catalog, PaymentTokenRecord, StoreCatalog, StoreCategory, StoreOffer, StoreProduct,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
        let mut sess = SessionState::new(sid);
        sess.user_id = Some(uid);
        sess.character_id = Some(cid);
        sess.corporation_id = Some(corp_id);
        sess.solar_system_id = Some(30_000_142);
        sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
        sess
    }

    fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
        if let PyRep::KeyVal(entries) = rep {
            for (k, v) in entries {
                if k == "response_proto" {
                    if let PyRep::Bytes(b) = v {
                        return EvePublicResponse::decode_proto(b)
                            .expect("decode EvePublicResponse from response_proto");
                    }
                }
            }
        }
        panic!("Expected KeyVal containing response_proto, got {rep:?}");
    }

    async fn quasar_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        type_name: &str,
        payload_bytes: Vec<u8>,
    ) -> EvePublicResponse {
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: ctx.now_unix() as i64,
                nanos: 0,
            }),
            correlation_uuid: b"q3-q9-closure-01".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
                value: payload_bytes,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"app-q3-q9-test".to_vec(),
            authoritative_context: None,
            trace_context: None,
        };
        let empty_kw = HashMap::new();
        let rep = ctx
            .dispatch_rpc(
                sess,
                "publicGatewaySvc",
                None,
                "SendRequest",
                &[PyRep::Bytes(req.encode_proto())],
                &empty_kw,
                None,
            )
            .await
            .expect("SendRequest");
        extract_response_proto(&rep)
    }

    #[derive(Debug, Clone)]
    enum RawField {
        Varint(u64),
        Bytes(Vec<u8>),
    }

    fn parse_raw_proto(mut buf: &[u8]) -> Vec<(u32, RawField)> {
        let mut out = Vec::new();
        while !buf.is_empty() {
            let mut tag = 0u64;
            let mut shift = 0;
            loop {
                let b = buf[0];
                buf = &buf[1..];
                tag |= ((b & 0x7f) as u64) << shift;
                if b & 0x80 == 0 {
                    break;
                }
                shift += 7;
            }
            let field_num = (tag >> 3) as u32;
            let wire_type = (tag & 0x07) as u8;
            match wire_type {
                0 => {
                    let mut val = 0u64;
                    let mut s = 0;
                    loop {
                        let b = buf[0];
                        buf = &buf[1..];
                        val |= ((b & 0x7f) as u64) << s;
                        if b & 0x80 == 0 {
                            break;
                        }
                        s += 7;
                    }
                    out.push((field_num, RawField::Varint(val)));
                }
                2 => {
                    let mut len = 0u64;
                    let mut s = 0;
                    loop {
                        let b = buf[0];
                        buf = &buf[1..];
                        len |= ((b & 0x7f) as u64) << s;
                        if b & 0x80 == 0 {
                            break;
                        }
                        s += 7;
                    }
                    let len = len as usize;
                    out.push((field_num, RawField::Bytes(buf[..len].to_vec())));
                    buf = &buf[len..];
                }
                _ => panic!("Unexpected wire type {wire_type}"),
            }
        }
        out
    }

    fn decode_rsp_fields(rsp: &EvePublicResponse) -> Vec<(u32, RawField)> {
        parse_raw_proto(&rsp.payload.as_ref().expect("response payload").value)
    }

    fn raw_u64(fields: &[(u32, RawField)], num: u32) -> Option<u64> {
        fields.iter().find_map(|(n, f)| match f {
            RawField::Varint(v) if *n == num => Some(*v),
            _ => None,
        })
    }

    fn raw_str(fields: &[(u32, RawField)], num: u32) -> Option<String> {
        fields.iter().find_map(|(n, f)| match f {
            RawField::Bytes(b) if *n == num => std::str::from_utf8(b).ok().map(str::to_string),
            _ => None,
        })
    }

    fn raw_msg(fields: &[(u32, RawField)], num: u32) -> Option<Vec<(u32, RawField)>> {
        fields.iter().find_map(|(n, f)| match f {
            RawField::Bytes(b) if *n == num => Some(parse_raw_proto(b)),
            _ => None,
        })
    }

    #[tokio::test]
    async fn test_q3_payment_purchase_and_store_catalog_closure() {
        // parity: quasar:eveProto/generated/eve_public/payment/purchase/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/category/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/offer/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/product/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj
        let ctx = ServerContext::new();
        let mut sess = make_session(1, 1, 90_000_001, 1_000_009);

        // 1. Unseeded store catalog returns empty lists (no hardcoded default fixture)
        let cats_empty = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.store.category.api.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(cats_empty.status_code, STATUS_OK);
        assert!(cats_empty.payload.as_ref().unwrap().value.is_empty());

        // 2. Seed catalog with one offer lacking price_cents and one offer with explicit price_cents
        let school_id = ctx.sde.schools.values().find(|s| s.race_id == 1).unwrap().school_id;
        let ch = ctx.db.create_character(1, "Test Pilot", 1, 1, 11, school_id, &ctx.sde).unwrap();
        sess.character_id = Some(ch.character_id);
        ctx.db.with_mut_snapshot(|snap| {
            let catalog = StoreCatalog {
                categories: vec![StoreCategory {
                    id: 1,
                    name: "Omega".into(),
                    description: "Clone state".into(),
                }],
                products: vec![StoreProduct {
                    id: 101,
                    name: "30 Days Omega".into(),
                    description: "Omega time".into(),
                    type_id: 3966,
                    quantity: 1,
                }],
                offers: vec![
                    StoreOffer {
                        id: 5001,
                        name: "PLEX-Only Offer".into(),
                        description: "No fiat price_cents configured".into(),
                        category_id: 1,
                        price_plex: 500,
                        product_ids: vec![101],
                        targeted: false,
                        price_cents: None,
                        currency: None,
                    },
                    StoreOffer {
                        id: 5002,
                        name: "Fiat-Priced Offer".into(),
                        description: "Explicit price_cents".into(),
                        category_id: 1,
                        price_plex: 500,
                        product_ids: vec![101],
                        targeted: true,
                        price_cents: Some(1_999),
                        currency: Some("USD".into()),
                    },
                ],
            };
            save_store_catalog(snap, &catalog);
            let tokens = vec![PaymentTokenRecord {
                id: 77,
                user_id: 1,
                token_str: "tok_test_77".into(),
                is_quick_pay: true,
                consumed: false,
            }];
            snap.service_state_blobs.insert(
                "payment_tokens".to_string(),
                serde_json::to_string(&tokens).unwrap(),
            );
        });

        // 2b. Verify store.category.api, store.product.api, store.offer.api, and store.targetaudience.offer.api
        let cats_seeded = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.store.category.api.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(cats_seeded.status_code, STATUS_OK);
        let cats_msg = decode_rsp_fields(&cats_seeded);
        let cat_entry = raw_msg(&cats_msg, 1).expect("seeded category entry");
        assert_eq!(raw_u64(&cat_entry, 1), Some(1)); // sequential category id
        assert_eq!(raw_u64(&cat_entry, 2), Some(1)); // catalog = Ingame

        let mut prod_id_msg = Vec::new();
        encode_varint_field(1, 101, &mut prod_id_msg);
        let mut prod_req = Vec::new();
        encode_bytes_field(1, &prod_id_msg, &mut prod_req);
        let prod_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.store.product.api.GetRequest",
            prod_req,
        )
        .await;
        assert_eq!(prod_rsp.status_code, STATUS_OK);
        let prod_msg = decode_rsp_fields(&prod_rsp);
        let prod_attr = raw_msg(&prod_msg, 1).expect("product attributes");
        assert_eq!(raw_str(&prod_attr, 1).as_deref(), Some("30 Days Omega"));
        let eve_item = raw_msg(&prod_attr, 2).expect("product item");
        assert_eq!(raw_u64(&eve_item, 1), Some(3966));
        assert_eq!(raw_u64(&eve_item, 2), Some(1));

        let mut prod_miss_id_msg = Vec::new();
        encode_varint_field(1, 999_999, &mut prod_miss_id_msg);
        let mut prod_miss_req = Vec::new();
        encode_bytes_field(1, &prod_miss_id_msg, &mut prod_miss_req);
        let prod_miss = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.store.product.api.GetRequest",
            prod_miss_req,
        )
        .await;
        assert_eq!(prod_miss.status_code, STATUS_NOT_FOUND);

        let offers_all = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.store.offer.api.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(offers_all.status_code, STATUS_OK);
        let offers_all_msg = decode_rsp_fields(&offers_all);
        let off_entry = raw_msg(&offers_all_msg, 1).expect("non-targeted offer 5001");
        assert_eq!(raw_str(&off_entry, 1).as_deref(), Some("5001"));

        let mut off_id_msg = Vec::new();
        encode_string_field(1, "5002", &mut off_id_msg);
        let mut off_get_req = Vec::new();
        encode_bytes_field(1, &off_id_msg, &mut off_get_req);
        let off_get_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.store.offer.api.GetRequest",
            off_get_req,
        )
        .await;
        assert_eq!(off_get_rsp.status_code, STATUS_OK);
        let off_get_msg = decode_rsp_fields(&off_get_rsp);
        let off_attr = raw_msg(&off_get_msg, 1).expect("offer attributes");
        assert_eq!(raw_str(&off_attr, 1).as_deref(), Some("Fiat-Priced Offer"));
        let price_msg = raw_msg(&off_attr, 12).expect("offer_price");
        assert_eq!(raw_u64(&price_msg, 1), Some(50_000));

        let offers_tgt = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.store.offer.api.GetAllTargetedRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(offers_tgt.status_code, STATUS_OK);
        let offers_tgt_msg = decode_rsp_fields(&offers_tgt);
        let tgt_entry = raw_msg(&offers_tgt_msg, 1).expect("targeted offer 5002");
        assert_eq!(raw_str(&tgt_entry, 1).as_deref(), Some("5002"));

        let ta_list = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.store.targetaudience.offer.api.ListRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(ta_list.status_code, STATUS_OK);
        let ta_list_msg = decode_rsp_fields(&ta_list);
        let ta_entry = raw_msg(&ta_list_msg, 1).expect("targeted offer 5002 in targetaudience");
        assert_eq!(raw_u64(&ta_entry, 1), Some(5002));

        // 3. CostRequest and TokenRequest fail closed with STATUS_NOT_IMPLEMENTED when price_cents is None
        let mut cost_no_fiat = Vec::new();
        encode_varint_field(1, 5001, &mut cost_no_fiat);
        let cost_unpriced = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.payment.purchase.api.CostRequest",
            cost_no_fiat,
        )
        .await;
        assert_eq!(cost_unpriced.status_code, STATUS_NOT_IMPLEMENTED);

        let mut unpriced_offer_id_msg = Vec::new();
        encode_string_field(1, "5001", &mut unpriced_offer_id_msg);
        let mut unpriced_order_msg = Vec::new();
        encode_bytes_field(1, &unpriced_offer_id_msg, &mut unpriced_order_msg);
        encode_varint_field(2, 1, &mut unpriced_order_msg);
        let mut tok_id_msg_unpriced = Vec::new();
        encode_varint_field(1, 77, &mut tok_id_msg_unpriced);
        let mut tok_req_unpriced = Vec::new();
        encode_bytes_field(1, &unpriced_order_msg, &mut tok_req_unpriced);
        encode_bytes_field(2, &tok_id_msg_unpriced, &mut tok_req_unpriced);
        let tok_unpriced = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.payment.purchase.api.TokenRequest",
            tok_req_unpriced,
        )
        .await;
        assert_eq!(tok_unpriced.status_code, STATUS_NOT_IMPLEMENTED);
        ctx.db.with_snapshot(|snap| {
            let tokens: Vec<PaymentTokenRecord> = serde_json::from_str(
                snap.service_state_blobs.get("payment_tokens").unwrap(),
            )
            .unwrap();
            assert!(!tokens[0].consumed, "Token must not be consumed on fail-closed TokenRequest");
        });

        // 4. CostRequest and TokenRequest succeed when price_cents is explicitly configured
        let mut cost_fiat = Vec::new();
        encode_varint_field(1, 5002, &mut cost_fiat);
        let cost_ok = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.payment.purchase.api.CostRequest",
            cost_fiat,
        )
        .await;
        assert_eq!(cost_ok.status_code, STATUS_OK);
        assert_eq!(
            cost_ok.payload.as_ref().unwrap().normalized_type_name(),
            "eve_public.payment.purchase.api.CostResponse"
        );
        let cost_msg = decode_rsp_fields(&cost_ok);
        let cost_inner = raw_msg(&cost_msg, 1).expect("CostResponse inner cost message");
        assert_eq!(raw_u64(&cost_inner, 1), Some(1_999)); // catalog_amount_in_cents
        assert_eq!(raw_u64(&cost_inner, 2), Some(0)); // tax_amount_in_cents
        assert_eq!(raw_u64(&cost_inner, 3), Some(1_999)); // total_amount_in_cents
        assert_eq!(raw_str(&cost_inner, 5).as_deref(), Some("USD")); // currency

        let mut offer_id_msg = Vec::new();
        encode_string_field(1, "5002", &mut offer_id_msg);
        let mut order_msg = Vec::new();
        encode_bytes_field(1, &offer_id_msg, &mut order_msg);
        encode_varint_field(2, 1, &mut order_msg);
        let mut tok_id_msg = Vec::new();
        encode_varint_field(1, 77, &mut tok_id_msg);
        let mut tok_req = Vec::new();
        encode_bytes_field(1, &order_msg, &mut tok_req);
        encode_bytes_field(2, &tok_id_msg, &mut tok_req);
        let tok_ok = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.payment.purchase.api.TokenRequest",
            tok_req,
        )
        .await;
        assert_eq!(tok_ok.status_code, STATUS_OK, "{}", tok_ok.status_message);

        // 5. Unknown offer ID returns STATUS_NOT_FOUND
        let mut cost_miss = Vec::new();
        encode_varint_field(1, 999_999, &mut cost_miss);
        let miss_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.payment.purchase.api.CostRequest",
            cost_miss,
        )
        .await;
        assert_eq!(miss_rsp.status_code, STATUS_NOT_FOUND);
    }

    #[tokio::test]
    async fn test_q4_achievements_sde_and_career_goals_closure() {
        let ctx = ServerContext::new();
        let mut sess = make_session(1, 1, 90_000_001, 1_000_009);
        let mut unauth = SessionState::new(2);

        // 1. AchievementManager loads real SDE achievements with zero synthetic category/score/rewards
        let mgr = AchievementManager::new_with_sde(Some(Arc::clone(&ctx.db)), Some(Arc::clone(&ctx.sde)));
        assert!(mgr.sde_achievement_count > 0, "SDE must contain achievements");
        for def in &mgr.definitions {
            assert_eq!(def.category_id, 0);
            assert_eq!(def.score, 0);
            assert_eq!(def.reward_isk, 0);
            assert_eq!(def.reward_sp, 0);
        }

        // 2. Achievement category, showcase, and reward endpoints
        let mut cat_id_msg = Vec::new();
        encode_bytes_field(1, &1u32.to_be_bytes(), &mut cat_id_msg);
        let mut cat_req = Vec::new();
        encode_bytes_field(1, &cat_id_msg, &mut cat_req);
        let cat_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.achievement.category.api.GetRequest",
            cat_req,
        )
        .await;
        assert_eq!(cat_rsp.status_code, STATUS_OK, "{}", cat_rsp.status_message);

        let score_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.achievement.showcase.api.GetCharacterScoreRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(score_rsp.status_code, STATUS_OK);

        let unauth_claim = quasar_rpc(
            &ctx,
            &mut unauth,
            "eve_public.achievement.reward.api.ClaimRequest",
            Vec::new(),
        )
        .await;
        assert_ne!(unauth_claim.status_code, STATUS_OK);

        // 3. Career goal endpoints
        let goals_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.career.goal.api.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(goals_rsp.status_code, STATUS_OK);
        assert_eq!(
            goals_rsp.payload.as_ref().unwrap().normalized_type_name(),
            "eve_public.career.goal.api.GetAllResponse"
        );
    }

    #[tokio::test]
    async fn test_q5_q6_social_economy_and_ai_assistant_fail_closed() {
        let ctx = ServerContext::new();
        let mut sess = make_session(1, 1, 90_000_001, 1_000_009);

        // 1. AI assistant fails closed with STATUS_NOT_IMPLEMENTED (no generic schema-filler)
        let mut query_payload = Vec::new();
        encode_string_field(1, "How do I fit a Drake?", &mut query_payload);
        let ai_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.ai.assistant.chat.api.QueryRequest",
            query_payload,
        )
        .await;
        assert_eq!(ai_rsp.status_code, STATUS_NOT_IMPLEMENTED);

        // 2. Space CDN map-tag checkpoint validates request and returns SDE-backed checkpoint
        let cdn_bad = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.space.api.cdn.GetUpdateRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(cdn_bad.status_code, STATUS_BAD_REQUEST);

        let mut cdn_req = Vec::new();
        encode_varint_field(2, 1, &mut cdn_req); // no_local_version_available = true
        let cdn_ok = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.space.api.cdn.GetUpdateRequest",
            cdn_req,
        )
        .await;
        assert_eq!(cdn_ok.status_code, STATUS_OK);
        let cdn_payload = cdn_ok.payload.as_ref().unwrap();
        assert_eq!(
            cdn_payload.normalized_type_name(),
            "eve_public.space.api.cdn.GetUpdateResponse"
        );
        assert!(!cdn_payload.value.is_empty());
    }

    #[tokio::test]
    async fn test_q7_q8_sovereignty_planet_resources_and_mercenary_den() {
        let ctx = ServerContext::new();
        let mut sess = make_session(1, 1, 90_000_001, 1_000_009);
        let mut unauth = SessionState::new(2);

        // 1. Planet resource definitions & version
        let defs_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(defs_rsp.status_code, STATUS_OK);
        assert!(!defs_rsp.payload.as_ref().unwrap().value.is_empty());

        let ver_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(ver_rsp.status_code, STATUS_OK);

        // 2. Planet resource configurations for caller's SDE solar system (and unauth rejection)
        let cfgs_unauth = quasar_rpc(
            &ctx,
            &mut unauth,
            "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(cfgs_unauth.status_code, STATUS_UNAUTHORIZED);

        let cfgs_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(cfgs_rsp.status_code, STATUS_OK);
        assert!(!cfgs_rsp.payload.as_ref().unwrap().value.is_empty());

        // 3. Mercenary den maximum & owned queries
        let max_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(max_rsp.status_code, STATUS_OK);
        assert_eq!(
            max_rsp.payload.as_ref().unwrap().normalized_type_name(),
            "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterResponse"
        );
    }

    #[tokio::test]
    async fn test_q9_cosmetics_market_and_skinr_sequencing_closure() {
        let ctx = ServerContext::new();
        let mut sess = make_session(1, 1, 90_000_001, 1_000_009);

        // 1. Broker fees, tax rate, and buyer fees definition
        let broker_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(broker_rsp.status_code, STATUS_OK);

        let tax_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.cosmetic.market.api.GetTaxRateRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(tax_rsp.status_code, STATUS_OK);

        // 2. GetBuyerFeeRequest rejects empty skin identifier with STATUS_BAD_REQUEST and unknown skin with STATUS_NOT_FOUND
        let empty_buyer = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.cosmetic.market.api.GetBuyerFeeRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(empty_buyer.status_code, STATUS_BAD_REQUEST);

        let mut skin_id_msg = Vec::new();
        encode_string_field(1, "nonexistent_skin_hex", &mut skin_id_msg);
        let mut buyer_req = Vec::new();
        encode_bytes_field(2, &skin_id_msg, &mut buyer_req);
        let miss_buyer = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.cosmetic.market.api.GetBuyerFeeRequest",
            buyer_req,
        )
        .await;
        assert_eq!(miss_buyer.status_code, STATUS_NOT_FOUND);

        // 3. Component & SKIN listings and SKINR sequencing job queries
        let comp_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.cosmetic.market.component.listing.api.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(comp_rsp.status_code, STATUS_OK);

        let skin_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(skin_rsp.status_code, STATUS_OK);

        let seq_rsp = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(seq_rsp.status_code, STATUS_OK);
    }
}

pub(crate) mod social_economy_package {
    // Integration tests for Quasar Social & Organization (`corporation`, `corporationgoal`,
    // `freelance`) gateway endpoints.

    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
        TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::STATUS_OK;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
        let mut sess = SessionState::new(sid);
        sess.user_id = Some(uid);
        sess.character_id = Some(cid);
        sess.corporation_id = Some(corp_id);
        sess.solar_system_id = Some(30_000_142);
        sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
        sess
    }

    fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
        if let PyRep::KeyVal(entries) = rep {
            for (k, v) in entries {
                if k == "response_proto" {
                    if let PyRep::Bytes(b) = v {
                        return EvePublicResponse::decode_proto(b)
                            .expect("decode EvePublicResponse from response_proto");
                    }
                }
            }
        }
        panic!("Expected KeyVal containing response_proto, got {rep:?}");
    }

    async fn quasar_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        type_name: &str,
        payload_bytes: Vec<u8>,
    ) -> EvePublicResponse {
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: ctx.now_unix() as i64,
                nanos: 0,
            }),
            correlation_uuid: b"q5-q6-closure-01".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
                value: payload_bytes,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"app-q5-q6-test".to_vec(),
            authoritative_context: None,
            trace_context: None,
        };
        let empty_kw = HashMap::new();
        let rep = ctx
            .dispatch_rpc(
                sess,
                "publicGatewaySvc",
                None,
                "SendRequest",
                &[PyRep::Bytes(req.encode_proto())],
                &empty_kw,
                None,
            )
            .await
            .expect("SendRequest");
        extract_response_proto(&rep)
    }

    #[tokio::test]
    async fn test_q5_q6_social_and_economy_package_closure() {
        let ctx = ServerContext::new();
        let mut sess = make_session(1, 1, 90_000_001, 1_000_009);
        let mut unauth = SessionState::new(2);

        // 1. corporation package: skill plans
        let unauth_corp = quasar_rpc(
            &ctx,
            &mut unauth,
            "eve_public.corporation.skill.plan.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_ne!(unauth_corp.status_code, STATUS_OK);

        let corp_plans = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.corporation.skill.plan.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(corp_plans.status_code, STATUS_OK);
        assert_eq!(
            corp_plans.payload.as_ref().unwrap().normalized_type_name(),
            "eve_public.corporation.skill.plan.GetAllResponse"
        );

        // 2. corporationgoal package: active/all goals
        let corp_goals = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.corporationgoal.api.GetAllRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(corp_goals.status_code, STATUS_OK);
        assert_eq!(
            corp_goals.payload.as_ref().unwrap().normalized_type_name(),
            "eve_public.corporationgoal.api.GetAllResponse"
        );

        // 3. freelance package: active projects & creation limits
        let freelance_active = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.freelance.project.api.GetAllActiveRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(freelance_active.status_code, STATUS_OK);
        assert_eq!(
            freelance_active.payload.as_ref().unwrap().normalized_type_name(),
            "eve_public.freelance.project.api.GetAllActiveResponse"
        );

        let freelance_limits = quasar_rpc(
            &ctx,
            &mut sess,
            "eve_public.freelance.project.api.GetCreationLimitsRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(freelance_limits.status_code, STATUS_OK);
        assert_eq!(
            freelance_limits.payload.as_ref().unwrap().normalized_type_name(),
            "eve_public.freelance.project.api.GetCreationLimitsResponse"
        );
    }
}

pub(crate) mod sov_hub_star_resource_configurations {

    use crate::common;
    use crate::quasar_sov_equinox_hub::{drain, field, fields, id_req, idm, rpc, sess, structure, subscribe, varint};
    use eve_db::corp_roles;
    use eve_db::snapshot_ext::FwSystemOccupationRecord;
    use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
    use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_IMPLEMENTED, STATUS_OK};
    use eve_server::router::ServerContext;
    use eve_server::services::sov_services::{TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB};

    const PILOT: i64 = 90_000_001;
    const CORP: i64 = 98_000_001;
    const JITA: u32 = 30_000_142;
    const PERIMETER: u32 = 30_000_144;
    const URLEN: u32 = 30_000_138;

    fn anchor_jita_skyhooks(ctx: &ServerContext) {
        let jita_planets: Vec<i64> = ctx
            .sde
            .celestials_in_system(JITA)
            .into_iter()
            .filter(|c| c.group_id == 7)
            .map(|c| c.celestial_id)
            .collect();
        ctx.db.with_mut_snapshot(|snap| {
            for (idx, pid) in jita_planets.into_iter().enumerate() {
                let sh_id = 1_020_000_200 + idx as i64;
                snap.structures.insert(sh_id, structure(sh_id, CORP, JITA, TYPE_ORBITAL_SKYHOOK));
                snap.sov_runtime.skyhook_planets.insert(sh_id, pid);
                snap.sov_runtime.skyhook_active.insert(sh_id, true);
            }
        });
    }

    #[tokio::test]
    async fn test_sov_hub_and_star_resource_configurations() {
        // Kept untagged (open): client sovereignty star/planet resource tables are not in the SDE.
        let ctx = ServerContext::new();
        let mut s = sess(92_001, PILOT, CORP, JITA, corp_roles::ROLE_STATION_MANAGER);

        let hub_id = 1_020_000_051;
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(hub_id, structure(hub_id, CORP, JITA, TYPE_SOVEREIGNTY_HUB));
        });
        anchor_jita_skyhooks(&ctx);

        let res = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_id as u64)).await;
        assert_eq!(res.status_code, STATUS_OK);
        let payload = res.payload.expect("GetResourcesResponse payload").value;
        let power = field(&payload, 1).expect("power pair");
        let workforce = field(&payload, 2).expect("workforce pair");
        assert!(varint(&power, 1).unwrap_or(0) > 0, "Jita star power > 0");
        assert!(varint(&workforce, 1).unwrap_or(0) > 0, "Jita planet workforce > 0");

        let star_res = rpc(
            &ctx,
            &mut s,
            "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(star_res.status_code, STATUS_OK);
        let star_payload = star_res.payload.expect("GetAllConfigurationsResponse payload").value;
        let expected_stars = ctx.sde.all_stars();
        let returned_cfgs = fields(&star_payload, 1);
        assert_eq!(returned_cfgs.len(), expected_stars.len(), "must return all SDE stars");
        assert!(returned_cfgs.len() > 1, "must return stars across all systems, not just caller's system");

        let cfg = &returned_cfgs[0];
        let star_id = field(&cfg, 1).and_then(|b| varint(&b, 1)).expect("star id");
        let star_power = varint(&cfg, 2).expect("star power");
        assert!(
            expected_stars.iter().any(|(id, _)| *id as u64 == star_id),
            "star_id {star_id} must match an SDE star"
        );
        assert!(star_power > 0);
    }

    #[tokio::test]
    async fn test_sov_hub_workforce_transit_relay_network() {
        // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj
        eve_testkit::attest(&[
            "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj",
            "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj",
        ]);

        let ctx = ServerContext::new();
        let mut s = sess(92_002, PILOT, CORP, JITA, corp_roles::ROLE_STATION_MANAGER);
        let mut rx = subscribe(&ctx, &mut s).await;

        let (hub_a, hub_b, hub_c) = (1_020_000_061, 1_020_000_062, 1_020_000_063);
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(hub_a, structure(hub_a, CORP, JITA, TYPE_SOVEREIGNTY_HUB));
            snap.structures.insert(hub_b, structure(hub_b, CORP, PERIMETER, TYPE_SOVEREIGNTY_HUB));
            snap.structures.insert(hub_c, structure(hub_c, CORP, URLEN, TYPE_SOVEREIGNTY_HUB));
        });
        anchor_jita_skyhooks(&ctx);

        // Export 1000 from Jita to Perimeter, and Import in Urlen from Jita.
        let mut exp = Vec::new();
        encode_bytes_field(3, &idm(PERIMETER as u64), &mut exp);
        encode_varint_field(2, 1_000, &mut exp);
        let mut exp_cfg = Vec::new();
        encode_bytes_field(3, &exp, &mut exp_cfg);
        let mut cfg_req_a = id_req(1, hub_a as u64);
        encode_bytes_field(2, &exp_cfg, &mut cfg_req_a);
        assert_eq!(
            rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_req_a)
                .await
                .status_code,
            STATUS_OK
        );

        let mut imp = Vec::new();
        encode_bytes_field(1, &id_req(1, JITA as u64), &mut imp);
        let mut imp_cfg = Vec::new();
        encode_bytes_field(2, &imp, &mut imp_cfg);
        let mut cfg_req_c = id_req(1, hub_c as u64);
        encode_bytes_field(2, &imp_cfg, &mut cfg_req_c);
        assert_eq!(
            rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_req_c)
                .await
                .status_code,
            STATUS_OK
        );
        drain(&mut rx);

        // Configure Perimeter hub as Transit relay (field 4 = true).
        let mut transit_cfg = Vec::new();
        encode_varint_field(4, 1, &mut transit_cfg);
        let mut cfg_req_b = id_req(1, hub_b as u64);
        encode_bytes_field(2, &transit_cfg, &mut cfg_req_b);
        assert_eq!(
            rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_req_b)
                .await
                .status_code,
            STATUS_OK
        );

        let notices = drain(&mut rx);
        assert!(
            notices.iter().any(|(t, _)| {
                t == "eve_public.sovereignty.resource.transfer.workforce.api.StateChangedNotice"
            }),
            "Transit configuration must emit StateChangedNotice"
        );

        // Verify Urlen hub now imports 1000 workforce via Perimeter Transit relay.
        let st_res = rpc(
            &ctx,
            &mut s,
            "eve_public.sovereignty.hub.workforce.api.GetStateRequest",
            id_req(1, hub_c as u64),
        )
        .await;
        assert_eq!(st_res.status_code, STATUS_OK);
        let st_val = st_res.payload.unwrap().value;
        let state_msg = field(&st_val, 1).unwrap();
        let imp_state = field(&state_msg, 2).unwrap();
        let src_entry = field(&imp_state, 1).unwrap();
        assert_eq!(varint(&src_entry, 2), Some(1_000));

        // (a) Negative test: two importers competing for the same export -> only lowest hub ID receives it.
        let hub_d = 1_020_000_064;
        let ikuchi: u32 = 30_000_137; // 1 jump from Perimeter
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(hub_d, structure(hub_d, CORP, ikuchi, TYPE_SOVEREIGNTY_HUB));
        });
        let mut imp_d = Vec::new();
        encode_bytes_field(1, &id_req(1, JITA as u64), &mut imp_d);
        let mut imp_cfg_d = Vec::new();
        encode_bytes_field(2, &imp_d, &mut imp_cfg_d);
        let mut cfg_req_d = id_req(1, hub_d as u64);
        encode_bytes_field(2, &imp_cfg_d, &mut cfg_req_d);
        assert_eq!(
            rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_req_d)
                .await
                .status_code,
            STATUS_OK
        );

        let st_d = rpc(
            &ctx,
            &mut s,
            "eve_public.sovereignty.hub.workforce.api.GetStateRequest",
            id_req(1, hub_d as u64),
        )
        .await;
        assert_eq!(st_d.status_code, STATUS_OK);
        let st_d_val = st_d.payload.unwrap().value;
        let state_d = field(&st_d_val, 1).unwrap();
        let imp_d_state = field(&state_d, 2).unwrap();
        let src_d = field(&imp_d_state, 1).unwrap();
        assert_eq!(varint(&src_d, 2), Some(0), "second competing importer must receive 0 workforce");

        // (b) Negative test: Transit hub owned by a foreign corp/alliance -> relay is blocked.
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&hub_b) {
                st.owner_corp_id = CORP + 999;
            }
        });
        let st_c_foreign = rpc(
            &ctx,
            &mut s,
            "eve_public.sovereignty.hub.workforce.api.GetStateRequest",
            id_req(1, hub_c as u64),
        )
        .await;
        assert_eq!(st_c_foreign.status_code, STATUS_OK);
        let st_cf_val = st_c_foreign.payload.unwrap().value;
        let state_cf = field(&st_cf_val, 1).unwrap();
        let imp_cf = field(&state_cf, 2).unwrap();
        let src_cf = field(&imp_cf, 1).unwrap();
        assert_eq!(varint(&src_cf, 2), Some(0), "foreign-owned Transit hub must block relay");

        // Restore hub_b ownership for out-of-range test.
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&hub_b) {
                st.owner_corp_id = CORP;
            }
        });

        // (c) Negative test: out-of-range (> 2 jumps / > WORKFORCE_TRANSFER_MAX_JUMPS) -> rejected & 0 imported.
        let hub_far = 1_020_000_065;
        let amarr: u32 = 30_002_187; // > 5 jumps from Jita and Perimeter
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(hub_far, structure(hub_far, CORP, amarr, TYPE_SOVEREIGNTY_HUB));
        });
        let mut imp_far = Vec::new();
        encode_bytes_field(1, &id_req(1, JITA as u64), &mut imp_far);
        let mut imp_cfg_far = Vec::new();
        encode_bytes_field(2, &imp_far, &mut imp_cfg_far);
        let mut cfg_req_far = id_req(1, hub_far as u64);
        encode_bytes_field(2, &imp_cfg_far, &mut cfg_req_far);
        assert_eq!(
            rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_req_far)
                .await
                .status_code,
            STATUS_BAD_REQUEST,
            "out-of-range import configuration must be rejected"
        );
    }

    #[tokio::test]
    async fn test_skinr_sequencing_job_natural_completion_notice() {
        // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/notices_pb2.pyj
        eve_testkit::attest(&[
            "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/notices_pb2.pyj",
        ]);

        let ctx = ServerContext::new();
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT) {
                ch.plex_balance = 100;
            }
        });
        let mut gm = sess(92_003, PILOT, CORP, JITA, 0);
        gm.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
        let mut s = sess(92_004, PILOT, CORP, JITA, 0);
        let mut rx = subscribe(&ctx, &mut s).await;

        let skinr_cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
        let coating_comp = skinr_cat
            .components
            .values()
            .find(|c| !c.is_pattern() && !c.associated_types.is_empty())
            .expect("coating comp");
        let comp_id = coating_comp.component_id as u64;

        // Grant finite component license to PILOT
        let grant_req = id_req(1, comp_id);
        let mut grant_outer = Vec::new();
        encode_bytes_field(1, &grant_req, &mut grant_outer);
        encode_varint_field(2, 5, &mut grant_outer);
        assert_eq!(
            rpc(
                &ctx,
                &mut gm,
                "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
                grant_outer,
            )
            .await
            .status_code,
            STATUS_OK
        );

        // Initiate sequencing job for Rifter (type 587)
        let mut slot_msg = Vec::new();
        encode_bytes_field(1, &idm(1), &mut slot_msg);
        let mut coat_cfg = Vec::new();
        encode_bytes_field(1, &idm(comp_id), &mut coat_cfg);
        let mut cfg_outer = Vec::new();
        encode_bytes_field(1, &coat_cfg, &mut cfg_outer);
        encode_bytes_field(2, &cfg_outer, &mut slot_msg);

        let mut layout = Vec::new();
        encode_bytes_field(1, &slot_msg, &mut layout);
        encode_bytes_field(2, &idm(587), &mut layout);
        encode_varint_field(3, 1, &mut layout);

        let mut init_req = Vec::new();
        encode_string_field(1, "T1T3T5 Test Skin", &mut init_req);
        encode_string_field(2, "Test Line", &mut init_req);
        encode_bytes_field(3, &layout, &mut init_req);
        encode_varint_field(5, 1, &mut init_req);

        let init_res = rpc(
            &ctx,
            &mut s,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            init_req,
        )
        .await;
        assert_eq!(init_res.status_code, STATUS_OK);
        drain(&mut rx);

        // Advance clock by 30 days so planned_duration_secs elapses naturally
        ctx.clock.read().unwrap().advance_secs(30 * 86_400);


        // Without calling GetAllActiveRequest first:
        // 1. CompleteEarlyRequest on an already-elapsed job must reject and deduct 0 PLEX.
        let plex_before_early = ctx
            .db
            .with_snapshot(|snap| snap.characters.get(&PILOT).map(|c| c.plex_balance))
            .unwrap_or(0);
        let early_res = rpc(
            &ctx,
            &mut s,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(early_res.status_code, STATUS_BAD_REQUEST);
        let plex_after = ctx
            .db
            .with_snapshot(|snap| snap.characters.get(&PILOT).map(|c| c.plex_balance))
            .unwrap_or(0);
        assert_eq!(plex_after, plex_before_early, "Naturally elapsed job must not deduct 5 PLEX on CompleteEarlyRequest");

        // 2. GetOwnedRequest must already include the newly sequenced SKIN license.
        let owned_res = rpc(
            &ctx,
            &mut s,
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(owned_res.status_code, STATUS_OK);
        assert_eq!(
            fields(&owned_res.payload.unwrap().value, 1).len(),
            1,
            "GetOwnedRequest must settle elapsed job and return the new SKIN license"
        );

        let active_res = rpc(
            &ctx,
            &mut s,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(active_res.status_code, STATUS_OK);
        let notices = drain(&mut rx);
        assert!(
            notices.iter().any(|(t, _)| {
                t == "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompletedNotice"
            }),
            "Natural completion must emit CompletedNotice"
        );
    }

    #[tokio::test]
    async fn test_payment_schedule_omega_purchase_and_faction_activity_admin() {
        // parity: quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj
        eve_testkit::attest(&[
            "quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj",
            "quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj",
        ]);

        let ctx = ServerContext::new();
        common::seed_test_nes_store_catalog(&ctx);
        ctx.db.with_mut_snapshot(|snap| {
            let tokens = vec![
                eve_server::services::quasar_payment_store::PaymentTokenRecord {
                    id: 1,
                    user_id: 1,
                    token_str: "tok_visa_4242".into(),
                    is_quick_pay: true,
                    consumed: false,
                },
                eve_server::services::quasar_payment_store::PaymentTokenRecord {
                    id: 2,
                    user_id: 1,
                    token_str: "tok_visa_4243".into(),
                    is_quick_pay: true,
                    consumed: false,
                },
            ];
            snap.service_state_blobs
                .insert("payment_tokens".to_string(), serde_json::to_string(&tokens).unwrap());
        });
        let mut s = sess(92_005, PILOT, CORP, JITA, 0);

        // Purchase Omega offer "1001" twice via TokenRequest -> creates and extends payment schedule
        let now_unix = ctx.now_unix();
        let mut off_ident = Vec::new();
        encode_string_field(1, "1001", &mut off_ident);
        let mut order_msg = Vec::new();
        encode_bytes_field(1, &off_ident, &mut order_msg);
        let mut purchase_req = Vec::new();
        encode_bytes_field(1, &order_msg, &mut purchase_req);
        encode_bytes_field(2, &idm(1), &mut purchase_req);
        let p_res = rpc(&ctx, &mut s, "eve_public.payment.purchase.api.TokenRequest", purchase_req).await;
        assert_eq!(p_res.status_code, STATUS_OK);

        let mut purchase_req2 = Vec::new();
        encode_bytes_field(1, &order_msg, &mut purchase_req2);
        encode_bytes_field(2, &idm(2), &mut purchase_req2);
        let p_res2 = rpc(&ctx, &mut s, "eve_public.payment.purchase.api.TokenRequest", purchase_req2).await;
        assert_eq!(p_res2.status_code, STATUS_OK);

        let mut by_type_req = Vec::new();
        encode_varint_field(1, 1, &mut by_type_req);
        let active_sched = rpc(
            &ctx,
            &mut s,
            "eve_public.payment.schedule.api.GetActiveByTypeRequest",
            by_type_req,
        )
        .await;
        assert_eq!(active_sched.status_code, STATUS_OK);
        let active_payload = active_sched.payload.unwrap().value;
        let first_id = field(&active_payload, 1)
            .and_then(|b| varint(&b, 1))
            .expect("schedule id");

        let get_sched = rpc(
            &ctx,
            &mut s,
            "eve_public.payment.schedule.api.GetRequest",
            id_req(1, first_id),
        )
        .await;
        assert_eq!(get_sched.status_code, STATUS_OK);
        let sched_payload = get_sched.payload.unwrap().value;
        let sched_attr = field(&sched_payload, 1).expect("schedule attributes");
        assert_eq!(varint(&sched_attr, 1), Some(1), "Omega schedule type = 1");
        let next_pay_ts = field(&sched_attr, 2).and_then(|t| varint(&t, 1)).unwrap_or(0);
        assert!(
            next_pay_ts >= now_unix + 60 * 86_400,
            "Consecutive 30-day Omega purchases must accumulate next_payment_unix (+60d), got {next_pay_ts} vs {now_unix}"
        );
        assert_eq!(varint(&sched_attr, 4), Some(30), "30-day recurring interval");

        // Faction activity admin: non-admin rejected, admin increments FW victory points
        ctx.db.with_mut_snapshot(|snap| {
            snap.fw_system_occupations.insert(
                30_002_537,
                FwSystemOccupationRecord {
                    solar_system_id: 30_002_537,
                    owner_faction_id: 500_001,
                    occupier_faction_id: 500_001,
                    victory_points: 100,
                    victory_points_threshold: 3_000,
                },
            );
        });

        let mut inc_req = id_req(1, 30_002_537);
        encode_bytes_field(2, &idm(500_002), &mut inc_req);
        encode_varint_field(3, 250, &mut inc_req);

        assert_eq!(
            rpc(
                &ctx,
                &mut s,
                "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
                inc_req.clone(),
            )
            .await
            .status_code,
            STATUS_FORBIDDEN
        );

        let mut gm = sess(92_006, PILOT, CORP, 30_002_537, 0);
        gm.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
        assert_eq!(
            rpc(
                &ctx,
                &mut gm,
                "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
                inc_req,
            )
            .await
            .status_code,
            STATUS_OK
        );
        let vp = ctx
            .db
            .with_snapshot(|snap| snap.fw_runtime.system_victory_points.get(&30_002_537).copied())
            .unwrap_or(0);
        assert!(vp >= 250, "FW system victory points must reflect admin increment, got {vp}");
    }

    #[tokio::test]
    async fn test_ai_assistant_chat_data_gap() {
        // Kept untagged (open): Aura Guidance AI assistant requires external LLM service (DATA GAP).
        let ctx = ServerContext::new();
        let mut s = sess(92_007, PILOT, CORP, JITA, 0);

        let mut jita_req = Vec::new();
        encode_string_field(1, "Jita", &mut jita_req);
        let jita_res = rpc(&ctx, &mut s, "eve_public.ai.assistant.chat.api.QueryRequest", jita_req).await;
        assert_eq!(jita_res.status_code, STATUS_NOT_IMPLEMENTED);
    }
}

pub(crate) mod freelance_pirate_stargate_mercenary_den {
    // behavioral verification tests for Quasar Pirate Insurgency, Stargate Lock, Freelance Contribution Method,
    // Mercenary Den, Skyhook admin and Star resource endpoints.
    //
    // Validates data-driven responses, state mutations, and notices emitted by real server state
    // transitions (RPCs / stargate jumps) — no test calls a `publish_*` helper directly.


    use crate::quasar_sov_equinox_hub::{drain, field, fields, id_req, idm, rpc, sess, structure, subscribe, varint};
    use eve_destiny::MovementMode;
    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        encode_bytes_field, encode_varint_field, EvePublicNotice, EvePublicRequest, EvePublicResponse,
        GoogleAny, GoogleTimestamp, QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_OK};
    use eve_server::router::ServerContext;
    use eve_server::services::sov_services::TYPE_ORBITAL_SKYHOOK;
    use std::collections::HashMap;
    use tokio::sync::mpsc::Receiver;

    const PILOT: i64 = 90_000_001;
    const OTHER: i64 = 90_000_002;
    const TURNUR: u32 = 30_002_086;
    const ZARZAKH: u32 = 30_100_000;
    /// SDE stargate Turnur -> Zarzakh (`mapJumps`).
    const TURNUR_ZARZAKH_GATE: i64 = 50_016_562;

    fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
        if let PyRep::KeyVal(entries) = rep {
            for (k, v) in entries {
                if k == "response_proto" {
                    if let PyRep::Bytes(b) = v {
                        return EvePublicResponse::decode_proto(b)
                            .expect("decode EvePublicResponse from response_proto");
                    }
                }
            }
        }
        panic!("Expected KeyVal containing response_proto, got {rep:?}");
    }

    fn build_sess(sid: u64, cid: u32, corp_id: u32, sys_id: u32) -> SessionState {
        let mut sess = SessionState::new(sid);
        sess.user_id = Some(1);
        sess.character_id = Some(cid as i64);
        sess.corporation_id = Some(corp_id as i64);
        sess.solar_system_id = Some(sys_id);
        sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
        sess.corp_role = 0x01; // Director role
        sess
    }

    async fn quasar_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        type_url: &str,
        payload_value: Vec<u8>,
        corr: &[u8],
    ) -> EvePublicResponse {
        let empty_kw = HashMap::new();
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: ctx.now_unix() as i64,
                nanos: 0,
            }),
            correlation_uuid: corr.to_vec(),
            payload: Some(GoogleAny {
                type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                    type_url.to_string()
                } else {
                    format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
                },
                value: payload_value,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"app-sov-pirate-01".to_vec(),
            authoritative_context: None,
            trace_context: None,
        };
        let rep = ctx
            .dispatch_rpc(
                sess,
                "publicGatewaySvc",
                None,
                "SendRequest",
                &[PyRep::Bytes(req.encode_proto())],
                &empty_kw,
                None,
            )
            .await
            .expect("SendRequest");
        extract_response_proto(&rep)
    }

    fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<EvePublicNotice> {
        let mut out = Vec::new();
        for _ in 0..1024 {
            match rx.try_recv() {
                Ok(msg) => out.push(msg),
                Err(_) => break,
            }
        }
        out
    }

    fn encode_id_req(id: u64) -> Vec<u8> {
        let mut out = Vec::new();
        let mut id_buf = Vec::new();
        encode_varint_field(1, id, &mut id_buf);
        encode_bytes_field(1, &id_buf, &mut out);
        out
    }

    #[tokio::test]
    async fn test_authentic_freelance_pirate_stargate_operations() {
        // parity: feat:quasar.pirate, feat:quasar.stargate
        // parity: quasar:eveProto/generated/eve_public/freelance/contributionmethod/definition/api/requests_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/pirate/corruption/api/notices_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/pirate/corruption/api/requests_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/pirate/suppression/api/notices_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/pirate/suppression/api/requests_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/stargate/lock/api/notices_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/stargate/lock/api/requests_pb2.pyj
        eve_testkit::attest(&[
            "feat:quasar.pirate",
            "feat:quasar.stargate",
            "quasar:eveProto/generated/eve_public/freelance/contributionmethod/definition/api/requests_pb2.pyj",
            "quasar:eveProto/generated/eve_public/pirate/corruption/api/notices_pb2.pyj",
            "quasar:eveProto/generated/eve_public/pirate/corruption/api/requests_pb2.pyj",
            "quasar:eveProto/generated/eve_public/pirate/suppression/api/notices_pb2.pyj",
            "quasar:eveProto/generated/eve_public/pirate/suppression/api/requests_pb2.pyj",
            "quasar:eveProto/generated/eve_public/stargate/lock/api/notices_pb2.pyj",
            "quasar:eveProto/generated/eve_public/stargate/lock/api/requests_pb2.pyj",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let zarzakh_sys = ZARZAKH;
        let mut sess = build_sess(60_001, 90_000_021, 98_000_001, zarzakh_sys);

        ctx.dispatch_rpc(&mut sess, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None)
            .await
            .expect("Subscribe");
        let mut stream_rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("stream rx");

        // 1. Freelance contribution method definitions: GetAllLatestRequest and GetAllLatestWithinMajorRequest
        let r1 = quasar_rpc(&ctx, &mut sess, "eve_public.freelance.contributionmethod.definition.api.GetAllLatestRequest", Vec::new(), b"fl-01").await;
        assert_eq!(r1.status_code, STATUS_OK);
        let r1_payload = r1.payload.expect("r1 payload");
        assert_eq!(r1_payload.normalized_type_name(), "eve_public.freelance.contributionmethod.definition.api.GetAllLatestResponse");

        let wm1 = WireMessage::decode(&r1_payload.value).expect("decode GetAllLatestResponse");
        // Verify field 1: Version specification
        let ver_field = wm1.fields.iter().find(|f| f.field_number == 1).expect("version field");
        let ver_wm = WireMessage::decode(ver_field.value.as_bytes().expect("ver bytes")).expect("decode ver");
        assert_eq!(ver_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()), Some(1), "Major 1");

        // Verify field 2: Repeated definition entries (>= 20 authentic methods)
        let def_fields: Vec<_> = wm1.fields.iter().filter(|f| f.field_number == 2).collect();
        assert_eq!(def_fields.len(), 22, "all 22 SDE contributionmethods.fsdbinary records");
        // SDE record `ManufactureItem`: titleID 665394, progressDescriptionID 696246, 3 parameters.
        let manufacture = def_fields
            .iter()
            .filter_map(|f| f.value.as_bytes())
            .find(|d| field(d, 1).and_then(|id| WireMessage::decode(&id).ok()).is_some_and(|m| m.fields.iter().any(|f| f.value.as_bytes() == Some(b"ManufactureItem".as_slice()))))
            .expect("ManufactureItem definition");
        let attrs = field(manufacture, 2).expect("attributes");
        assert_eq!(field(&attrs, 1).and_then(|t| varint(&t, 1)), Some(665_394), "SDE titleID");
        assert_eq!(field(&attrs, 3).and_then(|t| varint(&t, 1)), Some(696_246), "SDE progressDescriptionID");
        let params = fields(&attrs, 11);
        assert_eq!(params.len(), 3, "SDE parameter definitions encoded");
        let first = field(&params[0], 1).expect("matcher parameter");
        assert_eq!(field(&first, 1).as_deref(), Some(b"item_type".as_slice()));

        let r2 = quasar_rpc(&ctx, &mut sess, "eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorRequest", Vec::new(), b"fl-02").await;
        assert_eq!(r2.status_code, STATUS_OK);

        // 2. Pirate Insurgency Corruption & Suppression: GetSystemInfoRequest & GetStageThresholdsRequest
        //    DERIVED: stage thresholds `[200, 400, 600, 800, 1000]` over denominator `1000`
        //    (`quasar_space_services::INSURGENCY_STAGE_THRESHOLDS`).
        let gate_pos = ctx.sde.jumps[&TURNUR_ZARZAKH_GATE].position.to_array();
        ctx.db.with_mut_snapshot(|s| {
            s.insurgency_runtime.campaigns.insert(
                1,
                eve_db::PersistedInsurgencyCampaign {
                    campaign_id: 1,
                    pirate_faction_id: eve_server::services::insurgency_services::FACTION_GURISTAS,
                    target_faction_id: 500001,
                    fob_solar_system_id: zarzakh_sys,
                    systems: vec![zarzakh_sys, TURNUR],
                    created_at_unix: ctx.now_unix(),
                    forecasting_end_unix: ctx.now_unix(),
                    active_end_unix: ctx.now_unix() + 86400,
                    winddown_end_unix: ctx.now_unix() + 172800,
                    corruption_points: [(zarzakh_sys, 450), (TURNUR, 200)].into_iter().collect(),
                    suppression_points: [(zarzakh_sys, 250), (TURNUR, 150)].into_iter().collect(),
                },
            );
            s.system_states.insert(
                zarzakh_sys,
                eve_db::SystemDynamicState {
                    solar_system_id: zarzakh_sys,
                    insurgency_corruption_stage: 3,
                    insurgency_suppression_stage: 2,
                    ..Default::default()
                },
            );
            s.system_states.insert(
                TURNUR,
                eve_db::SystemDynamicState {
                    solar_system_id: TURNUR,
                    fw_complex_position: Some(gate_pos),
                    fw_capture_duration_seconds: 600,
                    insurgency_corruption_stage: 1,
                    insurgency_suppression_stage: 1,
                    ..Default::default()
                },
            );
        });

        assert_eq!(
            quasar_rpc(&ctx, &mut sess, "eve_public.pirate.corruption.api.GetSystemInfoRequest", encode_id_req(99_999_999), b"pi-404c").await.status_code,
            eve_server::quasar_gateway::STATUS_NOT_FOUND
        );
        assert_eq!(
            quasar_rpc(&ctx, &mut sess, "eve_public.pirate.suppression.api.GetSystemInfoRequest", encode_id_req(99_999_999), b"pi-404s").await.status_code,
            eve_server::quasar_gateway::STATUS_NOT_FOUND
        );

        let corr_info = quasar_rpc(&ctx, &mut sess, "eve_public.pirate.corruption.api.GetSystemInfoRequest", encode_id_req(zarzakh_sys as u64), b"pi-01").await;
        assert_eq!(corr_info.status_code, STATUS_OK);
        let c_bytes = corr_info.payload.expect("corr payload").value;
        let c_wm = WireMessage::decode(&c_bytes).expect("decode corr info");
        assert_eq!(c_wm.fields.iter().find(|f| f.field_number == 4).and_then(|f| f.value.as_varint()), Some(3), "Stage 3");
        assert_eq!(field(&c_bytes, 1).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(450), Some(1000))));

        let corr_th = quasar_rpc(&ctx, &mut sess, "eve_public.pirate.corruption.api.GetStageThresholdsRequest", Vec::new(), b"pi-02").await;
        assert_eq!(corr_th.status_code, STATUS_OK);
        let cth_bytes = corr_th.payload.expect("corr th payload").value;
        assert_eq!(
            fields(&cth_bytes, 1).iter().map(|f| (varint(f, 1), varint(f, 2))).collect::<Vec<_>>(),
            vec![(Some(200), Some(1000)), (Some(400), Some(1000)), (Some(600), Some(1000)), (Some(800), Some(1000)), (Some(1000), Some(1000))]
        );

        let supp_info = quasar_rpc(&ctx, &mut sess, "eve_public.pirate.suppression.api.GetSystemInfoRequest", encode_id_req(zarzakh_sys as u64), b"pi-03").await;
        assert_eq!(supp_info.status_code, STATUS_OK);
        let s_bytes = supp_info.payload.expect("supp payload").value;
        let s_wm = WireMessage::decode(&s_bytes).expect("decode supp info");
        assert_eq!(s_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(2), "Stage 2");
        assert_eq!(field(&s_bytes, 1).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(250), Some(1000))));

        let supp_th = quasar_rpc(&ctx, &mut sess, "eve_public.pirate.suppression.api.GetStageThresholdsRequest", Vec::new(), b"pi-04").await;
        assert_eq!(supp_th.status_code, STATUS_OK);
        let sth_bytes = supp_th.payload.expect("supp th payload").value;
        assert_eq!(fields(&sth_bytes, 1).len(), 5);

        // GetForWarzoneRequest maps target_faction_id 500_001 -> warzone 1 (campaign 1), warzone 2 -> no_campaign (field 1 == 1), warzone 0 -> 400.
        assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.pirate.insurgency.api.GetForWarzoneRequest", Vec::new(), b"pi-wz0").await.status_code, STATUS_BAD_REQUEST);
        let wz1 = quasar_rpc(&ctx, &mut sess, "eve.pirate.insurgency.api.GetForWarzoneRequest", encode_id_req(1), b"pi-wz1").await.payload.expect("wz1").value;
        assert_eq!(field(&wz1, 2).and_then(|i| varint(&i, 1)), Some(1), "warzone 1 returns campaign 1");
        let wz2 = quasar_rpc(&ctx, &mut sess, "eve.pirate.insurgency.api.GetForWarzoneRequest", encode_id_req(2), b"pi-wz2").await.payload.expect("wz2").value;
        assert_eq!(varint(&wz2, 1), Some(1), "warzone 2 reports no_campaign");

        // 3. Point decreases emit SetNotice (not IncreasedNotice).
        eve_server::services::insurgency_services::add_system_corruption(&ctx, 1, zarzakh_sys, -50);
        eve_server::services::insurgency_services::add_system_suppression(&ctx, 1, zarzakh_sys, -50);
        let dec_nots = drain_notices(&mut stream_rx);
        assert!(!dec_nots.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.ends_with("api.IncreasedNotice"))));
        let c_set = dec_nots.iter().find_map(|n| n.payload.as_ref().filter(|p| p.type_url.ends_with("pirate.corruption.api.SetNotice"))).expect("corruption SetNotice");
        assert_eq!(field(&c_set.value, 1).and_then(|i| varint(&i, 1)), Some(zarzakh_sys as u64));
        assert_eq!(field(&c_set.value, 2).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(400), Some(1000))));
        let s_set = dec_nots.iter().find_map(|n| n.payload.as_ref().filter(|p| p.type_url.ends_with("pirate.suppression.api.SetNotice"))).expect("suppression SetNotice");
        assert_eq!(field(&s_set.value, 1).and_then(|i| varint(&i, 1)), Some(zarzakh_sys as u64));
        assert_eq!(field(&s_set.value, 2).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(200), Some(1000))));

        // 4. Real client-facing complex capture RPC in Turnur emits IncreasedNotice for corruption/suppression,
        //    followed by a real CmdStargateJump Turnur -> Zarzakh that creates the 6h gate lock.
        let kw = HashMap::new();
        let mut jsess = SessionState::new(60_010);
        crate::common::login_pilot(&ctx, &mut jsess, "demo", "demo-secret").await;
        ctx.dispatch_rpc(&mut jsess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(PILOT)], &kw, None).await.expect("SelectCharacterID");
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT);
        ctx.dispatch_rpc(&mut jsess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
        let ship_id = jsess.ship_id.expect("active ship");
        let mut ball = ctx.systems.get_or_spawn(30_000_142).remove_ball(ship_id).await.expect("ball in Jita");
        ctx.db.set_character_solar_system(PILOT, TURNUR, &ctx.sde).expect("move to Turnur");
        ball.position = ctx.sde.jumps[&TURNUR_ZARZAKH_GATE].position;
        ball.mode = MovementMode::Stop;
        ctx.systems.get_or_spawn(TURNUR).spawn_ball(ball).await.expect("spawn in Turnur");
        jsess.solar_system_id = Some(TURNUR);
        ctx.dispatch_rpc(&mut jsess, "publicGatewaySvc", None, "Subscribe", &[], &kw, None).await.expect("Subscribe");
        let mut jump_rx = ctx.quasar_gateway.take_stream_receiver(jsess.session_id).expect("jump stream rx");

        // Enlist in Guristas (500_010) and step capture tick near complex beacon -> emits corruption IncreasedNotice.
        ctx.dispatch_rpc(&mut jsess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(500_010)], &kw, None).await.expect("Join Guristas");
        eve_server::services::insurgency_services::step_system_insurgency_capture_tick(&ctx, TURNUR, ship_id, 500_010).await;
        ctx.dispatch_rpc(&mut jsess, "insurgencySolarsystem", None, "GetLocalCampaignClientSnapshot", &[], &kw, None).await.expect("GetLocalCampaignClientSnapshot pirate");
        let c_inc = drain_notices(&mut jump_rx)
            .into_iter()
            .find_map(|n| n.payload.filter(|p| p.type_url.ends_with("pirate.corruption.api.IncreasedNotice")))
            .expect("corruption IncreasedNotice from complex pulse");
        assert_eq!(field(&c_inc.value, 1).and_then(|i| varint(&i, 1)), Some(TURNUR as u64));
        assert_eq!(field(&c_inc.value, 2).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(200), Some(1000))));
        assert_eq!(field(&c_inc.value, 3).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(201), Some(1000))));
        assert_eq!((varint(&c_inc.value, 4), varint(&c_inc.value, 5)), (Some(1), Some(0)));

        // Enlist in Caldari State (500_001), reset complex tick guard, and step capture tick -> emits suppression IncreasedNotice.
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&PILOT) {
                ch.war_faction_id = None;
            }
            s.system_states.get_mut(&TURNUR).unwrap().last_fw_capture_tick = 0;
        });
        ctx.dispatch_rpc(&mut jsess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(500_001)], &kw, None).await.expect("Join Caldari");
        eve_server::services::insurgency_services::step_system_insurgency_capture_tick(&ctx, TURNUR, ship_id, 500_001).await;
        ctx.dispatch_rpc(&mut jsess, "insurgencySolarsystem", None, "GetLocalCampaignClientSnapshot", &[], &kw, None).await.expect("GetLocalCampaignClientSnapshot anti-pirate");
        let s_inc = drain_notices(&mut jump_rx)
            .into_iter()
            .find_map(|n| n.payload.filter(|p| p.type_url.ends_with("pirate.suppression.api.IncreasedNotice")))
            .expect("suppression IncreasedNotice from complex pulse");
        assert_eq!(field(&s_inc.value, 1).and_then(|i| varint(&i, 1)), Some(TURNUR as u64));
        assert_eq!(field(&s_inc.value, 2).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(150), Some(1000))));
        assert_eq!(field(&s_inc.value, 3).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(151), Some(1000))));
        assert_eq!((varint(&s_inc.value, 4), varint(&s_inc.value, 5)), (Some(1), Some(0)));

        // Ending the insurgency campaign emits ResetNotice (0/1000) for both corruption and suppression.
        eve_server::services::insurgency_services::end_insurgency_campaign(&ctx, 1);
        let reset_nots = drain_notices(&mut stream_rx);
        for suffix in ["pirate.corruption.api.ResetNotice", "pirate.suppression.api.ResetNotice"] {
            let r_not = reset_nots.iter().find_map(|n| n.payload.as_ref().filter(|p| p.type_url.ends_with(suffix))).expect(suffix);
            assert_eq!(field(&r_not.value, 1).and_then(|i| varint(&i, 1)), Some(zarzakh_sys as u64));
            assert_eq!(field(&r_not.value, 2).map(|f| (varint(&f, 1), varint(&f, 2))), Some((Some(0), Some(1000))));
        }

        let beyonce = ctx.bind_object(&mut jsess, "beyonce", TURNUR as i64).expect("bind beyonce");
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT);
        ctx.dispatch_rpc(&mut jsess, "beyonce", Some(beyonce), "CmdStargateJump", &[PyRep::Integer(TURNUR_ZARZAKH_GATE)], &kw, None)
            .await
            .expect("jump into Zarzakh");
        assert_eq!(jsess.solar_system_id, Some(ZARZAKH));
        let dest_gate = ctx.sde.jumps[&TURNUR_ZARZAKH_GATE].destination_stargate_id;
        let lock_not = drain_notices(&mut jump_rx)
            .into_iter()
            .find_map(|n| n.payload.filter(|p| p.type_url.ends_with("stargate.lock.api.ActivatedNotice")))
            .expect("jumping pilot receives ActivatedNotice");
        let lock_wm = WireMessage::decode(&lock_not.value).expect("decode ActivatedNotice");
        let sub = |n: u32| lock_wm.fields.iter().find(|f| f.field_number == n).and_then(|f| f.value.as_bytes()).and_then(|b| WireMessage::decode(b).ok());
        assert_eq!(sub(1).and_then(|m| m.fields[0].value.as_varint()), Some(ZARZAKH as u64));
        assert_eq!(sub(2).and_then(|m| m.fields[0].value.as_varint()), Some(dest_gate as u64));

        let lock_rsp = quasar_rpc(&ctx, &mut jsess, "eve_public.stargate.lock.api.GetRequest", Vec::new(), b"gl-01").await;
        assert_eq!(lock_rsp.status_code, STATUS_OK);
        let lock_wm = WireMessage::decode(&lock_rsp.payload.expect("lock payload").value).expect("decode lock info");
        let gate = lock_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()).and_then(|b| WireMessage::decode(b).ok());
        assert_eq!(gate.and_then(|m| m.fields[0].value.as_varint()), Some(dest_gate as u64), "lock bound to arrival gate");

        let rest_rsp = quasar_rpc(&ctx, &mut jsess, "eve_public.stargate.lock.api.GetRestrictedSystemsRequest", Vec::new(), b"gl-02").await;
        assert_eq!(rest_rsp.status_code, STATUS_OK);
        let rest_wm = WireMessage::decode(&rest_rsp.payload.expect("rest payload").value).expect("decode rest");
        let rest_sys = rest_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()).and_then(|b| WireMessage::decode(b).ok());
        assert_eq!(rest_sys.and_then(|m| m.fields[0].value.as_varint()), Some(ZARZAKH as u64), "restricted system is Zarzakh");

        // After the 6h lock expires neither request reports it.
        ctx.advance_secs(6 * 3_600 + 1);
        let lock_rsp = quasar_rpc(&ctx, &mut jsess, "eve_public.stargate.lock.api.GetRequest", Vec::new(), b"gl-03").await;
        assert!(lock_rsp.payload.expect("lock payload").value.is_empty(), "expired lock not reported");
        let rest_rsp = quasar_rpc(&ctx, &mut jsess, "eve_public.stargate.lock.api.GetRestrictedSystemsRequest", Vec::new(), b"gl-04").await;
        assert!(rest_rsp.payload.expect("rest payload").value.is_empty(), "no restricted systems once expired");
    }

    #[tokio::test]
    async fn test_mercenary_den_activities_and_evolution_state_backed() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let sys_id = ctx
            .sde
            .solar_systems
            .values()
            .find(|s| {
                s.security_status <= 0.0
                    && s.solar_system_id < 31_000_000
                    && s.solar_system_id != 30_100_000
                    && s.region_id != 10_000_070
                    && ctx.sde.constellations.get(&s.constellation_id).and_then(|c| c.faction_id).is_none()
                    && ctx.sde.regions.get(&s.region_id).and_then(|r| r.faction_id).is_none()
                    && ctx.sde.celestials_in_system(s.solar_system_id).iter().any(|c| c.type_id == 11)
            })
            .map(|s| s.solar_system_id)
            .expect("claimable nullsec system with Temperate planet");
        let temp_planet = ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| c.type_id == 11).unwrap();
        let (hub_id, skyhook_id, ship_id) = (1_000_000_049i64, 1_000_000_050i64, 100_000_001i64);
        let mut s = sess(60_003, PILOT, 98_000_001, sys_id, eve_db::corp_roles::ROLE_DIRECTOR);
        s.alliance_id = Some(99_000_001);
        s.ship_id = Some(ship_id);
        let mut rx = subscribe(&ctx, &mut s).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.system_states.entry(sys_id).or_default().sov_alliance_id = Some(99_000_001);
            snap.structures.insert(hub_id, structure(hub_id, 98_000_001, sys_id, eve_server::services::sov_services::TYPE_SOVEREIGNTY_HUB));
            let mut sh = structure(skyhook_id, 98_000_001, sys_id, TYPE_ORBITAL_SKYHOOK);
            sh.position = temp_planet.position.to_array();
            snap.structures.insert(skyhook_id, sh);
            snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet.celestial_id);
            snap.skills.entry(PILOT).or_default().insert(85_233, eve_db::TrainedSkillRecord { type_id: 85_233, level: 3, skill_points: 0.0 });
            snap.items.insert(
                990_701,
                eve_db::InventoryItemRecord {
                    item_id: 990_701,
                    type_id: 85_230,
                    owner_id: PILOT,
                    location_id: ship_id,
                    flag: eve_db::InventoryFlag::Cargo,
                    quantity: 1,
                    ..Default::default()
                },
            );
            if let Some(ch) = snap.characters.get_mut(&PILOT) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = sys_id;
                ch.active_ship_id = ship_id;
            }
        });
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_h.spawn_ball(eve_destiny::Ball {
            id: skyhook_id,
            type_id: TYPE_ORBITAL_SKYHOOK,
            position: temp_planet.position,
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 65.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        }).await;
        let _ = sys_h.spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 639,
            position: temp_planet.position + glam::DVec3::new(2_500.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 1_500.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 65.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        }).await;

        let anchor_rep = ctx
            .dispatch_rpc(
                &mut s,
                "structureDeployment",
                None,
                "Anchor",
                &[PyRep::Integer(85_230), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet.celestial_id)],
                &std::collections::HashMap::new(),
                None,
            )
            .await
            .expect("anchor Mercenary Den");
        let den_id = anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()).expect("den_id");
        let _ = drain(&mut rx);

        let max = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest", Vec::new()).await;
        let max = max.payload.expect("max").value;
        assert_eq!((varint(&max, 1), varint(&max, 2)), (Some(3), Some(5)), "Mercenary Den Management level 3");
        let owned = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest", Vec::new()).await.payload.expect("owned").value;
        assert_eq!(fields(&owned, 1).iter().map(|i| varint(i, 1)).collect::<Vec<_>>(), vec![Some(den_id as u64)]);
        let mut other = sess(60_004, OTHER, 98_000_002, sys_id, 0);
        assert_eq!(rpc(&ctx, &mut other, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.status_code, STATUS_FORBIDDEN);

        // Canonical evolution (+5/24h): 3 days lift development 0 -> 15 (still stage 1), +24h lifts 15 -> 20 (stage 1 -> 2) and emits EvolutionStateChangedNotice.
        ctx.advance_secs(3 * 86_400);
        let den_day3 = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.payload.expect("den day 3").value;
        let dev_day3 = field(&field(&field(&den_day3, 3).unwrap(), 2).unwrap(), 3).unwrap();
        assert_eq!((varint(&dev_day3, 1), varint(&dev_day3, 2)), (Some(15), Some(1)));
        let info_day3 = field(&field(&den_day3, 4).unwrap(), 2).unwrap();
        assert_eq!(varint(&info_day3, 1), Some(3_000), "72h * 65/h caps at 3,000 infomorph capacity");

        ctx.advance_secs(86_400);
        let den = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.payload.expect("den day 4").value;
        let dev = field(&field(&field(&den, 3).unwrap(), 2).unwrap(), 3).unwrap();
        assert_eq!((varint(&dev, 1), varint(&dev, 2)), (Some(20), Some(2)));
        assert!(drain(&mut rx).iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice"));

        // GM admin development change to max (100 = stage 5) -> EvolutionStateChangedNotice; players are rejected.
        let mut modify = id_req(1, den_id as u64);
        encode_varint_field(2, 100, &mut modify);
        assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.api.admin.ModifyDevelopmentLevelRequest", modify.clone()).await.status_code, STATUS_FORBIDDEN);
        let mut gm = sess(60_005, OTHER, 98_000_002, sys_id, 0);
        gm.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
        assert_eq!(rpc(&ctx, &mut gm, "eve_public.sovereignty.mercenaryden.api.admin.ModifyDevelopmentLevelRequest", modify).await.status_code, STATUS_OK);
        let gm_notices = drain(&mut rx);
        assert!(gm_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice"));
        assert!(gm_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.DefinitionsUpdatedNotice"));

        // Activity generation at stage 5 expires the 4-day-old initial 3 ops and fills all 7 slots; capacity matches.
        let acts = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64)).await.payload.expect("acts").value;
        let list = fields(&acts, 1);
        assert_eq!(list.len(), 7);
        let gen_notices = drain(&mut rx);
        assert_eq!(gen_notices.iter().filter(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.RemovedNotice").count(), 3);
        assert_eq!(gen_notices.iter().filter(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.AddedNotice").count(), 7);
        let cap = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest", Vec::new()).await.payload.expect("cap").value;
        assert_eq!(varint(&cap, 1), Some(7));
        let all = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest", Vec::new()).await.payload.expect("all").value;
        assert_eq!(fields(&all, 1).len(), 7);

        // Start one operation: StartedNotice, ExpiryChangedNotice, persisted, and cannot start twice.
        let start_req = field(&list[0], 1).map(|id| {
            let mut b = Vec::new();
            encode_bytes_field(1, &id, &mut b);
            b
        }).expect("activity id");
        let started = rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req.clone()).await;
        assert_eq!(started.status_code, STATUS_OK);
        assert_eq!(field(&started.payload.unwrap().value, 1).and_then(|a| varint(&a, 3)), Some(1));
        let start_notices = drain(&mut rx);
        assert!(start_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.StartedNotice"));
        assert!(start_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.ExpiryChangedNotice"));
        assert_ne!(rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req).await.status_code, STATUS_OK);
        assert_eq!(ctx.db.with_snapshot(|snap| snap.sov_runtime.mercenary_activities.values().filter(|a| a.started).count()), 1);

        // Complete tactical operation via space entity destruction.
        let site_ball_id = ctx.db.with_snapshot(|snap| snap.sov_runtime.mto_site_balls.keys().copied().next()).expect("site ball");
        eve_server::services::sov_gameplay::on_space_entity_destroyed_ext(&ctx, sys_id, Some(PILOT), site_ball_id, 24_690, true);
        let complete_notices = drain(&mut rx);
        assert!(complete_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice"));
        assert_eq!(ctx.db.with_snapshot(|snap| snap.sov_runtime.mercenary_activities.values().filter(|a| a.started).count()), 0);

        // Four days later unstarted operations expire (RemovedNotice) and the generator refills.
        ctx.advance_secs(4 * 86_400);
        rpc(&ctx, &mut s, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64)).await;
        let n = drain(&mut rx);
        assert_eq!(n.iter().filter(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.RemovedNotice").count(), 6);
        assert_eq!(n.iter().filter(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.AddedNotice").count(), 7);
    }

    #[tokio::test]
    async fn test_skyhook_admin_auth_window_and_star_resources() {
        // parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/requests_pb2.pyj
        eve_testkit::attest(&[
            "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/admin/admin_pb2.pyj",
            "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/notices_pb2.pyj",
            "quasar:eveProto/generated/eve_public/sovereignty/skyhook/api/requests_pb2.pyj",
        ]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let (sys_id, skyhook_id) = (30_000_142u32, 1_000_000_050i64);
        let mut s = sess(60_006, PILOT, 98_000_001, sys_id, eve_db::corp_roles::ROLE_DIRECTOR);
        let mut rx = subscribe(&ctx, &mut s).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(skyhook_id, structure(skyhook_id, 98_000_001, sys_id, TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(skyhook_id, 40_009_080); // Jita III (non-first planet)
            snap.structures.insert(1_000_000_052, structure(1_000_000_052, 98_000_002, sys_id, TYPE_ORBITAL_SKYHOOK));
        });

        // Star configurations come from all SDE star celestials, including Jita - Star (40_009_076).
        let stars = rpc(&ctx, &mut s, "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest", Vec::new()).await.payload.expect("stars").value;
        let all_stars = fields(&stars, 1);
        assert_eq!(all_stars.len(), ctx.sde.all_stars().len());
        let jita_star = all_stars
            .iter()
            .find(|cfg| field(cfg, 1).and_then(|i| varint(&i, 1)) == Some(40_009_076))
            .expect("Jita - Star configuration");
        assert_eq!(varint(jita_star, 2), Some(800), "O-class star power");

        // ModifyReagents: ownership + director role enforced; state mutated on success.
        let mut reag = Vec::new();
        encode_bytes_field(1, &idm(81_143), &mut reag);
        encode_varint_field(2, 1_200, &mut reag);
        let mut mod_req = id_req(1, skyhook_id as u64);
        encode_bytes_field(2, &reag, &mut mod_req);
        let mut other = sess(60_007, OTHER, 98_000_002, sys_id, eve_db::corp_roles::ROLE_DIRECTOR);
        let mut no_role = sess(60_008, PILOT, 98_000_001, sys_id, 0);
        // `.admin.` packages are GM-gated by the gateway; give every session GM bits so the
        // handler's own ownership + director checks are what reject `other` / `no_role`.
        for x in [&mut s, &mut other, &mut no_role] {
            x.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
        }
        assert_eq!(rpc(&ctx, &mut other, "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest", mod_req.clone()).await.status_code, STATUS_FORBIDDEN);
        assert_eq!(rpc(&ctx, &mut no_role, "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest", mod_req.clone()).await.status_code, STATUS_FORBIDDEN);
        assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest", mod_req).await.status_code, STATUS_OK);
        assert_eq!(ctx.db.with_snapshot(|snap| snap.sov_runtime.skyhook_reagents.get(&skyhook_id).copied()), Some((81_143, 1_200)));
        assert!(drain(&mut rx).iter().any(|(t, _)| t == "eve_public.sovereignty.skyhook.api.ReagentSimulationsNotice"));

        // Theft window: requested timestamps decoded and validated against the server clock.
        let now = ctx.now_unix();
        let window = |start: u64, end: u64| {
            let mut b = id_req(1, skyhook_id as u64);
            encode_bytes_field(2, &idm(start), &mut b);
            encode_bytes_field(3, &idm(end), &mut b);
            b
        };
        let sched = "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest";
        assert_eq!(rpc(&ctx, &mut s, sched, window(now - 60, now + 3_600)).await.status_code, STATUS_BAD_REQUEST);
        assert_eq!(rpc(&ctx, &mut s, sched, window(now + 3_600, now + 1_800)).await.status_code, STATUS_BAD_REQUEST);
        assert_eq!(rpc(&ctx, &mut no_role, sched, window(now + 3_600, now + 7_200)).await.status_code, STATUS_FORBIDDEN);
        assert_eq!(rpc(&ctx, &mut s, sched, window(now + 3_600, now + 7_200)).await.status_code, STATUS_OK);
        assert_eq!(ctx.db.with_snapshot(|snap| snap.sov_runtime.skyhook_theft_windows.get(&skyhook_id).copied()), Some((now + 3_600, now + 7_200)));
        let n = drain(&mut rx);
        let notice = n.iter().find(|(t, _)| t == "eve_public.sovereignty.skyhook.api.TheftVulnerabilityWindowScheduledNotice").expect("scheduled notice");
        assert_eq!(field(&notice.1, 2).and_then(|t| varint(&t, 1)), Some(now + 3_600));
        assert_eq!(field(&notice.1, 3).and_then(|t| varint(&t, 1)), Some(now + 7_200));

        // Queries are state-backed: unknown skyhooks 404, corp listing only returns skyhooks, and the
        // theft window shows up as vulnerable only once it opens on the server clock.
        assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.GetRequest", id_req(1, 1_000_000_001)).await.status_code, eve_server::quasar_gateway::STATUS_NOT_FOUND);
        ctx.db.with_mut_snapshot(|snap| { snap.structures.insert(1_000_000_051, structure(1_000_000_051, 98_000_001, sys_id, 35_832)); });
        let by_corp = rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest", id_req(1, 98_000_001)).await.payload.expect("by corp").value;
        assert_eq!(fields(&by_corp, 1).iter().map(|sk| field(sk, 1).and_then(|i| varint(&i, 1))).collect::<Vec<_>>(), vec![Some(skyhook_id as u64)]);
        let vuln = "eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksRequest";
        assert!(rpc(&ctx, &mut s, vuln, Vec::new()).await.payload.expect("vuln").value.is_empty(), "window not open yet");
        ctx.advance_secs(3_600);
        let systems = rpc(&ctx, &mut s, vuln, Vec::new()).await.payload.expect("vuln").value;
        assert_eq!(fields(&systems, 1).iter().map(|f| varint(f, 1)).collect::<Vec<_>>(), vec![Some(sys_id as u64)]);
        let in_sys = rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemRequest", id_req(1, sys_id as u64)).await.payload.expect("in sys").value;
        assert_eq!(field(&fields(&in_sys, 1)[0], 3).and_then(|t| varint(&t, 1)), Some(now + 7_200), "window end");

        // Activate / Deactivate require ownership + director, persist in `skyhook_active` without
        // mutating `UpwellStructureRecord.state`, reject redundant transitions without leaking notices,
        // and emit `ActivationNotice` with `solar_system(1)`, `planet(2)`, `skyhook(3)`, and `active(4)`.
        assert_eq!(rpc(&ctx, &mut no_role, "eve_public.sovereignty.skyhook.api.DeactivateRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_FORBIDDEN);
        assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.ActivateRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_BAD_REQUEST, "already active");
        assert!(drain(&mut rx).is_empty(), "failed ActivateRequest must not publish notices");
        assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.DeactivateRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_OK);
        assert_eq!(ctx.db.with_snapshot(|snap| snap.structures[&skyhook_id].state.clone()), "ShieldVulnerable", "UpwellStructureRecord.state preserved");
        let sk_deact = rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.GetRequest", id_req(1, skyhook_id as u64)).await.payload.expect("get skyhook").value;
        assert_eq!(varint(&sk_deact, 3), Some(0), "skyhook active==false after DeactivateRequest");
        let sk_unbound = rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.GetRequest", id_req(1, 1_000_000_052)).await.payload.expect("get unbound skyhook").value;
        assert!(field(&sk_unbound, 6).is_none(), "unbound Skyhook omits planet_id instead of fabricating Jita I");
        assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.DeactivateRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_BAD_REQUEST, "already inactive");
        assert_eq!(rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.ActivateRequest", id_req(1, skyhook_id as u64)).await.status_code, STATUS_OK);
        let sk_act = rpc(&ctx, &mut s, "eve_public.sovereignty.skyhook.api.GetRequest", id_req(1, skyhook_id as u64)).await.payload.expect("get skyhook").value;
        assert_eq!(varint(&sk_act, 3), Some(1), "skyhook active==true after ActivateRequest");
        assert_eq!(field(&sk_act, 6).and_then(|i| varint(&i, 1)), Some(40_009_080), "bound non-first planet_id");
        let act_notices: Vec<_> = drain(&mut rx).into_iter().filter(|(t, _)| t == "eve_public.sovereignty.skyhook.api.ActivationNotice").collect();
        assert_eq!(act_notices.len(), 2);
        for (idx, expected_active) in [(0usize, 0u64), (1usize, 1u64)] {
            let body = &act_notices[idx].1;
            assert_eq!(field(body, 1).and_then(|i| varint(&i, 1)), Some(sys_id as u64), "solar_system(1)");
            assert_eq!(field(body, 2).and_then(|i| varint(&i, 1)), Some(40_009_080), "planet(2) bound Jita III");
            assert_eq!(field(body, 3).and_then(|i| varint(&i, 1)), Some(skyhook_id as u64), "skyhook(3)");
            assert_eq!(varint(body, 4), Some(expected_active), "active(4)");
        }
    }
}
