//! Regression tests for Round-7 Track A P0 session sync (#416) and object binding (#417).

use super::ServerContext;
use eve_db::{CharacterRecord, UpwellStructureRecord, WorldRepository};
use eve_machonet::service_roles::ROLE_PLAYER;
use eve_machonet::SessionState;
use std::sync::Arc;

fn test_ctx() -> ServerContext {
    ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()))
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn make_char(char_id: i64, corp_id: i64, alliance_id: Option<i64>, sys_id: u32) -> CharacterRecord {
    CharacterRecord {
        activity_status: None,
        character_id: char_id,
        user_id: 1,
        name: format!("Pilot {char_id}"),
        corporation_id: corp_id,
        alliance_id,
        war_faction_id: None,
        station_id: Some(60_003_760),
        structure_id: None,
        solar_system_id: sys_id,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        active_ship_id: 100_000_000 + char_id,
        balance: eve_db::IskCents(1_000_000),
        plex_balance: 0,
        loyalty_points: std::collections::HashMap::new(),
        standings: std::collections::HashMap::new(),
        security_status: 0.0,
        clone_grade: 2,
        home_station_id: 60_003_760,
        race_id: 1,
        bloodline_id: 1,
        ancestry_id: 1,
        school_id: 11,
        neural: eve_db::NeuralAttributes::default(),
        skill_points: 500_000.0,
        free_skill_points: 0.0,
        last_clone_jump_unix: 0,
        jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0,
        jump_fatigue_until_unix: 0,
        description: String::new(),
        title: String::new(),
        gender: 1,
        create_date_unix: 0,
        fleet_id: None,
        autopilot_waypoints: Vec::new(),
        version: 1,
        system_epoch: 1,
    }
}

#[test]
fn test_issue_416_notify_drain_syncs_session_after_capturing_prev_sys() {
    let ctx = test_ctx();
    let char_id = 95_000_416_i64;
    let jita = 30_000_142_u32;
    let perimeter = 30_000_144_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .insert(char_id, make_char(char_id, 1_000_035, None, jita));
    });

    let mut session = SessionState {
        session_id: 416,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(jita),
        role: ROLE_PLAYER,
        ..Default::default()
    };
    let mut subscribed_sys_id = session.solar_system_id;

    // Move character in DB to Perimeter (30000144) before notification drain runs
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = perimeter;
        }
    });

    // Capturing prev_sys BEFORE sync_session detects the system transition immediately
    let prev_sys = session.solar_system_id;
    ctx.sync_session(&mut session);
    assert_eq!(prev_sys, Some(jita));
    assert_eq!(session.solar_system_id, Some(perimeter));
    assert!(session.solar_system_id != prev_sys || session.solar_system_id != subscribed_sys_id);

    subscribed_sys_id = session.solar_system_id;
    assert_eq!(subscribed_sys_id, Some(perimeter));
}

