//! Integration tests for LP Store mechanics, NPC-NPC Standings dispatch, and Shipcaster Travel mechanics.
//!
//! Verifies:
//! - `TakeOfferForCharacter` and `TakeOfferForCorporation` LP/ISK deduction, item consumption, and delivery.
//! - `standingMgr.GetNPCNPCStandings` Rowset shape and standing values dispatch.
//! - `starterShipcasterTravelMgr.CanCharacterJump` weapons timer and capital ship restrictions.
//! - `CmdJumpThroughStarterShipcaster` 5km proximity boundary checks (4.9km success, 5.1km rejection), session timer, and ball transfer.
//! - `CmdJumpThroughShipcaster` ball transfer and destination placement.
//!
//! DATA GAPs (server-only data not present in V24 client FSD):
//! - LP store corporation offer catalog is server-only.
//! - Static NPC-NPC cross-standing matrix is server-internal.
//! - Starter and interstellar shipcaster placements and linkages are server-side structures.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::lp_store::{
    load_corp_lp_balances, save_corp_lp_balances, save_lp_store_offers, LPOffer,
};
use glam::DVec3;
use std::collections::HashMap;

async fn rpc(
    ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn k_i64(r: &PyRep, k: &str) -> Option<i64> { r.dict_get(k).and_then(|v| v.as_i64()) }
fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    match rep { PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_i64()).unwrap_or(0), _ => 0 }
}
fn kv_bool(rep: &PyRep, key: &str) -> bool {
    match rep { PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_bool()).unwrap_or(false), _ => false }
}

