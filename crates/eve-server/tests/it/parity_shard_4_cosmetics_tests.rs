//! End-to-end integration tests for Paragon EverMarks, SKINR designer and sequencing
//! studio, Paragon Hub cosmetic market, and character titles.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes,
    TrainedSkillRecord, WorldRepository,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicNotice,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::lp_store_catalog::{emblem_evermark_price, paragon_sde_evermark_offers};
use eve_server::services::quasar_cosmetics_skinr::{encode_component_id, SkinrStaticCatalog};
use eve_server::services::quasar_cosmetics_skinr_seq::early_completion_plex_cents;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;

const PARAGON_CORP: i64 = 1_000_419;
const PARAGON_STATION: i64 = 60_015_169;
const JITA_STATION: i64 = 60_003_760;
const JITA_SYS: u32 = 30_000_142;
const RIFTER_TYPE: u32 = 587;
const THORAX_TYPE: u32 = 627;
const RAVEN_TYPE: u32 = 638;
const NAGLFAR_TYPE: u32 = 19_724;
const RAGNAROK_TYPE: u32 = 23_773;

fn sess(sid: u64, uid: u64, cid: i64, corp: i64, role: u64) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(uid);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp);
    s.solar_system_id = Some(JITA_SYS);
    s.station_id = Some(JITA_STATION);
    s.role = role;
    s
}

fn seed_pilot(db: &WorldRepository, cid: i64, uid: u64, name: &str, corp: i64, isk_cents: i64, plex: i64, evermarks: i64) {
    db.with_mut_snapshot(|snap| {
        let uname = format!("user_{uid}");
        snap.accounts.entry(uname.clone()).or_insert_with(|| AccountRecord {
            user_id: uid,
            username: uname,
            password_token: "hash".into(),
            role: service_roles::ROLE_PLAYER_DEFAULT,
            banned_until_unix: None,
            ban_reason: None,
        });
        snap.account_plex_vault.insert(uid, plex);
        let ship_id = snap.next_entity_id.max(1_000_000_100) + cid;
        snap.next_entity_id = ship_id + 10;
        let mut lp = HashMap::new();
        lp.insert(PARAGON_CORP, evermarks);
        snap.characters.insert(cid, CharacterRecord {
            character_id: cid,
            user_id: uid,
            name: name.into(),
            corporation_id: corp,
            alliance_id: None,
            war_faction_id: None,
            station_id: Some(JITA_STATION),
            structure_id: None,
            solar_system_id: JITA_SYS,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            active_ship_id: ship_id,
            balance: IskCents(isk_cents),
            plex_balance: plex,
            loyalty_points: lp,
            standings: HashMap::new(),
            security_status: 5.0,
            clone_grade: 2,
            home_station_id: JITA_STATION,
            race_id: 1,
            bloodline_id: 1,
            ancestry_id: 11,
            school_id: 17,
            neural: NeuralAttributes::default(),
            skill_points: 5_000_000.0,
            activity_status: None,
            free_skill_points: 50_000.0,
            last_clone_jump_unix: 0,
            jump_fatigue_minutes: 0.0,
            jump_cooldown_until_unix: 0,
            jump_fatigue_until_unix: 0,
            description: String::new(),
            title: String::new(),
            gender: 0,
            create_date_unix: 1_704_067_200,
            fleet_id: None,
            autopilot_waypoints: Vec::new(),
            version: 1,
            system_epoch: 1,
        });
        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id,
            type_id: RIFTER_TYPE,
            owner_id: cid,
            location_id: JITA_STATION,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        });
    });
}

async fn qrpc(ctx: &ServerContext, s: &mut SessionState, ty: &str, value: Vec<u8>) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: vec![7; 16],
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{ty}"), value }),
        external_origin: "eve-client".into(),
        application_instance_uuid: vec![9; 16],
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(s, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None)
        .await
        .expect("SendRequest");
    let PyRep::KeyVal(kv) = &rep else { panic!("expected KeyVal response, got {rep:?}"); };
    let raw = kv.iter().find_map(|(k, v)| match (k.as_str(), v) {
        ("response_proto", PyRep::Bytes(b)) => Some(b.clone()),
        _ => None,
    }).expect("response_proto");
    EvePublicResponse::decode_proto(&raw).expect("decode EvePublicResponse")
}

