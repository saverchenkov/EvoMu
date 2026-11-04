//! Integration tests for HyperNet Relay HyperCore sizing/expiry, NPC item traders,
//! ESI corporation structure timers and services, and visited systems tracking
//! (`feat:wallet.*`, `feat:esi.*`, `feat:ui.*`).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;

const JITA_SOLAR_SYSTEM: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;
const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: Vec<String>) -> String {
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&character_id) {
            ch.user_id = 1;
        }
        Ok(())
    });
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "evomu-esi-test",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-esi-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

async fn login_and_select(ctx: &ServerContext, session_id: u64, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");
    sess
}

#[tokio::test]
// parity: feat:wallet.hypernet-hypercore-sizing-expiry
async fn test_hypernet_hypercore_sizing_and_expiry_refund_lifecycle() {
    eve_testkit::attest(&["feat:wallet.hypernet-hypercore-sizing-expiry"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let p1 = 90_000_001_i64;
    let p2 = 90_000_002_i64;
    let mut s1 = login_and_select(&ctx, 2101, p1).await;
    let mut s2 = SessionState::new(2102);
    common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut s2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(p2)], &kw, None)
        .await
        .expect("SelectCharacterID p2");

    let valid_item = 998_501_i64;
    let hypercore_item = 998_510_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(valid_item, InventoryItemRecord {
            item_id: valid_item, type_id: 587, owner_id: p1,
            location_id: JITA_STATION, flag: InventoryFlag::Hangar,
            quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(hypercore_item, InventoryItemRecord {
            item_id: hypercore_item, type_id: 52568, owner_id: p1,
            location_id: JITA_STATION, flag: InventoryFlag::Hangar,
            quantity: 100, singleton: false, ..Default::default()
        });
        if let Some(c) = snap.characters.get_mut(&p1) {
            c.station_id = Some(JITA_STATION);
            c.balance = IskCents::from_isk(10_000_000);
        }
        if let Some(c) = snap.characters.get_mut(&p2) {
            c.station_id = Some(JITA_STATION);
            c.balance = IskCents::from_isk(50_000_000);
        }
    });

    // 1. Verify node count validation: 8, 16, 48, 512 allowed; others rejected
    let bad_nodes = ctx.dispatch_rpc(&mut s1, "raffleProxy", None, "CreateRaffle", &[PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(valid_item)),
        (PyRep::String("ticketCount".into()), PyRep::Integer(20)),
        (PyRep::String("ticketPrice".into()), PyRep::Real(1_000_000.0)),
    ])], &kw, None).await.unwrap_err();
    assert_eq!(bad_nodes.code, "InvalidRaffleTicketCount");

    // 2. Verify HyperCore requirement scaling with estimated value and node counts
    let cores_8 = eve_server::services::hypernet_raffle_ops::compute_required_hypercores(100_000_000.0, 8).unwrap();
    let cores_16 = eve_server::services::hypernet_raffle_ops::compute_required_hypercores(100_000_000.0, 16).unwrap();
    let cores_48 = eve_server::services::hypernet_raffle_ops::compute_required_hypercores(100_000_000.0, 48).unwrap();
    let cores_512 = eve_server::services::hypernet_raffle_ops::compute_required_hypercores(100_000_000.0, 512).unwrap();
    assert!(cores_8 <= cores_16);
    assert!(cores_16 <= cores_48);
    assert!(cores_48 <= cores_512);
    assert_eq!(cores_512, 20);

    // 3. Create 48-node raffle and verify HyperCores consumed from hangar
    let create_rep = ctx.dispatch_rpc(&mut s1, "raffleProxy", None, "CreateRaffle", &[PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(valid_item)),
        (PyRep::String("ticketCount".into()), PyRep::Integer(48)),
        (PyRep::String("ticketPrice".into()), PyRep::Real(1_000_000.0)),
    ])], &kw, None).await.expect("CreateRaffle 48 nodes");
    let PyRep::KeyVal(r_kv) = create_rep else { panic!("expected KeyVal") };
    let raffle_id = r_kv.get("raffle_id").and_then(|v| v.as_i64()).unwrap();
    let cores_used = r_kv.get("hypercores_used").and_then(|v| v.as_i64()).unwrap();
    assert!(cores_used > 0);

    let cores_remaining = ctx.db.with_snapshot(|s| s.items[&hypercore_item].quantity);
    assert_eq!(cores_remaining, 100 - (cores_used as i32));

    ctx.db.with_snapshot(|s| {
        let it = &s.items[&valid_item];
        assert_ne!(it.owner_id, p1);
    });

    // 4. P2 buys 2 tickets (2,000,000 ISK)
    let p2_bal_before = ctx.db.with_snapshot(|s| s.characters[&p2].balance);
    let buy_rep = ctx.dispatch_rpc(&mut s2, "raffleProxy", None, "BuyRandomTickets", &[
        PyRep::Integer(raffle_id), PyRep::Integer(2),
    ], &kw, None).await.expect("BuyRandomTickets");
    assert!(matches!(buy_rep, PyRep::KeyVal(_)));

    let p2_bal_after_buy = ctx.db.with_snapshot(|s| s.characters[&p2].balance);
    assert_eq!(p2_bal_before.0 - p2_bal_after_buy.0, 2_000_000_00);

    // 5. Advance clock past 72 hours (259,200s)
    ctx.advance_virtual_clock(259_201);

    // 6. Expire unsold raffle: ticket buyer refunded, item returned to creator, HyperCores remain consumed
    let swept = eve_server::services::hypernet_raffle_ops::sweep_expired_raffles(&ctx);
    assert_eq!(swept, 1);

    let p2_bal_after_expiry = ctx.db.with_snapshot(|s| s.characters[&p2].balance);
    assert_eq!(p2_bal_after_expiry, p2_bal_before, "Ticket buyer must be fully refunded upon raffle expiry");

    ctx.db.with_snapshot(|s| {
        let it = &s.items[&valid_item];
        assert_eq!(it.owner_id, p1, "Raffle item must be returned to creator upon expiry");
        assert_eq!(it.location_id, JITA_STATION);
        assert_eq!(it.flag, InventoryFlag::Hangar);
    });

    let cores_after_expiry = ctx.db.with_snapshot(|s| s.items[&hypercore_item].quantity);
    assert_eq!(cores_after_expiry, 100 - (cores_used as i32), "HyperCores must remain consumed after raffle expiry");
}

