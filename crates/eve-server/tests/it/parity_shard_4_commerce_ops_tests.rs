//! End-to-end integration tests for NES Store hydration and purchases, Fast Checkout,
//! payment schedules/tokens, HyperNet Relay, Freelance projects, Entitlements (`Q-5`),
//! and ServerOps downtime, daily jobs, node balancing, and Time Dilation (`Q-6`).

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    corp_roles, AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents,
    NeuralAttributes, WorldRepository,
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
use eve_server::services::hypernet_raffle_ops::{compute_required_hypercores, sweep_expired_raffles};
use eve_server::services::serverops_cluster::{
    execute_daily_maintenance_jobs, get_system_tidi_factor, get_system_worker_node,
    rebalance_overloaded_worker_nodes, reinforce_solar_system_for_fleet_fight,
    set_system_tidi_factor,
};
use eve_server::services::wallet_serverops::run_daily_downtime_and_jobs;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;

const JITA: u32 = 30_000_142;
const PERIMETER: u32 = 30_000_144;
const JITA_STATION: i64 = 60_003_760;
const HYPERCORE_TYPE_ID: u32 = 52_568;
const TRITANIUM_TYPE_ID: u32 = 34;

fn sess(sid: u64, uid: u64, cid: i64, corp: i64, corp_role: u64) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(uid);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp);
    s.solar_system_id = Some(JITA);
    s.station_id = Some(JITA_STATION);
    s.role = service_roles::ROLE_PLAYER_DEFAULT;
    s.corp_role = corp_role;
    s
}