async fn spawn_test_ship(
    ctx: &ServerContext,
    handle: &eve_server::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
    pos: DVec3,
) {
    let _ = handle.spawn_ball_with_combat(
        Ball {
            id: ship_id, type_id, position: pos, velocity: DVec3::ZERO,
            max_velocity: 1000.0, inertia: 1.0, mass: 100_000.0, radius: 50.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        ctx.build_ship_combat_state(char_id, ship_id, type_id),
    ).await;
}

#[tokio::test]
async fn test_lp_store_mgr_gameplay_closure() {
    // DATA GAP: LP store corporation offer catalog is server-only and not present in V24 client FSD.
    // Tests authentic LP/ISK deduction, required item trade-in consumption, and hangar delivery mechanics.
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let station_id = 60_003_760;
    let ship_id = 100_000_001;

    let mut sess = SessionState::new(4321);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    // Seed LP store test offers for corp 1_000_035
    let test_offers = vec![
        LPOffer {
            offer_id: 1001, corp_id: 1_000_035, type_id: 15810, quantity: 1,
            lp_cost: 2_400, isk_cost: IskCents::from_isk(2_400_000),
            req_items: vec![], ak_cost: 0, min_standing: None,
        },
        LPOffer {
            offer_id: 1004, corp_id: 1_000_035, type_id: 621, quantity: 1,
            lp_cost: 5_000, isk_cost: IskCents::from_isk(5_000_000),
            req_items: vec![(34, 100)], ak_cost: 0, min_standing: None,
        },
    ];
    ctx.db.with_mut_snapshot(|s| {
        save_lp_store_offers(s, &test_offers);
    });

    // 1. GetAvailableOffersFromCorp returns seeded offers for corporation
    let rep = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(1_000_035)],
    )
    .await
    .expect("GetAvailableOffersFromCorp");
    let PyRep::List(ref offers) = rep else {
        panic!("Expected List for 1_000_035");
    };
    assert_eq!(offers.len(), 2, "Seeded corp must return 2 offers");
    for o in offers {
        assert_eq!(k_i64(o, "corpID"), Some(1_000_035));
        assert!(k_i64(o, "offerID").unwrap_or(0) > 0);
        assert!(k_i64(o, "typeID").unwrap_or(0) > 0);
        assert!(k_i64(o, "quantity").unwrap_or(0) > 0);
        assert!(k_i64(o, "lpCost").unwrap_or(0) >= 0);
        assert!(
            o.dict_get("iskCost")
                .and_then(|v| v.as_f64())
                .unwrap_or(-1.0)
                >= 0.0
        );
    }

    // Unknown or unseeded corp returns empty list
    let empty_rep = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(999_888_777)],
    )
    .await
    .expect("GetAvailableOffersFromCorp unknown");
    assert_eq!(empty_rep, PyRep::List(vec![]));

    // 2. TakeOfferForCharacter with seeded offer (offer 1001: Ballistic Control System, 2400 LP, 2.4M ISK)
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.corporation_id = corp_id;
        ch.loyalty_points.insert(1_000_035, 50_000);
        ch.balance = IskCents::from_isk(100_000_000);
        ch.station_id = Some(station_id);

        let mut corp = eve_db::types::CorporationRecord {
            corporation_id: corp_id,
            name: "LP Test Corp".into(),
            ticker: "LPTC".into(),
            ceo_id: char_id + 999,
            master_wallet: IskCents::from_isk(100_000_000),
            members: [char_id].into_iter().collect(),
            hq_station_id: Some(station_id),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        corp.wallet_divisions[0].balance = IskCents::from_isk(100_000_000);
        s.corporations.insert(corp_id, corp);
    });
    sess.corporation_id = Some(corp_id);

    let res_char = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(1_000_035), PyRep::Integer(1001), PyRep::Integer(2)],
    )
    .await
    .expect("TakeOfferForCharacter 1001 x2");
    assert_eq!(kv_i64(&res_char, "offerID"), 1001);
    assert_eq!(kv_i64(&res_char, "corpID"), 1_000_035);
    assert_eq!(kv_i64(&res_char, "quantity"), 2);
    assert_eq!(kv_i64(&res_char, "remainingLP"), 50_000 - 4_800);
    assert!(kv_i64(&res_char, "deliveredItemID") > 0);

    ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&char_id).unwrap();
        assert_eq!(
            ch.loyalty_points.get(&1_000_035).copied(),
            Some(50_000 - 4_800)
        );
        assert_eq!(ch.balance, IskCents::from_isk(100_000_000 - 4_800_000));
        let delivered = s.items.values().find(|it| {
            it.owner_id == char_id
                && it.location_id == station_id
                && it.type_id == 15810
                && it.flag == InventoryFlag::Hangar
        });
        assert!(
            delivered.is_some(),
            "Delivered item must be in character hangar"
        );
    });

    // Take offer 1004 which requires trade-in items (100x Tritanium type 34)
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            77_000_101,
            InventoryItemRecord {
                item_id: 77_000_101,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 200,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let trit_qty_before: i64 = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|it| {
                it.owner_id == char_id
                    && it.type_id == 34
                    && it.location_id == station_id
            })
            .map(|it| it.quantity as i64)
            .sum()
    });

    let res_char_1004 = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(1_000_035), PyRep::Integer(1004), PyRep::Integer(2)],
    )
    .await
    .expect("TakeOfferForCharacter 1004 x2 with trade-in");
    assert_eq!(kv_i64(&res_char_1004, "offerID"), 1004);
    assert_eq!(kv_i64(&res_char_1004, "corpID"), 1_000_035);
    assert_eq!(kv_i64(&res_char_1004, "quantity"), 2);
    assert!(kv_i64(&res_char_1004, "deliveredItemID") > 0);

    // Verify trade-in items consumed (2x 100 = 200 consumed)
    ctx.db.with_snapshot(|s| {
        let trit_qty_after: i64 = s
            .items
            .values()
            .filter(|it| {
                it.owner_id == char_id
                    && it.type_id == 34
                    && it.location_id == station_id
            })
            .map(|it| it.quantity as i64)
            .sum();
        assert_eq!(
            trit_qty_before - trit_qty_after,
            200,
            "Trade-in Tritanium must be consumed"
        );
    });

    // 3. TakeOfferForCorporation
    let mut corp_bals = HashMap::new();
    corp_bals.insert(1_000_035, 100_000);
    ctx.db.with_mut_snapshot(|s| {
        save_corp_lp_balances(s, corp_id, &corp_bals);
        if let Some(c) = s.corporations.get_mut(&corp_id) {
            c.ensure_wallet_divisions();
            c.wallet_divisions[0].balance = IskCents::from_isk(50_000_000);
        }
    });

    let res_corp = rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCorporation",
        &[PyRep::Integer(1_000_035), PyRep::Integer(1001), PyRep::Integer(1)],
    )
    .await
    .expect("TakeOfferForCorporation 1001");
    assert_eq!(kv_i64(&res_corp, "offerID"), 1001);
    assert_eq!(kv_i64(&res_corp, "corpID"), 1_000_035);
    assert_eq!(kv_i64(&res_corp, "quantity"), 1);
    assert!(kv_i64(&res_corp, "deliveredItemID") > 0);

    ctx.db.with_snapshot(|s| {
        let b = load_corp_lp_balances(s, corp_id);
        assert_eq!(b.get(&1_000_035).copied(), Some(100_000 - 2_400));
        let c = s.corporations.get(&corp_id).unwrap();
        assert_eq!(
            c.wallet_divisions[0].balance,
            IskCents::from_isk(50_000_000 - 2_400_000)
        );
        let corp_item = s.items.values().find(|it| {
            it.owner_id == corp_id
                && it.location_id == station_id
                && it.type_id == 15810
        });
        assert!(corp_item.is_some(), "Delivered item must be owned by corp");
    });
}