#[test]
fn test_issue_417_bind_object_enforces_strict_context_authorization() {
    let ctx = test_ctx();
    let char1 = 95_000_001_i64;
    let jita = 30_000_142_u32;
    let amarr = 30_002_187_u32;
    let corp1 = 98_000_101_i64;
    let corp2 = 98_000_202_i64;
    let alliance1 = 99_000_101_i64;
    let alliance2 = 99_000_202_i64;
    let fleet1 = 77_000_101_i64;
    let fleet2 = 77_000_202_i64;
    let struct1 = 1_000_101_001_i64;
    let struct2 = 1_000_202_002_i64;
    let jita_planet = 40_009_082_i64;
    let amarr_planet = 40_138_719_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .insert(char1, make_char(char1, corp1, Some(alliance1), jita));
        snap.structures.insert(
            struct1,
            UpwellStructureRecord {
                structure_id: struct1,
                type_id: 35832,
                name: "Friendly Astrahus".into(),
                solar_system_id: jita,
                owner_corp_id: corp1,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct2,
            UpwellStructureRecord {
                structure_id: struct2,
                type_id: 35832,
                name: "Hostile Astrahus".into(),
                solar_system_id: amarr,
                owner_corp_id: corp2,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState {
        session_id: 417,
        user_id: Some(1),
        character_id: Some(char1),
        corporation_id: Some(corp1),
        alliance_id: Some(alliance1),
        fleet_id: Some(fleet1),
        solar_system_id: Some(jita),
        station_id: Some(60_003_760),
        ship_id: Some(100_000_000 + char1),
        role: ROLE_PLAYER,
        ..Default::default()
    };

    // Authorized own bindings succeed
    let corp_bid = ctx.bind_object(&sess, "corpRegistry", corp1).unwrap();
    assert!(ctx.bind_object(&sess, "corpmgr", corp1).is_ok());
    assert!(ctx.bind_object(&sess, "allianceRegistry", alliance1).is_ok());
    assert!(ctx.bind_object(&sess, "fleetObjectHandler", fleet1).is_ok());
    assert!(ctx.bind_object(&sess, "planetMgr", jita_planet).is_ok());
    assert!(ctx.bind_object(&sess, "structure", struct1).is_ok());
    assert!(ctx.bind_object(&sess, "posMgr", struct1).is_ok());
    assert!(ctx.bind_object(&sess, "trademgr", 10_000_002).is_ok());
    assert!(ctx.bind_object(&sess, "trademgr", 60_003_760).is_ok());

    // Cross-corp, cross-alliance, cross-fleet, cross-system, and arbitrary object bindings fail
    for (svc, forbidden_id) in [
        ("corpRegistry", corp2),
        ("corpmgr", corp2),
        ("allianceRegistry", alliance2),
        ("fleetObjectHandler", fleet2),
        ("planetMgr", amarr_planet),
        ("structure", struct2),
        ("posMgr", struct2),
        ("posMgr", corp2),
        ("trademgr", 60_008_494),
        ("trademgr", struct2),
        ("trademgr", 999_999_999),
    ] {
        let err = ctx.bind_object(&sess, svc, forbidden_id).unwrap_err();
        assert_eq!(
            err.code,
            "BindAuthorizationDenied", // rpc-lint: allow error code assertion
            "Expected BindAuthorizationDenied for {svc}({forbidden_id})"
        );
    }

    // Leaving the corporation immediately invalidates existing bound corp moniker verification
    sess.corporation_id = Some(corp2);
    assert!(ctx.verify_bound_object(&sess, corp_bid).is_err());
}

#[tokio::test]
async fn test_issue_418_verify_and_invalidate_bound_objects_on_session_transitions() {
    let char1: i64 = 90_000_201;
    let corp1: i64 = 98_000_201;
    let corp2: i64 = 98_000_202;
    let fleet1: i64 = 77_000_201;
    let fleet2: i64 = 77_000_202;
    let ship1: i64 = 100_000_201;
    let ship2: i64 = 100_000_202;
    let jita: u32 = 30_000_142;
    let perim: u32 = 30_000_144;
    let struct1: i64 = 1_000_000_201_001;

    let ctx = test_ctx();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = make_char(char1, corp1, None, jita);
        ch.active_ship_id = ship1;
        ch.fleet_id = Some(fleet1);
        snap.characters.insert(char1, ch);
        snap.structures.insert(
            struct1,
            UpwellStructureRecord {
                structure_id: struct1,
                type_id: 35832,
                name: "Citadel".into(),
                solar_system_id: jita,
                owner_corp_id: corp1,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState {
        session_id: 418,
        user_id: Some(1),
        character_id: Some(char1),
        corporation_id: Some(corp1),
        fleet_id: Some(fleet1),
        solar_system_id: Some(jita),
        ship_id: Some(ship1),
        role: ROLE_PLAYER,
        ..Default::default()
    };
    ctx.db
        .acquire_online_lock(1, 418, Some(char1), true, ctx.now_unix())
        .unwrap();

    let bid_ship = ctx.bind_object(&sess, "ship", ship1).unwrap();
    let bid_dogma = ctx.bind_object(&sess, "dogmaIM", ship1).unwrap();
    let bid_corp = ctx.bind_object(&sess, "corpRegistry", corp1).unwrap();
    let bid_fleet = ctx.bind_object(&sess, "fleetObjectHandler", fleet1).unwrap();
    let bid_struct = ctx.bind_object(&sess, "structure", struct1).unwrap();

    // Ship board_or_eject invalidates old ship & dogmaIM and emits OnMachoObjectDisconnect
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char1) {
            c.active_ship_id = ship2;
        }
    });
    sess.ship_id = Some(ship2);
    assert!(ctx.verify_bound_object(&sess, bid_ship).is_err());
    assert!(ctx.verify_bound_object(&sess, bid_dogma).is_err());
    ctx.pending_notifications.write().unwrap().clear();
    ctx.invalidate_bound_objects_for_session_transition(char1, jita, None, "board_or_eject");
    let disconnects = ctx
        .pending_notifications
        .read()
        .unwrap()
        .get(&char1)
        .map_or(0, |v| {
            v.iter()
                .filter(|n| n.event == "OnMachoObjectDisconnect")
                .count()
        });
    assert!(disconnects >= 2);
    assert!(ctx.verify_bound_object(&sess, bid_corp).is_ok());

    // Fleet transition invalidates old fleetObjectHandler
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char1) {
            c.fleet_id = Some(fleet2);
        }
    });
    sess.fleet_id = Some(fleet2);
    assert!(ctx.verify_bound_object(&sess, bid_fleet).is_err());
    ctx.invalidate_bound_objects_for_session_transition(char1, jita, None, "fleet_change");

    // Corp change + jump away from jita invalidates corpRegistry & structure
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char1) {
            c.corporation_id = corp2;
            c.solar_system_id = perim;
        }
    });
    sess.corporation_id = Some(corp2);
    sess.solar_system_id = Some(perim);
    assert!(ctx.verify_bound_object(&sess, bid_struct).is_err());
    ctx.invalidate_bound_objects_for_session_transition(char1, perim, None, "stargate_jump");
}