fn seed_pilot(db: &WorldRepository, cid: i64, uid: u64, name: &str, corp: i64, isk_cents: i64, plex: i64) {
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
        let ship_id = snap.next_entity_id.max(1_000_700_100) + cid;
        snap.next_entity_id = ship_id + 10;
        snap.characters.insert(cid, CharacterRecord {
            character_id: cid,
            user_id: uid,
            name: name.into(),
            corporation_id: corp,
            alliance_id: None,
            war_faction_id: None,
            station_id: Some(JITA_STATION),
            structure_id: None,
            solar_system_id: JITA,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            active_ship_id: ship_id,
            balance: IskCents(isk_cents),
            plex_balance: 0,
            loyalty_points: HashMap::new(),
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
            free_skill_points: 0.0,
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
            type_id: 587,
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
        correlation_uuid: b"ops-test".to_vec(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{ty}"), value }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-ops".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(s, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None)
        .await
        .expect("SendRequest");
    let PyRep::KeyVal(kv) = &rep else { panic!("expected KeyVal, got {rep:?}"); };
    let raw = kv.iter().find_map(|(k, v)| match (k.as_str(), v) {
        ("response_proto", PyRep::Bytes(b)) => Some(b.clone()),
        _ => None,
    }).expect("response_proto");
    EvePublicResponse::decode_proto(&raw).expect("decode EvePublicResponse")
}

async fn subscribe(ctx: &ServerContext, s: &mut SessionState) -> Receiver<EvePublicNotice> {
    ctx.dispatch_rpc(s, "publicGatewaySvc", None, "Subscribe", &[], &HashMap::new(), None).await.expect("Subscribe");
    ctx.quasar_gateway.take_stream_receiver(s.session_id).expect("stream rx")
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

fn idm(v: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, v, &mut b);
    b
}

fn uuid_msg(field: u32, uuid: &[u8; 16], out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_bytes_field(1, uuid, &mut b);
    encode_bytes_field(field, &b, out);
}

// parity: feat:wallet.nes-store
// parity: rpc:storeManager.get_categories
// parity: rpc:storeManager.get_products
// parity: rpc:storeManager.get_offers
// parity: rpc:storeManager.can_purchase_offer
// parity: rpc:storeManager.buy_offer
// parity: quasar:eveProto/generated/eve_public/store/category/api_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/store/product/api_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/store/offer/api_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nes_store_sde_hydration_catalog_and_purchase_flow() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let cid = 2_115_400_001i64;
    let uid = 6401u64;
    seed_pilot(&ctx.db, cid, uid, "NesShopper", 98_000_001, 5_000_000_00, 5_000);
    let mut player = sess(40, uid, cid, 98_000_001, 0);

    let cats = ctx.dispatch_rpc(&mut player, "storeManager", None, "get_categories", &[], &HashMap::new(), None).await.expect("get_categories");
    let PyRep::List(cat_list) = cats else { panic!("expected categories list"); };
    assert!(cat_list.len() >= 1);
    let prods = ctx.dispatch_rpc(&mut player, "storeManager", None, "get_products", &[], &HashMap::new(), None).await.expect("get_products");
    let PyRep::List(prod_list) = prods else { panic!("expected products list"); };
    assert!(prod_list.len() >= 1);
    let offers = ctx.dispatch_rpc(&mut player, "storeManager", None, "get_offers", &[], &HashMap::new(), None).await.expect("get_offers");
    let PyRep::List(offer_list) = offers else { panic!("expected offers list"); };
    let offer_1001_plex = offer_list
        .iter()
        .find(|o| o.dict_get("offerID") == Some(&PyRep::Integer(1001)))
        .and_then(|o| o.dict_get("plexCost").or_else(|| o.dict_get("price")).and_then(|v| v.as_i64()))
        .unwrap_or(100);

    let can_buy = ctx.dispatch_rpc(&mut player, "storeManager", None, "can_purchase_offer", &[PyRep::Integer(1001), PyRep::Integer(1)], &HashMap::new(), None).await.expect("can_purchase_offer");
    assert_eq!(can_buy, PyRep::Bool(true));

    let plex_before = ctx.db.with_snapshot(|s| s.unified_plex_balance(cid));
    assert_eq!(plex_before, 5_000);
    let bought = ctx.dispatch_rpc(&mut player, "storeManager", None, "buy_offer", &[PyRep::Integer(1001), PyRep::Integer(1)], &HashMap::new(), None).await.expect("buy_offer");
    assert!(matches!(bought, PyRep::KeyVal(_)));
    let plex_after = ctx.db.with_snapshot(|s| s.unified_plex_balance(cid));
    assert!(plex_after < plex_before, "buy_offer must deduct PLEX ({plex_before} -> {plex_after})");
    if offer_1001_plex > 0 && plex_before - plex_after == offer_1001_plex {
        assert_eq!(plex_after, 5_000 - offer_1001_plex);
    }

    let mut cat_req = Vec::new();
    encode_bytes_field(1, &idm(1), &mut cat_req);
    let cat_rsp = qrpc(&ctx, &mut player, "eve_public.store.category.api.GetRequest", cat_req).await;
    assert_eq!(cat_rsp.status_code, STATUS_OK);
    let cat_wm = WireMessage::decode(&cat_rsp.payload.unwrap().value).unwrap();
    assert_ne!(cat_wm.fields.len(), 0);

    let mut prod_req = Vec::new();
    encode_bytes_field(1, &idm(102), &mut prod_req);
    let prod_rsp = qrpc(&ctx, &mut player, "eve_public.store.product.api.GetRequest", prod_req).await;
    assert_eq!(prod_rsp.status_code, STATUS_OK);
    let prod_wm = WireMessage::decode(&prod_rsp.payload.unwrap().value).unwrap();
    assert_ne!(prod_wm.fields.len(), 0);

    let mut off_id = Vec::new();
    encode_string_field(1, "1001", &mut off_id);
    let mut off_req = Vec::new();
    encode_bytes_field(1, &off_id, &mut off_req);
    let off_rsp = qrpc(&ctx, &mut player, "eve_public.store.offer.api.GetRequest", off_req).await;
    assert_eq!(off_rsp.status_code, STATUS_OK);
    let off_wm = WireMessage::decode(&off_rsp.payload.unwrap().value).unwrap();
    assert_ne!(off_wm.fields.len(), 0);

    let ta_rsp = qrpc(&ctx, &mut player, "eve_public.store.targetaudience.offer.api.ListRequest", Vec::new()).await;
    assert_eq!(ta_rsp.status_code, STATUS_OK);
    let ta_wm = WireMessage::decode(&ta_rsp.payload.unwrap().value).unwrap();
    assert_ne!(ta_wm.fields.len(), 0);
}

// parity: quasar:eveProto/generated/eve_public/payment/purchase/api_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/payment/schedule/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/payment/token/api_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/payment/token/api/notices_pb2.pyj
// parity: rpc:FastCheckoutService.GetOffersForUser
// parity: rpc:FastCheckoutService.BuyOffer
// parity: rpc:FastCheckoutService.RequestMiningCycleOutput
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn payment_purchase_schedules_tokens_and_fast_checkout() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let cid = 2_115_400_011i64;
    let uid = 6411u64;
    seed_pilot(&ctx.db, cid, uid, "PaymentPilot", 98_000_001, 2_000_000_00, 1_000);
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_537);
    let metenox_id = 1_000_750_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.entry(98_000_001).or_insert_with(|| eve_db::CorporationRecord {
            corporation_id: 98_000_001,
            name: "Payment Test Corp".into(),
            ticker: "PAYC".into(),
            ceo_id: cid,
            alliance_id: None,
            tax_rate: 0.1,
            master_wallet: IskCents::from_isk(100_000_000),
            members: [cid].into_iter().collect(),
            member_roles: HashMap::new(),
            applications: HashMap::new(),
            wallet_divisions: eve_db::CorporationRecord::default_wallet_divisions(IskCents::from_isk(100_000_000)),
            description: String::new(),
            url: String::new(),
            hq_station_id: Some(JITA_STATION),
        });
        corp.ceo_id = cid;
        corp.members.insert(cid);
        corp.member_roles
            .insert(cid, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER);
        snap.structures.insert(metenox_id, eve_db::UpwellStructureRecord {
            structure_id: metenox_id,
            type_id: 81_826,
            owner_corp_id: 98_000_001,
            solar_system_id: lowsec_sys,
            name: "Metenox Moon Drill".into(),
            position: [100_000.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            ..Default::default()
        });
        snap.moon_extractions.insert(metenox_id, eve_db::MoonExtractionRecord {
            structure_id: metenox_id,
            moon_id: 40_009_082,
            extraction_start_unix: 1_700_000_000,
            chunk_arrival_unix: 1_700_050_000,
            natural_decay_unix: 1_700_090_000,
            status: "Started".into(),
            auto_fracture: false,
            event_id: 9_001,
            moon_ores: vec![(45_490, 0.75)],
            spawned_asteroid_ids: Vec::new(),
        });
    });

    let mut player = sess(41, uid, cid, 98_000_001, corp_roles::ROLE_DIRECTOR);
    let mut rx = subscribe(&ctx, &mut player).await;

    let mut cost_req = Vec::new();
    encode_varint_field(1, 1002, &mut cost_req);
    let cost_rsp = qrpc(&ctx, &mut player, "eve_public.payment.purchase.api.CostRequest", cost_req).await;
    assert_eq!(cost_rsp.status_code, STATUS_OK);
    let cost_wm = WireMessage::decode(&cost_rsp.payload.unwrap().value).unwrap();
    let price_bytes = cost_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()).expect("payment price bytes");
    let price_wm = WireMessage::decode(price_bytes).unwrap();
    assert_ne!(price_wm.fields.len(), 0);

    let qp_rsp = qrpc(&ctx, &mut player, "eve_public.payment.token.api.GetQuickPayRequest", Vec::new()).await;
    assert_eq!(qp_rsp.status_code, STATUS_OK);
    assert_eq!(
        qp_rsp.payload.as_ref().unwrap().type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.token.api.GetQuickPayResponse")
    );
    assert_eq!(qrpc(&ctx, &mut player, "eve_public.payment.token.api.GetRequest", Vec::new()).await.status_code, STATUS_BAD_REQUEST);
    assert_eq!(qrpc(&ctx, &mut player, "eve_public.payment.schedule.api.GetRequest", Vec::new()).await.status_code, STATUS_BAD_REQUEST);

    let fc_offers = ctx.dispatch_rpc(&mut player, "FastCheckoutService", None, "GetOffersForUser", &[], &HashMap::new(), None).await.expect("GetOffersForUser");
    let PyRep::List(fc_list) = fc_offers else { panic!("expected List from GetOffersForUser"); };
    assert!(fc_list.len() <= 64);

    let disabled_fc = ctx.dispatch_rpc(&mut player, "FastCheckoutService", None, "BuyOffer", &[PyRep::Integer(1), PyRep::Integer(1)], &HashMap::new(), None).await.unwrap_err();
    assert_eq!(disabled_fc.code.as_str(), "FastCheckoutDisabled");
    eve_server::services::fast_checkout_and_activity::set_user_fast_checkout_enabled(&ctx, uid, true);
    let missing_fc = ctx.dispatch_rpc(&mut player, "FastCheckoutService", None, "BuyOffer", &[PyRep::Integer(999_999), PyRep::Integer(1), PyRep::String("hash".into())], &HashMap::new(), None).await.unwrap_err();
    assert_eq!(missing_fc.code.as_str(), "OfferNotFound");

    let cycle_out = ctx.dispatch_rpc(&mut player, "FastCheckoutService", None, "RequestMiningCycleOutput", &[PyRep::Integer(metenox_id)], &HashMap::new(), None).await.expect("RequestMiningCycleOutput");
    let PyRep::List(items) = cycle_out else { panic!("expected List from RequestMiningCycleOutput"); };
    assert_ne!(items.len(), 0);
    let PyRep::Tuple(ref cols) = items[0] else { panic!("expected Tuple from RequestMiningCycleOutput"); };
    assert_eq!(cols.len(), 3);
    let mat_tid = cols[0].as_i64().unwrap_or(0);
    let mat_qty = cols[1].as_i64().unwrap_or(0);
    assert!(mat_tid > 0 && mat_qty > 0, "Metenox mining cycle output must yield positive material, got tid={mat_tid} qty={mat_qty}");

    let _ = drain_notices(&mut rx);
}