#[tokio::test]
async fn test_standing_mgr_npc_npc_standings_closure() {
    // DATA GAP: V24 client SDE does not ship the full server-side inter-NPC standings matrix;
    // stands open until static data provenance is available, while GetNPCNPCStandings returns derived faction pairs.
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(4322);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    let rep = rpc(&ctx, &mut sess, "standingMgr", "GetNPCNPCStandings", &[])
        .await
        .expect("GetNPCNPCStandings");

    let PyRep::Rowset { columns, rows, .. } = rep else {
        panic!("GetNPCNPCStandings must return a Rowset");
    };

    assert_eq!(columns, vec!["fromID", "toID", "standing"]);
    assert!(!rows.is_empty(), "GetNPCNPCStandings must return derived inter-faction pairs");

    let mut rel_map = HashMap::new();
    for row in &rows {
        assert_eq!(row.len(), 3);
        let from_id = row[0].as_i64().expect("fromID");
        let to_id = row[1].as_i64().expect("toID");
        let standing = match &row[2] {
            PyRep::Real(v) => *v,
            PyRep::Integer(v) => *v as f64,
            other => panic!("expected real standing, got {other:?}"),
        };
        assert!((-10.0..=10.0).contains(&standing), "Standing must be in [-10, 10]");
        rel_map.insert((from_id, to_id), standing);
    }
    for &(f, t, exp) in &[
        (500_003, 500_002, -5.0),
        (500_015, 500_002, 3.0),
        (500_026, 500_027, -10.0),
        (500_027, 500_026, -10.0),
    ] {
        assert_eq!(rel_map.get(&(f, t)).copied(), Some(exp), "mismatch for ({f}, {t})");
    }
}