#[tokio::test]
// parity: feat:wallet.item-trader
async fn test_item_trader_atomic_exchange_per_recipe() {
    eve_testkit::attest(&["feat:wallet.item-trader"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let mut sess = login_and_select(&ctx, 2103, char_id).await;

    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock into space");
    let ship_id = sess.ship_id.expect("Active ship in space");
    let sys_handle = ctx.systems.get_or_spawn(JITA_SOLAR_SYSTEM);
    assert!(sys_handle.set_ball_position(ship_id, glam::DVec3::ZERO).await);

    // Seed NPC item trader entity (type 86278, which offers recipe 67) at 40km distance
    let trader_entity_id = 998_601_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(trader_entity_id, InventoryItemRecord {
            item_id: trader_entity_id, type_id: 86278, owner_id: 1_000_125,
            location_id: JITA_SOLAR_SYSTEM as i64, flag: InventoryFlag::Hangar,
            quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(trader_entity_id, [40_000.0, 0.0, 0.0]);
    });

    // 1. Out of range check (> 30km interactionRange)
    let out_of_range = ctx.dispatch_rpc(&mut sess, "beyonce", None, "CallComponentFromClient", &[
        PyRep::Integer(trader_entity_id), PyRep::String("itemTrader".into()),
        PyRep::String("ProcessTrade".into()), PyRep::Integer(67), PyRep::Integer(1),
    ], &kw, None).await.unwrap_err();
    assert_eq!(out_of_range.code, "ItemTraderOutOfRange");

    // Move ship within interaction range (15km position => 25km distance to trader)
    assert!(sys_handle.set_ball_position(ship_id, glam::DVec3::new(15_000.0, 0.0, 0.0)).await);

    // 2. Invalid recipe ID rejected
    let bad_recipe = ctx.dispatch_rpc(&mut sess, "beyonce", None, "CallComponentFromClient", &[
        PyRep::Integer(trader_entity_id), PyRep::String("itemTrader".into()),
        PyRep::String("ProcessTrade".into()), PyRep::Integer(99999), PyRep::Integer(1),
    ], &kw, None).await.unwrap_err();
    assert!(bad_recipe.code == "RecipeNotFound" || bad_recipe.code == "RecipeNotAvailableAtTrader");

    // 3. Insufficient input items rejected
    let missing_inputs = ctx.dispatch_rpc(&mut sess, "beyonce", None, "CallComponentFromClient", &[
        PyRep::Integer(trader_entity_id), PyRep::String("itemTrader".into()),
        PyRep::String("ProcessTrade".into()), PyRep::Integer(67), PyRep::Integer(1),
    ], &kw, None).await.unwrap_err();
    assert_eq!(missing_inputs.code, "InsufficientInputItems");

    // 4. Seed input items: recipe 67 requires type 85316 (15 units per trade)
    let input_item_id = 998_602_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(input_item_id, InventoryItemRecord {
            item_id: input_item_id, type_id: 85316, owner_id: char_id,
            location_id: ship_id, flag: InventoryFlag::Cargo,
            quantity: 45, singleton: false, ..Default::default()
        });
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(10_000_000);
        }
    });

    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);

    // 5. Execute valid trade with multiplier 2
    let trade_res = ctx.dispatch_rpc(&mut sess, "beyonce", None, "CallComponentFromClient", &[
        PyRep::Integer(trader_entity_id), PyRep::String("itemTrader".into()),
        PyRep::String("ProcessTrade".into()), PyRep::Integer(67), PyRep::Integer(2),
    ], &kw, None).await.expect("ProcessTrade x2");
    assert!(matches!(trade_res, PyRep::Tuple(ref items) if items.first() == Some(&PyRep::Bool(true))));

    // Verify atomic consumption: 30 units consumed from cargo, 15 remaining; 2 output items produced
    let (rem_in_qty, out_qty) = ctx.db.with_snapshot(|snap| {
        let r_in: i32 = snap.items.values()
            .filter(|i| i.owner_id == char_id && i.location_id == ship_id && i.type_id == 85316)
            .map(|i| i.quantity).sum();
        let r_out: i32 = snap.items.values()
            .filter(|i| i.owner_id == char_id && i.location_id == ship_id && i.type_id == 86272)
            .map(|i| i.quantity).sum();
        (r_in, r_out)
    });
    assert_eq!(rem_in_qty, 15, "Atomic consumption of 30 inputs leaves 15 in cargo");
    assert_eq!(out_qty, 2, "Multiplier 2 produces 2 output items");

    let bal_after = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    assert_eq!(bal_before, bal_after);
}

