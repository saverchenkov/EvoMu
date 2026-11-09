//! Unit tests for `station_structure` (station queries, guests, service standing checks, undock/dock, mobile deployable anchoring).

#![forbid(unsafe_code)]

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_db::{CorporationRecord, InventoryItemRecord, SystemDynamicState, UpwellStructureRecord};

#[tokio::test]
async fn station_queries_guests_standing_check_and_undock_dock_cycle() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let jita_station = 60_003_760_i64;
    let jita_sys = 30_000_142_u32;
    let char_id = 910_601_i64;
    let corp_id = 98_600_601_i64;
    let ally_id = 99_600_601_i64;
    let ship_id = 910_602_i64;
    let st_id = 1_000_000_960_001_i64;

    assert!(npc_station_offers_service(&ctx.sde, jita_station, 5));
    assert!(npc_station_offers_service(&ctx.sde, 60_003_466, 13));
    assert!(!npc_station_offers_service(&ctx.sde, jita_station, -1));
    assert!(!npc_station_offers_service(&ctx.sde, 999_999_999, 5));
    assert!(!npc_station_offers_service(&ctx.sde, 999_999_999, 13));
    if let Some(lowsec_no_repair) = ctx.sde.stations.values().find(|st| {
        ctx.sde.solar_systems.get(&st.solar_system_id).is_some_and(|sys| (sys.security_status * 10.0).round() / 10.0 < 0.5)
            && ctx.sde.station_operations.get(&st.operation_id).is_some_and(|op| !op.services.contains(&13))
    }) {
        assert!(!npc_station_offers_service(&ctx.sde, lowsec_no_repair.station_id, 13));
    }

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, char_id, "Station Pilot", corp_id, jita_sys, ship_id);
        ch.alliance_id = Some(ally_id);
        ch.station_id = Some(jita_station);
        snap.characters.insert(char_id, ch);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                alliance_id: Some(ally_id),
                ..Default::default()
            },
        );
        snap.system_states.insert(
            30_000_144,
            SystemDynamicState {
                solar_system_id: 30_000_144,
                sov_alliance_id: Some(ally_id),
                ..Default::default()
            },
        );
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: jita_sys,
                name: "Corp Citadel".into(),
                position: [50_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let mut session = SessionState::new(1);
    session.user_id = Some(char_id as u64);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.alliance_id = Some(ally_id);
    session.solar_system_id = Some(jita_sys);
    session.station_id = Some(jita_station);
    session.ship_id = Some(ship_id);
    let kw = HashMap::new();

    // GetAllianceSystems returns sov system + member corp structure system
    let PyRep::List(ally_sys) = handle(&ctx, &mut session, "stationSvc", "GetAllianceSystems", &[PyRep::Integer(ally_id)], &kw)
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected list");
    };
    assert!(ally_sys.contains(&PyRep::Integer(30_000_144)));
    assert!(ally_sys.contains(&PyRep::Integer(jita_sys as i64)));

    // GetStationsForOwner returns corp structure
    let PyRep::List(owner_stations) = handle(&ctx, &mut session, "stationSvc", "GetStationsForOwner", &[PyRep::Integer(corp_id)], &kw)
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected list");
    };
    assert_eq!(owner_stations.len(), 1);

    // GetSolarSystem for station ID vs solarSystemID vs missing
    assert_eq!(
        handle(&ctx, &mut session, "stationSvc", "GetSolarSystem", &[PyRep::Integer(jita_station)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(jita_sys as i64)
    );
    let PyRep::KeyVal(sys_kv) = handle(&ctx, &mut session, "stationSvc", "GetSolarSystem", &[PyRep::Integer(jita_sys as i64)], &kw)
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected keyval");
    };
    assert_eq!(sys_kv.get("solarSystemID").and_then(|v| v.as_i64()), Some(jita_sys as i64));
    assert_eq!(
        handle(&ctx, &mut session, "stationSvc", "GetSolarSystem", &[PyRep::Integer(69_999_999)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StationNotFound"
    );

    // GetStation and GetStationItemBits
    let PyRep::KeyVal(st_kv) = handle(&ctx, &mut session, "stationSvc", "GetStation", &[PyRep::Integer(jita_station)], &kw)
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected keyval");
    };
    assert_eq!(st_kv.get("stationID").and_then(|v| v.as_i64()), Some(jita_station));
    let PyRep::Tuple(bits) = handle(&ctx, &mut session, "stationSvc", "GetStationItemBits", &[PyRep::Integer(jita_station)], &kw)
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected 4-tuple");
    };
    assert_eq!(bits.len(), 4);
    assert_eq!(bits[1], PyRep::Integer(jita_station));

    // station.GetGuests returns docked caller
    let PyRep::List(guests) = handle(&ctx, &mut session, "station", "GetGuests", &[], &kw)
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected list");
    };
    assert!(guests.iter().any(|g| matches!(g, PyRep::Tuple(t) if t.first() == Some(&PyRep::Integer(char_id)))));

    // DoStandingCheckForStationService
    assert_eq!(
        handle(&ctx, &mut session, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(0)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidStationService"
    );
    assert_eq!(
        handle(&ctx, &mut session, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(5)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // Undock from Jita 4-4 into space, then Dock back into Jita 4-4
    let PyRep::KeyVal(undock_kv) = handle(&ctx, &mut session, "stationSvc", "Undock", &[], &kw)
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected undock keyval");
    };
    assert_eq!(undock_kv.get("shipID").and_then(|v| v.as_i64()), Some(ship_id));
    assert_eq!(session.station_id, None);

    let PyRep::KeyVal(dock_kv) = handle(
        &ctx,
        &mut session,
        "stationSvc",
        "Dock",
        &[PyRep::Integer(jita_station)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!("expected dock keyval");
    };
    assert_eq!(dock_kv.get("locationID").and_then(|v| v.as_i64()), Some(jita_station));
    assert_eq!(dock_kv.get("isStructure").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(session.station_id, Some(jita_station));
}

#[tokio::test]
async fn pos_mgr_anchor_structure_validates_legacy_pos_and_deploys_mobile_depot() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let sys_id = 30_000_144_u32;
    let char_id = 910_610_i64;
    let ship_id = 910_611_i64;
    let depot_id = 910_612_i64;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(200_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, char_id, "Deployer", 98_000_001, sys_id, ship_id);
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            depot_id,
            InventoryItemRecord {
                item_id: depot_id,
                type_id: 33_474, // Mobile Depot
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let mut session = SessionState::new(1);
    session.user_id = Some(char_id as u64);
    session.character_id = Some(char_id);
    session.corporation_id = Some(98_000_001);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);
    let kw = HashMap::new();

    // Missing item -> StarbaseRemoved
    assert_eq!(
        handle(&ctx, &mut session, "posMgr", "AnchorStructure", &[PyRep::Integer(999_888_777)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StarbaseRemoved"
    );

    // Valid Mobile Depot in cargo -> anchors in space
    let PyRep::KeyVal(dep_kv) = handle(&ctx, &mut session, "posMgr", "AnchorStructure", &[PyRep::Integer(depot_id)], &kw)
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected keyval");
    };
    assert_eq!(dep_kv.get("itemID").and_then(|v| v.as_i64()), Some(depot_id));
    assert_eq!(dep_kv.get("state").and_then(|v| v.as_str()), Some("Anchoring"));
}

#[tokio::test]
async fn criminal_undocking_in_highsec_schedules_concord() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let jita_station = 60_003_760_i64;
    let jita_sys = 30_000_142_u32;
    let char_id = 910_691_i64;
    let corp_id = 98_600_691_i64;
    let ship_id = 910_692_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, char_id, "Crim Undock", corp_id, jita_sys, ship_id);
        ch.station_id = Some(jita_station);
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let now_u = ctx.now_unix();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char_id).criminal_flag_until_unix = now_u + 900;
    }

    let mut session = SessionState::new(1);
    session.user_id = Some(char_id as u64);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.solar_system_id = Some(jita_sys);
    session.station_id = Some(jita_station);
    session.ship_id = Some(ship_id);

    handle(&ctx, &mut session, "stationSvc", "Undock", &[], &HashMap::new())
        .await
        .expect("handled")
        .expect("undock ok");

    let pending = ctx.crimewatch.read().unwrap().states[&char_id].pending_concord_arrival;
    assert_eq!(pending.map(|p| (p.0, p.2)), Some((jita_sys, ship_id)));
}