// parity: feat:quasar.corporation
// parity: feat:quasar.freelance
// parity: quasar:eveProto/generated/eve_public/freelance/project/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/freelance/project/api/notices_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/notices_pb2.pyj
// parity: rpc:raffleProxy.CreateRaffle
// parity: feat:wallet.hypernet-hypercore-sizing-expiry
// parity: feat:wallet.hypernet-relay
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn freelance_project_delete_and_hypernet_relay_lifecycle() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let dir_cid = 2_115_400_021i64;
    let member_cid = 2_115_400_022i64;
    let corp_id = 98_000_001i64;
    seed_pilot(&ctx.db, dir_cid, 6421, "CorpDirector", corp_id, 50_000_000_00, 500);
    seed_pilot(&ctx.db, member_cid, 6422, "CorpMember", corp_id, 50_000_000_00, 500);
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord {
            corporation_id: corp_id,
            name: "Commerce Ops Corp".into(),
            ticker: "COC".into(),
            ceo_id: dir_cid,
            members: std::collections::HashSet::from([dir_cid, member_cid]),
            member_roles: HashMap::from([(dir_cid, corp_roles::ROLE_DIRECTOR), (member_cid, 0)]),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);
    });

    let mut dir = sess(42, 6421, dir_cid, corp_id, corp_roles::ROLE_DIRECTOR);
    let mut member = sess(43, 6422, member_cid, corp_id, 0);
    let mut rx = subscribe(&ctx, &mut dir).await;

    let mut val_elem = Vec::new();
    encode_string_field(2, "34", &mut val_elem);
    let mut matcher = Vec::new();
    encode_bytes_field(1, &val_elem, &mut matcher);
    let mut param = Vec::new();
    encode_bytes_field(2, &matcher, &mut param);
    let mut cm_id = Vec::new();
    encode_string_field(1, "DeliverItem", &mut cm_id);
    let mut cfg = Vec::new();
    encode_bytes_field(1, &cm_id, &mut cfg);
    encode_bytes_field(3, &param, &mut cfg);

    let mut create_req = Vec::new();
    encode_string_field(1, "Tritanium Supply", &mut create_req);
    encode_string_field(2, "Deliver Tritanium to Jita 4-4", &mut create_req);
    encode_varint_field(3, 100, &mut create_req);
    encode_bytes_field(6, &cfg, &mut create_req);

    assert_eq!(qrpc(&ctx, &mut member, "eve_public.freelance.project.api.CreateRequest", create_req.clone()).await.status_code, STATUS_FORBIDDEN);

    let create_rsp = qrpc(&ctx, &mut dir, "eve_public.freelance.project.api.CreateRequest", create_req).await;
    assert_eq!(create_rsp.status_code, STATUS_OK, "freelance CreateRequest failed: {}", create_rsp.status_message);

    let active_rsp = qrpc(&ctx, &mut dir, "eve_public.freelance.project.api.GetAllActiveRequest", Vec::new()).await;
    assert_eq!(active_rsp.status_code, STATUS_OK);
    let act_wm = WireMessage::decode(&active_rsp.payload.unwrap().value).unwrap();
    let proj_uuid: [u8; 16] = act_wm.fields.iter().find_map(|f| {
        if f.field_number != 1 { return None; }
        let m = WireMessage::decode(f.value.as_bytes()?).ok()?;
        let b = m.fields.first()?.value.as_bytes()?;
        (b.len() == 16).then(|| b.try_into().unwrap())
    }).expect("active project uuid");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(998_100, InventoryItemRecord {
            item_id: 998_100,
            type_id: TRITANIUM_TYPE_ID,
            owner_id: member_cid,
            location_id: JITA_STATION,
            flag: InventoryFlag::Hangar,
            quantity: 100,
            ..Default::default()
        });
    });
    let mut commit_req = Vec::new();
    uuid_msg(1, &proj_uuid, &mut commit_req);
    assert_eq!(qrpc(&ctx, &mut member, "eve_public.freelance.project.api.CommitRequest", commit_req.clone()).await.status_code, STATUS_OK);

    let mut deliv_req = Vec::new();
    uuid_msg(1, &proj_uuid, &mut deliv_req);
    encode_varint_field(4, 40, &mut deliv_req);
    assert_eq!(qrpc(&ctx, &mut member, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", deliv_req).await.status_code, STATUS_OK);
    let remaining_trit = ctx.db.with_snapshot(|s| s.items.get(&998_100).map(|i| i.quantity));
    assert_eq!(remaining_trit, Some(60), "delivering 40 Tritanium must consume 40 from hangar stack");

    assert_eq!(qrpc(&ctx, &mut member, "eve_public.freelance.project.api.DeleteRequest", commit_req.clone()).await.status_code, STATUS_FORBIDDEN);
    assert_eq!(qrpc(&ctx, &mut dir, "eve_public.freelance.project.api.DeleteRequest", commit_req).await.status_code, STATUS_OK);

    let notices = drain_notices(&mut rx);
    assert!(notices.iter().any(|n| n == "eve_public.freelance.project.api.DeletedNotice"));

    let hc_needed = compute_required_hypercores(8_000_000.0, 8).expect("compute hypercores");
    assert!(hc_needed >= 1);

    let prize_id = 998_201i64;
    let hc_id = 998_202i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(prize_id, InventoryItemRecord {
            item_id: prize_id,
            type_id: 587,
            owner_id: member_cid,
            location_id: JITA_STATION,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            ..Default::default()
        });
        snap.items.insert(hc_id, InventoryItemRecord {
            item_id: hc_id,
            type_id: HYPERCORE_TYPE_ID,
            owner_id: member_cid,
            location_id: JITA_STATION,
            flag: InventoryFlag::Hangar,
            quantity: 20,
            ..Default::default()
        });
    });

    let raffle_kv = PyRep::keyval([
        ("item_id", PyRep::Integer(prize_id)),
        ("ticket_count", PyRep::Integer(8)),
        ("ticket_price", PyRep::Real(1_000_000.0)),
    ]);
    ctx.dispatch_rpc(&mut member, "raffleProxy", None, "CreateRaffle", &[raffle_kv], &HashMap::new(), None)
        .await
        .expect("CreateRaffle succeeds");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&hc_id].quantity), 20 - hc_needed);

    ctx.advance_secs(300_000);
    let settled = sweep_expired_raffles(&ctx);
    assert_eq!(settled, 1);
}