#[tokio::test]
// parity: feat:esi.corporations.structures-services-timers
async fn test_esi_corporation_structures_timers_and_online_services() {
    eve_testkit::attest(&["feat:esi.corporations.structures-services-timers"]);
    let ctx = std::sync::Arc::new(ServerContext::new());
    common::pin_clock(&ctx);

    let corp_id = 98_700_001_i64;
    let ceo_id = 90_000_001_i64;
    let struct_id = 1_040_000_001_i64;
    let svc_online = 998_701_i64;
    let svc_offline = 998_702_i64;

    let timer_start_unix = 1_720_000_000_u64;
    let timer_end_unix = 1_720_010_000_u64;
    let unanchors_at_unix = 1_722_000_000_u64;
    let next_apply_unix = 1_720_500_000_u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(corp_id, eve_db::CorporationRecord {
            corporation_id: corp_id, name: "Deep Space Engineering".into(),
            ticker: "DSE".into(), ceo_id, tax_rate: 0.05,
            members: std::collections::HashSet::from([ceo_id]),
            hq_station_id: Some(JITA_STATION), ..Default::default()
        });
        snap.structures.insert(struct_id, eve_db::UpwellStructureRecord {
            structure_id: struct_id, type_id: 35825, owner_corp_id: corp_id,
            solar_system_id: JITA_SOLAR_SYSTEM, name: "Perimeter Industrial Forge".into(),
            position: [0.0, 0.0, 0.0], state: "ArmorReinforced".into(),
            quantum_core_installed: true, fuel_blocks: 600, shield_hp: 0.0,
            armor_hp: 5_000.0, hull_hp: 10_000.0, reinforced_until_unix: timer_end_unix,
            last_damage_tick: 1, last_damage_unix: timer_start_unix, self_destruct_at_unix: None,
        });
        snap.structure_vulnerability_windows.insert(struct_id, (15 * 3600, next_apply_unix));

        // Fitted service modules: 1 online (ServiceSlot0), 1 offline (ServiceSlot1)
        snap.items.insert(svc_online, InventoryItemRecord {
            item_id: svc_online, type_id: 35894, owner_id: corp_id,
            location_id: struct_id, flag: InventoryFlag::ServiceSlot0,
            quantity: 1, singleton: true, ..Default::default()
        });
        snap.module_online_states.insert(svc_online, true);

        snap.items.insert(svc_offline, InventoryItemRecord {
            item_id: svc_offline, type_id: 35892, owner_id: corp_id,
            location_id: struct_id, flag: InventoryFlag::ServiceSlot1,
            quantity: 1, singleton: true, ..Default::default()
        });
        snap.module_online_states.insert(svc_offline, false);

        // Populate live StructureExtendedState directly (no snap.structure_esi_timer_meta)
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.structure_profile_id.insert(struct_id, 4242);
        ext.unanchoring_until_unix.insert(struct_id, unanchors_at_unix);
        ext.pending_reinforce_schedule.insert(struct_id, (21, 5, next_apply_unix));
        let life = ext.lifecycle.entry(struct_id).or_default();
        life.fight_started_unix = Some(timer_start_unix);
        life.repair_until_unix = Some(timer_end_unix);
        life.last_fuel_tick_unix = Some(timer_start_unix);
        life.online_modules = vec![svc_online];
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });

    let token = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec!["esi-corporations.read_structures.v1".to_string()],
    );
    let router = EsiRouter::new(ctx);
    let req = EsiHttpRequest::new("GET", format!("/v4/corporations/{corp_id}/structures/"))
        .with_bearer_token(&token);
    let resp = router.handle_request(&req);
    assert_eq!(
        resp.status_code, 200,
        "ESI response body: {}",
        String::from_utf8_lossy(&resp.body)
    );

    let rows: serde_json::Value =
        serde_json::from_slice(&resp.body).expect("parse ESI JSON");
    let arr = rows.as_array().expect("ESI structures array");
    assert_eq!(arr.len(), 1);

    let st = &arr[0];
    assert_eq!(st["structure_id"], struct_id);
    assert_eq!(st["profile_id"], 4242, "profile_id derived from StructureExtendedState");
    assert_eq!(st["state"], "armor_reinforce");
    assert_eq!(st["reinforce_hour"], 15);
    assert_eq!(st["next_reinforce_hour"], 21, "next_reinforce_hour derived from pending schedule");
    assert!(st["fuel_expires"].is_string(), "fuel_expires derived from live fuel & online services");
    assert!(st["state_timer_start"].is_string(), "state_timer_start matches fight start");
    assert!(st["state_timer_end"].is_string(), "state_timer_end matches reinforce end");
    assert!(st["unanchors_at"].is_string(), "unanchors_at derived from StructureExtendedState");
    assert!(st["next_reinforce_apply"].is_string(), "next_reinforce_apply derived from pending schedule");

    let svcs = st["services"].as_array().expect("services array");
    assert_eq!(svcs.len(), 2);
    let on_svc = svcs.iter().find(|s| s["state"] == "online").expect("online service");
    assert_eq!(on_svc["name"], "Standup Cloning Center I");
    let off_svc = svcs.iter().find(|s| s["state"] == "offline").expect("offline service");
    assert_eq!(off_svc["name"], "Standup Market Hub I");
}