#[tokio::test]
async fn docking_aborts_active_self_destruct_countdown() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let jita_station = 60_003_760_i64;
    let jita_sys = 30_000_142_u32;
    let char_id = 910_695_i64;
    let corp_id = 98_600_695_i64;
    let ship_id = 910_696_i64;
    let station_pos = ctx.sde.stations.get(&jita_station).unwrap().position;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, char_id, "Dock Abort Pilot", corp_id, jita_sys, ship_id);
        ch.station_id = None;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: jita_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.systems
        .get_or_spawn(jita_sys)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: station_pos + DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .expect("spawn ball");
    let now_u = ctx.now_unix();
    crate::sp6_lifecycle::register_self_destruct_countdown(&ctx, ship_id, char_id, jita_sys, now_u, now_u + 120);
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.self_destruct_countdowns.insert(
            ship_id,
            crate::services::combat_crimewatch::SelfDestructCountdown {
                ship_id,
                char_id,
                solar_system_id: jita_sys,
                started_unix: now_u,
                detonate_at_unix: now_u + 120,
            },
        );
    });

    let mut session = SessionState::new(1);
    session.user_id = Some(char_id as u64);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.solar_system_id = Some(jita_sys);
    session.ship_id = Some(ship_id);
    handle(&ctx, &mut session, "stationSvc", "Dock", &[PyRep::Integer(jita_station)], &HashMap::new())
        .await
        .expect("handled")
        .expect("dock ok");

    crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        assert!(!st.self_destructs.contains_key(&ship_id));
    });
    crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        assert!(!rt.self_destruct_countdowns.contains_key(&ship_id));
    });
}