#[tokio::test]
async fn test_shipcaster_and_starter_travel_closure() {
    // DATA GAP: Starter and interstellar shipcaster structures and linkages are server-side.
    // Tests CanCharacterJump weapons timer and capital ship restrictions, proximity check, and ball transfer.
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess = SessionState::new(4323);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    let ship_id = 100_000_001;
    let starter_sys: u32 = 30002800; // Haatomo starter staging linkage
    let staging_sys: u32 = 30002813; // Tama staging system
    let starter_launcher_id: i64 = 105_000_101;
    let dest_pad_id: i64 = 104_000_001;
    let warzone_sys: u32 = 30002811;

    // 1. GetAllLandingPads
    let all_pads_rep = rpc(
        &ctx,
        &mut sess,
        "shipCasterLandingPadMgr",
        "GetAllLandingPads",
        &[],
    )
    .await
    .expect("GetAllLandingPads");
    let PyRep::List(all_pads) = all_pads_rep else {
        panic!("Expected List");
    };
    assert_eq!(all_pads.len(), 4);
    for pad in &all_pads {
        assert!(kv_i64(pad, "itemID") > 0);
        assert!(kv_i64(pad, "solarSystemID") > 0);
        assert!(matches!(kv_i64(pad, "factionID"), 500001..=500004));
        assert!(
            kv_bool(pad, "isBuilt")
                && kv_bool(pad, "isLinked")
                && kv_bool(pad, "canBeLinked")
        );
    }

    // 2. GetFactionLandingPads
    for fid in &[500001, 500002, 500003, 500004] {
        let fpad_rep = rpc(
            &ctx,
            &mut sess,
            "shipCasterLandingPadMgr",
            "GetFactionLandingPads",
            &[PyRep::Integer(*fid)],
        )
        .await
        .expect("GetFactionLandingPads");
        let PyRep::List(fpads) = fpad_rep else {
            panic!("Expected List");
        };
        assert_eq!(fpads.len(), 1);
        assert_eq!(kv_i64(&fpads[0], "factionID"), *fid);
    }
    let empty_pads = rpc(
        &ctx,
        &mut sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(999_999)],
    )
    .await
    .expect("GetFactionLandingPads unknown");
    assert_eq!(empty_pads, PyRep::List(vec![]));

    // 3. GetFactionsWithShipcaster
    let fws_rep = rpc(
        &ctx,
        &mut sess,
        "shipCasterLauncherMgr",
        "GetFactionsWithShipcaster",
        &[],
    )
    .await
    .expect("GetFactionsWithShipcaster");
    let PyRep::List(fws) = fws_rep else {
        panic!("Expected List");
    };
    assert_eq!(
        fws,
        vec![
            PyRep::Integer(500001),
            PyRep::Integer(500002),
            PyRep::Integer(500003),
            PyRep::Integer(500004)
        ]
    );

    // 4. CanCharacterJump
    // Undocked in frigate: true
    sess.station_id = None;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(starter_sys);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.solar_system_id = starter_sys;
            ch.station_id = None;
        }
        if let Some(ship) = s.items.get_mut(&ship_id) {
            ship.type_id = 601; // Ibis frigate
            ship.location_id = starter_sys as i64;
        }
    });

    let can_jump = rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CanCharacterJump",
        &[],
    )
    .await
    .expect("CanCharacterJump");
    assert_eq!(can_jump, PyRep::Bool(true));

    // Weapons timer active -> false
    let now = ctx.now_unix();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char_id).weapon_timer_until_unix = now + 60;
    }
    let timer_rep = rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CanCharacterJump",
        &[],
    )
    .await
    .expect("CanCharacterJump with timer");
    assert_eq!(timer_rep, PyRep::Bool(false));
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char_id).weapon_timer_until_unix = 0;
    }

    // Capital ship -> false
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ship) = s.items.get_mut(&ship_id) {
            ship.type_id = 19720; // Revelation (Dreadnought, group 485)
        }
    });
    let cap_rep = rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CanCharacterJump",
        &[],
    )
    .await
    .expect("CanCharacterJump with dreadnought");
    assert_eq!(cap_rep, PyRep::Bool(false));
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ship) = s.items.get_mut(&ship_id) {
            ship.type_id = 601; // Reset to frigate
        }
    });

    // 5. CmdJumpThroughStarterShipcaster
    // Negative test (a): Out of range boundary check (5.1km > SHIPCASTER_MAX_RANGE_M 5.0km from launcher at 300km)
    let handle_starter = ctx.systems.get_or_spawn(starter_sys);
    spawn_test_ship(
        &ctx,
        &handle_starter,
        char_id,
        ship_id,
        601,
        DVec3::new(300_000.0, 5_100.0, 0.0),
    )
    .await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);
    let oor_err = rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(starter_launcher_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(oor_err.code, "OutsideOperationalRange");

    // Negative test (b): Session-change timer active at valid in-range boundary (4.9km <= 5.0km)
    assert!(
        handle_starter
            .set_ball_position(ship_id, DVec3::new(300_000.0, 4_900.0, 0.0))
            .await
    );
    let now_unix = ctx.now_unix();
    eve_server::services::space_destiny::record_session_change_timer(
        &ctx,
        char_id,
        now_unix,
        starter_sys,
        0,
        "test_session_lock",
    );
    let timer_err = rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(starter_launcher_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(timer_err.code, "SessionChangeInProgress");

    // Success path: clear session timer and perform jump at 4.9km boundary
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);
    let jump_starter = rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(starter_launcher_id)],
    )
    .await
    .expect("CmdJumpThroughStarterShipcaster");
    assert_eq!(jump_starter, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(staging_sys));

    ctx.db.with_snapshot(|s| {
        assert_eq!(
            s.characters.get(&char_id).unwrap().solar_system_id,
            staging_sys
        );
        assert_eq!(
            s.items.get(&ship_id).unwrap().location_id,
            staging_sys as i64
        );
    });

    // 6. CmdJumpThroughShipcaster (Interstellar FW shipcaster)
    let handle_staging = ctx.systems.get_or_spawn(staging_sys);
    spawn_test_ship(
        &ctx,
        &handle_staging,
        char_id,
        ship_id,
        601,
        DVec3::new(198_000.0, 0.0, 0.0),
    )
    .await;

    // Enroll in Caldari militia
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500001);
        }
    });
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);

    let origin_launcher_id = 105_000_001;
    let jump_fw = rpc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[
            PyRep::Integer(dest_pad_id),
            PyRep::Integer(dest_pad_id),
            PyRep::Integer(origin_launcher_id),
        ],
    )
    .await
    .expect("CmdJumpThroughShipcaster");
    assert_eq!(jump_fw, PyRep::None);
    assert_eq!(sess.solar_system_id, Some(warzone_sys));

    ctx.db.with_snapshot(|s| {
        assert_eq!(
            s.characters.get(&char_id).unwrap().solar_system_id,
            warzone_sys
        );
        assert_eq!(
            s.items.get(&ship_id).unwrap().location_id,
            warzone_sys as i64
        );
    });
}