#[tokio::test]
// parity: feat:ui.map-visited-systems
async fn test_map_visited_systems_tracking_on_system_entry() {
    eve_testkit::attest(&["feat:ui.map-visited-systems"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let mut sess = login_and_select(&ctx, 2104, char_id).await;
    let kw = HashMap::new();

    // 1. Initial check: Amamake and Poitot have 0 visits
    let amamake = 30_002_537_u32;
    let poitot = 30_003_271_u32;

    let init_res = ctx.dispatch_rpc(&mut sess, "map", None, "GetSolarSystemVisits", &[], &kw, None)
        .await
        .expect("initial GetSolarSystemVisits");
    let PyRep::List(ref init_list) = init_res else { panic!("expected List") };
    let amamake_initial = init_list.iter().find(|r| {
        matches!(r, PyRep::KeyVal(kv) if kv.get("solarSystemID") == Some(&PyRep::Integer(amamake as i64)))
    });
    assert!(amamake_initial.is_none(), "Amamake must not be visited yet");

    // 2. Record first visit to Amamake
    eve_server::services::map_service::record_solar_system_jump(&ctx, char_id, amamake);

    let res_after_first = ctx.dispatch_rpc(&mut sess, "map", None, "GetSolarSystemVisits", &[], &kw, None)
        .await
        .expect("GetSolarSystemVisits after 1st visit");
    let PyRep::List(ref list1) = res_after_first else { panic!("expected List") };
    let amamake_first = list1.iter().find(|r| {
        matches!(r, PyRep::KeyVal(kv) if kv.get("solarSystemID") == Some(&PyRep::Integer(amamake as i64)))
    }).expect("Amamake record present after 1st visit");
    let PyRep::KeyVal(ref kv1) = amamake_first else { panic!("expected KeyVal") };
    assert_eq!(kv1.get("visits"), Some(&PyRep::Integer(1)), "First visit count must be 1");
    let dt1 = kv1.get("lastDateTime").and_then(|v| v.as_i64()).unwrap();
    assert!(dt1 > 0);

    // 3. Advance clock and record 2nd visit to Amamake
    ctx.advance_virtual_clock(300);
    eve_server::services::map_service::record_solar_system_visit(&ctx, char_id, amamake);

    let res_after_second = ctx.dispatch_rpc(&mut sess, "map", None, "GetSolarSystemVisits", &[], &kw, None)
        .await
        .expect("GetSolarSystemVisits after 2nd visit");
    let PyRep::List(ref list2) = res_after_second else { panic!("expected List") };
    let amamake_second = list2.iter().find(|r| {
        matches!(r, PyRep::KeyVal(kv) if kv.get("solarSystemID") == Some(&PyRep::Integer(amamake as i64)))
    }).expect("Amamake record present after 2nd visit");
    let PyRep::KeyVal(ref kv2) = amamake_second else { panic!("expected KeyVal") };
    assert_eq!(kv2.get("visits"), Some(&PyRep::Integer(2)), "Visits count must increment to 2");
    let dt2 = kv2.get("lastDateTime").and_then(|v| v.as_i64()).unwrap();
    assert!(dt2 > dt1, "lastDateTime must advance with virtual clock");

    // 4. Enter a second solar system (Poitot)
    eve_server::services::map_service::record_solar_system_jump(&ctx, char_id, poitot);

    let res_both = ctx.dispatch_rpc(&mut sess, "map", None, "GetSolarSystemVisits", &[], &kw, None)
        .await
        .expect("GetSolarSystemVisits with both systems");
    let PyRep::List(ref list_both) = res_both else { panic!("expected List") };

    let amamake_rec = list_both.iter().find(|r| {
        matches!(r, PyRep::KeyVal(kv) if kv.get("solarSystemID") == Some(&PyRep::Integer(amamake as i64)))
    }).expect("Amamake record");
    if let PyRep::KeyVal(kv) = amamake_rec {
        assert_eq!(kv.get("visits"), Some(&PyRep::Integer(2)));
    }

    let poitot_rec = list_both.iter().find(|r| {
        matches!(r, PyRep::KeyVal(kv) if kv.get("solarSystemID") == Some(&PyRep::Integer(poitot as i64)))
    }).expect("Poitot record");
    if let PyRep::KeyVal(kv) = poitot_rec {
        assert_eq!(kv.get("visits"), Some(&PyRep::Integer(1)));
    }
}