#[tokio::test]
async fn test_issue_425_abyssal_dynamic_attributes_stay_online_on_rebuild() {
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use std::collections::HashMap;

    let char_id: i64 = 90_000_301;
    let ship_id: i64 = 100_000_301;
    let mod_id: i64 = 100_000_302;
    let hull_tid: u32 = 587;
    let ab_tid: u32 = 439;

    let mut ctx = test_ctx();
    let sde = Arc::make_mut(&mut ctx.sde);
    if let Some(hull) = sde.types.get_mut(&hull_tid) {
        hull.default_attributes.insert(eve_sde::attr_ids::CPU_OUTPUT, 50.0);
        hull.default_attributes.insert(eve_sde::attr_ids::POWER_OUTPUT, 50.0);
    }
    if let Some(m) = sde.types.get_mut(&ab_tid) {
        m.effect_ids = vec![13, 16]; // medPower + online
        for req_attr in [182_u16, 183, 184, 1285, 1289, 1290] {
            m.default_attributes.remove(&req_attr);
        }
        m.default_attributes.insert(eve_sde::attr_ids::CPU, 80.0);
        m.default_attributes.insert(eve_sde::attr_ids::POWER, 30.0);
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .insert(char_id, make_char(char_id, 1_000_009, None, 30_000_142));
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: hull_tid,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: ab_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(eve_sde::attr_ids::CPU, 35.0)]),
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_id, true);
    });

    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let fitted = fit.modules.get(&mod_id).unwrap();
    assert_eq!(fitted.state, eve_dogma::ModuleState::Online);
    assert_eq!(fitted.dynamic_attribute_overrides.get(&eve_sde::attr_ids::CPU), Some(&35.0));
    assert_eq!(ctx.db.with_snapshot(|s| s.module_online_states.get(&mod_id).copied()), Some(true));
}