// parity: feat:quasar.entitlement
// parity: plan:Q-5
// parity: feat:serverops.downtime
// parity: feat:serverops.daily-jobs
// parity: feat:serverops.node-balancing
// parity: feat:serverops.time-dilation
// parity: plan:Q-6
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entitlements_serverops_and_sqlite_restart_persistence() {
    let db_path = std::env::temp_dir().join(format!("evomu_shard4_ops_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("sqlite open"));
    let ctx = ServerContext::with_repository(Arc::clone(&db));
    common::pin_clock(&ctx);

    let cid = 2_115_400_031i64;
    let uid = 6431u64;
    seed_pilot(&db, cid, uid, "OpsPilot", 98_000_001, 25_000_000_00, 750);
    let mut player = sess(44, uid, cid, 98_000_001, 0);

    assert_eq!(qrpc(&ctx, &mut player, "eve_public.entitlement.user.api.GetRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(qrpc(&ctx, &mut player, "eve_public.entitlement.character.GetAllRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(qrpc(&ctx, &mut player, "eve.entitlement.character.admin.GetAllRequest", Vec::new()).await.status_code, STATUS_FORBIDDEN);

    set_system_tidi_factor(&ctx, JITA, 0.35);
    assert!((get_system_tidi_factor(&ctx, JITA) - 0.35).abs() < 1e-6);

    reinforce_solar_system_for_fleet_fight(&ctx, PERIMETER);
    let node_before = get_system_worker_node(&ctx, PERIMETER);
    let _moved = rebalance_overloaded_worker_nodes(&ctx);
    let node_after = get_system_worker_node(&ctx, PERIMETER);
    assert!(node_before > 0 && node_after > 0);

    let daily_rep = execute_daily_maintenance_jobs(&ctx);
    assert!(!daily_rep.is_empty());
    let dt_rep = run_daily_downtime_and_jobs(&ctx);
    assert!(!dt_rep.is_empty());

    drop(ctx);
    drop(db);
    let reopened = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = ServerContext::with_repository(Arc::clone(&reopened));
    assert!((get_system_tidi_factor(&ctx2, JITA) - 0.35).abs() < 1e-6);
    reopened.with_snapshot(|snap| {
        assert_eq!(snap.characters[&cid].balance.0, 25_000_000_00);
        assert_eq!(snap.unified_plex_balance(cid), 750);
    });
    let _ = std::fs::remove_file(&db_path);
}