async fn subscribe(ctx: &ServerContext, s: &mut SessionState) -> Receiver<EvePublicNotice> {
    ctx.dispatch_rpc(s, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None).await.expect("Subscribe");
    ctx.quasar_gateway.take_stream_receiver(s.session_id).expect("stream receiver")
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        if let Some(p) = n.payload {
            out.push(p.normalized_type_name().to_string());
        }
    }
    out
}

fn put_seq(field: u32, id: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    encode_bytes_field(field, &b, out);
}

fn put_uuid(field: u32, uuid: &[u8; 16], out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_bytes_field(1, uuid, &mut b);
    encode_bytes_field(field, &b, out);
}

fn put_isk(field: u32, units: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    let v = i64::try_from(units).unwrap_or(i64::MAX);
    let zz = ((v << 1) ^ (v >> 63)) as u64;
    encode_varint_field(1, zz, &mut b);
    encode_varint_field(2, 0, &mut b);
    encode_bytes_field(field, &b, out);
}

fn put_seconds(field: u32, secs: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, secs, &mut b);
    encode_bytes_field(field, &b, out);
}

fn extract_first_uuid(bytes: &[u8], list_field: u32) -> [u8; 16] {
    let wm = WireMessage::decode(bytes).expect("wire decode");
    for f in wm.fields {
        if f.field_number == list_field {
            if let Some(sub_bytes) = f.value.as_bytes() {
                if let Ok(entry) = WireMessage::decode(sub_bytes) {
                    for ef in entry.fields {
                        if ef.field_number == 1 {
                            if let Some(u) = ef.value.as_bytes() {
                                if u.len() == 16 {
                                    return u.try_into().unwrap();
                                }
                                if let Ok(inner_wm) = WireMessage::decode(u) {
                                    if let Some(raw) = inner_wm.fields.iter().find(|x| x.field_number == 1).and_then(|x| x.value.as_bytes()) {
                                        if raw.len() == 16 {
                                            return raw.try_into().unwrap();
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    panic!("no 16-byte uuid in field {list_field}");
}

fn valid_coating_id(sde: &eve_sde::SdeRegistry) -> u32 {
    let cat = SkinrStaticCatalog::load(sde);
    let allowed_cats = cat.slot_allowed_categories.get(&1);
    cat.components
        .iter()
        .filter(|(_, c)| {
            !c.is_pattern()
                && !c.disallowed_slots.contains(&1)
                && allowed_cats.map_or(true, |ac| ac.is_empty() || ac.contains(&c.category))
        })
        .min_by_key(|(&id, _)| id)
        .map(|(&id, _)| id)
        .expect("valid coating component in SDE")
}

fn encode_coating_slot(slot_id: u32, comp_id: u32) -> Vec<u8> {
    let mut slot_msg = Vec::new();
    let mut s_idx = Vec::new();
    encode_varint_field(1, u64::from(slot_id), &mut s_idx);
    encode_bytes_field(1, &s_idx, &mut slot_msg);

    let mut coat_cfg = Vec::new();
    let mut c_idx = Vec::new();
    encode_varint_field(1, u64::from(comp_id), &mut c_idx);
    encode_bytes_field(1, &c_idx, &mut coat_cfg);

    let mut cfg_outer = Vec::new();
    encode_bytes_field(1, &coat_cfg, &mut cfg_outer);
    encode_bytes_field(2, &cfg_outer, &mut slot_msg);
    slot_msg
}

// parity: feat:wallet.evermarks
// parity: feat:quasar.cosmetic
// parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/market/api/api_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/market/component/listing/api/api_pb2.pyj
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paragon_evermarks_store_and_component_licenses() {
    let db_path = std::env::temp_dir().join(format!("evomu_shard4_em_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("sqlite open"));
    let ctx = ServerContext::with_repository(Arc::clone(&db));
    common::pin_clock(&ctx);

    let cid = 2_115_100_001i64;
    seed_pilot(&db, cid, 5001, "ParagonPilot", 98_000_001, 50_000_000_00, 120, 40_000);

    // Verify hull-scaled Paragon EverMark pricing across ship classes & SDE offers
    assert_eq!(emblem_evermark_price(&ctx.sde, 672, 31, 1), 2_500);
    assert_eq!(emblem_evermark_price(&ctx.sde, RIFTER_TYPE, 25, 0), 5_500);
    assert_eq!(emblem_evermark_price(&ctx.sde, THORAX_TYPE, 26, 0), 10_500);
    assert_eq!(emblem_evermark_price(&ctx.sde, RAVEN_TYPE, 27, 0), 15_500);
    assert_eq!(emblem_evermark_price(&ctx.sde, NAGLFAR_TYPE, 485, 0), 18_000);
    assert_eq!(emblem_evermark_price(&ctx.sde, RAGNAROK_TYPE, 30, 0), 26_000);
    let offers = paragon_sde_evermark_offers(&ctx.sde);
    assert!(offers.len() >= 1);

    // Negative check: Jita 4-4 does not host Paragon LP store (while character is still at Jita 4-4)
    let mut jita_player = sess(10, 5001, cid, 98_000_001, service_roles::ROLE_PLAYER_DEFAULT);
    let wrong_stn_err = ctx
        .dispatch_rpc(
            &mut jita_player,
            "LPStoreMgr",
            None,
            "TakeOfferForCharacter",
            &[PyRep::Integer(PARAGON_CORP), PyRep::Integer(17_208), PyRep::Integer(1)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(wrong_stn_err.code, "InvalidStationCorpForLPStore");

    // Dock at Jita VI - Paragon Fulfillment Center (60015169) & redeem EverMarks
    db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().station_id = Some(PARAGON_STATION);
    });
    let mut paragon_player = sess(11, 5001, cid, 98_000_001, service_roles::ROLE_PLAYER_DEFAULT);
    paragon_player.station_id = Some(PARAGON_STATION);
    let offers_rep = ctx
        .dispatch_rpc(
            &mut paragon_player,
            "LPStoreMgr",
            None,
            "GetAvailableOffersFromCorp",
            &[PyRep::Integer(PARAGON_CORP)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("Paragon GetAvailableOffersFromCorp");
    let PyRep::List(store_offers) = offers_rep else {
        panic!("expected Paragon LP offers list");
    };
    let first_offer_id = store_offers[0].dict_get("offerID").and_then(|v| v.as_i64()).expect("offerID");
    let first_em_cost = store_offers[0].dict_get("lpCost").and_then(|v| v.as_i64()).expect("lpCost");
    assert!(first_em_cost > 0);

    let take_rep = ctx
        .dispatch_rpc(
            &mut paragon_player,
            "LPStoreMgr",
            None,
            "TakeOfferForCharacter",
            &[PyRep::Integer(PARAGON_CORP), PyRep::Integer(first_offer_id), PyRep::Integer(1)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("Paragon TakeOfferForCharacter");
    let PyRep::KeyVal(take_kv) = take_rep else {
        panic!("expected KeyVal from TakeOfferForCharacter");
    };
    let em_after = db.with_snapshot(|s| s.characters[&cid].loyalty_points[&PARAGON_CORP]);
    assert_eq!(em_after, 40_000 - first_em_cost);
    assert_eq!(take_kv.get("remainingLP").and_then(|v| v.as_i64()), Some(em_after));

    // Non-GM player must be rejected on admin component grant
    assert_eq!(
        qrpc(&ctx, &mut jita_player, "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest", Vec::new()).await.status_code,
        STATUS_FORBIDDEN
    );

    // Titles, sequencing catalog pricing/durations & cosmetic market fee definitions
    assert_eq!(qrpc(&ctx, &mut jita_player, "eve_public.cosmetic.character.title.definition.api.GetLatestRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(qrpc(&ctx, &mut jita_player, "eve_public.cosmetic.character.title.api.GetOwnedRequest", Vec::new()).await.status_code, STATUS_OK);
    for ep in [
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest",
    ] {
        let rsp = qrpc(&ctx, &mut jita_player, ep, Vec::new()).await;
        assert_eq!(rsp.status_code, STATUS_OK, "{ep}");
        let wm = WireMessage::decode(&rsp.payload.unwrap().value).unwrap();
        assert_ne!(wm.fields.len(), 0);
    }

    let mut bf_req = Vec::new();
    put_isk(2, 10_000_000, &mut bf_req);
    assert_eq!(qrpc(&ctx, &mut jita_player, "eve_public.cosmetic.market.api.GetBrokerFeesRequest", bf_req).await.status_code, STATUS_OK);
    let bfees_rsp = qrpc(&ctx, &mut jita_player, "eve_public.cosmetic.market.api.GetBuyerFeesDefinitionRequest", Vec::new()).await;
    assert_eq!(bfees_rsp.status_code, STATUS_OK);
    let bfees_wm = WireMessage::decode(&bfees_rsp.payload.unwrap().value).unwrap();
    assert_eq!(bfees_wm.fields.iter().filter(|f| f.field_number == 1).count(), 10);

    let comps_rsp = qrpc(&ctx, &mut jita_player, "eve_public.cosmetic.market.component.listing.api.GetAllRequest", Vec::new()).await;
    assert_eq!(comps_rsp.status_code, STATUS_OK);
    let comps_wm = WireMessage::decode(&comps_rsp.payload.unwrap().value).unwrap();
    assert_ne!(comps_wm.fields.len(), 0);

    drop(ctx);
    drop(db);
    let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen sqlite");
    reopened.with_snapshot(|snap| {
        assert_eq!(snap.characters[&cid].loyalty_points[&PARAGON_CORP], em_after);
    });
    let _ = std::fs::remove_file(&db_path);
}

// parity: feat:cosmetics.skinr-designer
// parity: feat:cosmetics.skinr-sequencing-studio
// parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/skin/thirdparty/sequencing/job/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/market/component/listing/api/requests_pb2.pyj
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skinr_designer_and_sequencing_studio_jobs() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let cid = 2_115_100_021i64;
    seed_pilot(&ctx.db, cid, 5021, "SkinrDesigner", 98_000_001, 50_000_000_00, 150, 10_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid).or_default().insert(81377, TrainedSkillRecord {
            type_id: 81377,
            level: 3,
            skill_points: 40_000.0,
        });
    });

    assert_eq!(early_completion_plex_cents(0), 0);
    assert_eq!(early_completion_plex_cents(300), 200);
    assert_eq!(early_completion_plex_cents(3_600), 400);
    assert_eq!(early_completion_plex_cents(7_200), 800);

    let mut player = sess(20, 5021, cid, 98_000_001, service_roles::ROLE_PLAYER_DEFAULT);
    let mut rx = subscribe(&ctx, &mut player).await;

    // Purchase component listing with ISK & verify balance deduction
    let comps_rsp = qrpc(&ctx, &mut player, "eve_public.cosmetic.market.component.listing.api.GetAllRequest", Vec::new()).await;
    let comp_uuid = extract_first_uuid(&comps_rsp.payload.unwrap().value, 1);
    let isk_before_comp = ctx.db.with_snapshot(|s| s.characters[&cid].balance.0);
    let mut buy_comp = Vec::new();
    put_uuid(1, &comp_uuid, &mut buy_comp);
    encode_varint_field(2, 1, &mut buy_comp);
    assert_eq!(qrpc(&ctx, &mut player, "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest", buy_comp).await.status_code, STATUS_OK);
    let isk_after_comp = ctx.db.with_snapshot(|s| s.characters[&cid].balance.0);
    assert!(isk_after_comp < isk_before_comp, "purchasing component must deduct ISK");

    // Grant finite coating license & sequence custom Rifter SKIN
    let comp_id = valid_coating_id(&ctx.sde);
    let mut gm_player = sess(91, 5021, cid, 98_000_001, service_roles::ROLE_GM_DEFAULT);
    let mut fin_grant = Vec::new();
    encode_bytes_field(1, &encode_component_id(comp_id, false), &mut fin_grant);
    encode_varint_field(2, 3, &mut fin_grant);
    assert_eq!(qrpc(&ctx, &mut gm_player, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest", fin_grant).await.status_code, STATUS_OK);
    let owned_rsp = qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GetOwnedRequest", Vec::new()).await;
    assert_eq!(owned_rsp.status_code, STATUS_OK);
    let owned_wm = WireMessage::decode(&owned_rsp.payload.unwrap().value).unwrap();
    assert_ne!(owned_wm.fields.len(), 0);

    let mut ship_seq = Vec::new();
    encode_varint_field(1, u64::from(RIFTER_TYPE), &mut ship_seq);
    let mut layout = Vec::new();
    encode_bytes_field(1, &encode_coating_slot(1, comp_id), &mut layout);
    encode_bytes_field(2, &ship_seq, &mut layout);
    encode_varint_field(3, 1, &mut layout);

    let mut init_req = Vec::new();
    encode_string_field(1, "Crimson Rifter", &mut init_req);
    encode_string_field(2, "Crimson Line", &mut init_req);
    encode_bytes_field(3, &layout, &mut init_req);
    encode_varint_field(5, 1, &mut init_req);

    let init_rsp = qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", init_req).await;
    assert_eq!(init_rsp.status_code, STATUS_OK, "sequencing InitiateRequest failed: {}", init_rsp.status_message);

    let act_ship = qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest", Vec::new()).await;
    let job_uuid = extract_first_uuid(&act_ship.payload.unwrap().value, 1);
    let mut get_job_req = Vec::new();
    put_uuid(1, &job_uuid, &mut get_job_req);
    let job_get_rsp = qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetRequest", get_job_req).await;
    assert_eq!(job_get_rsp.status_code, STATUS_OK);
    let job_get_wm = WireMessage::decode(&job_get_rsp.payload.unwrap().value).unwrap();
    assert_ne!(job_get_wm.fields.len(), 0);

    let plex_before = ctx.db.with_snapshot(|s| s.unified_plex_balance(cid));
    let mut early_req = Vec::new();
    put_uuid(1, &job_uuid, &mut early_req);
    let early_rsp = qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest", early_req).await;
    assert_eq!(early_rsp.status_code, STATUS_OK, "CompleteEarlyRequest failed: {}", early_rsp.status_message);
    let plex_after = ctx.db.with_snapshot(|s| s.unified_plex_balance(cid));
    assert!(plex_after < plex_before, "early completion must deduct PLEX ({plex_before} -> {plex_after})");

    let notices = drain_notices(&mut rx);
    assert!(notices.iter().any(|n| n.contains("sequencing.job.api.")), "expected sequencing notices, got {notices:?}");

    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().plex_balance = 0;
        s.account_plex_vault.insert(5021, 0);
    });
    let mut no_plex_init = Vec::new();
    encode_string_field(1, "ZeroPlex Skin", &mut no_plex_init);
    encode_string_field(2, "ZeroPlex", &mut no_plex_init);
    encode_bytes_field(3, &layout, &mut no_plex_init);
    encode_varint_field(5, 1, &mut no_plex_init);
    assert_eq!(
        qrpc(&ctx, &mut player, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", no_plex_init).await.status_code,
        STATUS_BAD_REQUEST
    );
}

// parity: feat:cosmetics.skinr-market
// parity: feat:cosmetics.skinr-paragon-hub-listing
// parity: quasar:eveProto/generated/eve_public/cosmetic/market/skin/Order_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/api_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/market/transaction/api/notices_pb2.pyj
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skinr_market_paragon_hub_skin_listings_and_trades() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let seller_id = 2_115_100_011i64;
    let buyer_id = 2_115_100_012i64;
    seed_pilot(&ctx.db, seller_id, 5011, "ParagonSeller", 98_000_011, 20_000_000_00, 100, 50_000);
    seed_pilot(&ctx.db, buyer_id, 5012, "ParagonBuyer", 98_000_012, 50_000_000_00, 200, 50_000);

    let mut seller = sess(11, 5011, seller_id, 98_000_011, service_roles::ROLE_PLAYER_DEFAULT);
    let mut buyer = sess(12, 5012, buyer_id, 98_000_012, service_roles::ROLE_PLAYER_DEFAULT);
    let mut admin = sess(99, 5011, seller_id, 98_000_011, service_roles::ROLE_GM_DEFAULT);
    let mut buyer_rx = subscribe(&ctx, &mut buyer).await;

    let comp_id = valid_coating_id(&ctx.sde);
    let mut fin_grant = Vec::new();
    encode_bytes_field(1, &encode_component_id(comp_id, false), &mut fin_grant);
    encode_varint_field(2, 3, &mut fin_grant);
    assert_eq!(qrpc(&ctx, &mut admin, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest", fin_grant).await.status_code, STATUS_OK);

    let mut ship_seq = Vec::new();
    encode_varint_field(1, u64::from(RIFTER_TYPE), &mut ship_seq);
    let mut layout = Vec::new();
    encode_bytes_field(1, &encode_coating_slot(1, comp_id), &mut layout);
    encode_bytes_field(2, &ship_seq, &mut layout);
    encode_varint_field(3, 1, &mut layout);
    let mut init_req = Vec::new();
    encode_string_field(1, "Market Rifter", &mut init_req);
    encode_string_field(2, "Market Line", &mut init_req);
    encode_bytes_field(3, &layout, &mut init_req);
    encode_varint_field(5, 1, &mut init_req);
    let init_rsp = qrpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", init_req).await;
    assert_eq!(init_rsp.status_code, STATUS_OK);
    let init_wm = WireMessage::decode(&init_rsp.payload.unwrap().value).unwrap();
    let job_attrs_wm = WireMessage::decode(init_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_bytes()).expect("job_attrs")).unwrap();
    let skin_id_wm = WireMessage::decode(job_attrs_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()).expect("skin_id")).unwrap();
    let skin_hex = std::str::from_utf8(skin_id_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()).expect("skin hex")).unwrap().to_string();
    assert_ne!(skin_hex.len(), 0);

    let seller_jobs = qrpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest", Vec::new()).await;
    let seller_job_uuid = extract_first_uuid(&seller_jobs.payload.unwrap().value, 1);
    let mut seller_early = Vec::new();
    put_uuid(1, &seller_job_uuid, &mut seller_early);
    assert_eq!(qrpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest", seller_early).await.status_code, STATUS_OK);

    let lic_rsp = qrpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest", Vec::new()).await;
    let lic_wm = WireMessage::decode(&lic_rsp.payload.unwrap().value).unwrap();
    assert_ne!(lic_wm.fields.len(), 0);

    let mut lic_id = Vec::new();
    put_seq(1, seller_id as u64, &mut lic_id);
    let mut sh_msg = Vec::new();
    encode_string_field(1, &skin_hex, &mut sh_msg);
    encode_bytes_field(2, &sh_msg, &mut lic_id);

    let mut create_req = Vec::new();
    put_isk(1, 1_000_000, &mut create_req);
    encode_bytes_field(3, &lic_id, &mut create_req);
    put_seconds(4, 86_400, &mut create_req);
    encode_varint_field(5, 1, &mut create_req);

    let seller_isk_before = ctx.db.with_snapshot(|s| s.characters[&seller_id].balance.0);
    let create_rsp = qrpc(&ctx, &mut seller, "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest", create_req).await;
    assert_eq!(create_rsp.status_code, STATUS_OK, "create SKIN listing failed: {}", create_rsp.status_message);
    let seller_isk_after_fee = ctx.db.with_snapshot(|s| s.characters[&seller_id].balance.0);
    assert!(seller_isk_after_fee < seller_isk_before, "creating listing must deduct broker fee");

    let all_skins = qrpc(&ctx, &mut buyer, "eve_public.cosmetic.market.skin.listing.api.GetAllRequest", Vec::new()).await;
    assert_eq!(all_skins.status_code, STATUS_OK);
    let listing_uuid = extract_first_uuid(&all_skins.payload.unwrap().value, 1);

    let buyer_isk_before = ctx.db.with_snapshot(|s| s.characters[&buyer_id].balance.0);
    let mut buy_skin = Vec::new();
    put_uuid(1, &listing_uuid, &mut buy_skin);
    encode_varint_field(2, 1, &mut buy_skin);
    let buy_skin_rsp = qrpc(&ctx, &mut buyer, "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest", buy_skin).await;
    assert_eq!(buy_skin_rsp.status_code, STATUS_OK, "purchase SKIN listing failed: {}", buy_skin_rsp.status_message);

    let buyer_isk_after = ctx.db.with_snapshot(|s| s.characters[&buyer_id].balance.0);
    let seller_isk_after_sale = ctx.db.with_snapshot(|s| s.characters[&seller_id].balance.0);
    assert_eq!(buyer_isk_before - buyer_isk_after, 1_000_000_00);
    assert!(seller_isk_after_sale > seller_isk_after_fee, "seller must receive net sale proceeds");

    let buyer_lic_rsp = qrpc(&ctx, &mut buyer, "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest", Vec::new()).await;
    let buyer_lic_wm = WireMessage::decode(&buyer_lic_rsp.payload.unwrap().value).unwrap();
    assert_eq!(buyer_lic_wm.fields.iter().filter(|f| f.field_number == 1).count(), 1);

    let tx_notices = drain_notices(&mut buyer_rx);
    assert!(tx_notices.iter().any(|n| n.contains("cosmetic.market.transaction.api.")), "expected transaction notice, got {tx_notices:?}");
}
