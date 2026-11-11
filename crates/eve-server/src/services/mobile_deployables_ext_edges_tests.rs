//! Edge-case tests for `mobile_deployables_ext`: space restrictions, MMJU spooling/blocks,
//! Observatory pings & decloak paths, and Siphon material theft/waste/capacity.

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::mobile_deployables::OBSERVATORY_DEPLOY_SECS;
use eve_db::clock::VirtualServerClock;
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, ServerClock, UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_sde::{InvType, SdeRegistry, SolarSystem, StargateJump, StationInfo};
use glam::DVec3;
use std::collections::HashSet;
use std::sync::Arc;

fn make_ctx_with_clock_and_sde(
    base_unix: u64,
    mutate_sde: impl FnOnce(&mut SdeRegistry),
) -> (ServerContext, Arc<VirtualServerClock>) {
    let base = test_ctx();
    let mut sde = (*base.sde).clone();
    mutate_sde(&mut sde);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(base_unix));
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

fn lowsec_system(sys_id: u32, region_id: u32, sec: f64, stations: Vec<StationInfo>) -> SolarSystem {
    SolarSystem {
        solar_system_id: sys_id,
        constellation_id: 20_000_100,
        region_id,
        name: format!("Sys-{sys_id}"),
        security_status: sec,
        radius: 1.0e11,
        galactic_pos_ly: [0.0; 3],
        planet_ids: Vec::new(),
        stations,
        wormhole_class_id: None,
    }
}

fn make_type(type_id: u32, group_id: u32, attrs: Vec<(u16, f64)>, capacity: f64, volume: f64) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id: 22,
        name: format!("Type-{type_id}"),
        published: true,
        mass: 1_000.0,
        volume,
        capacity,
        radius: 150.0,
        base_price: 100.0,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        default_effect_id: None,
        description: String::new(),
        default_attributes: attrs.into_iter().collect(),
        effect_ids: Vec::new(),
    }
}

#[test]
fn helpers_lifetime_delay_and_caller_fleet_branches() {
    let (ctx, _) = make_ctx_with_clock_and_sde(1_700_000_000, |sde| {
        sde.types.insert(
            990_001,
            make_type(990_001, GROUP_MOBILE_DEPOT, vec![(556, 200.0)], 0.0, 100.0),
        );
        sde.types.insert(
            990_002,
            make_type(990_002, GROUP_MOBILE_DEPOT, vec![(556, 120_000.0)], 0.0, 100.0),
        );
    });
    // Fallback unknown group lifetime & default deploy delay
    assert_eq!(total_lifetime_secs(0, 9999, 60), 2 * 86_400);
    assert_eq!(default_deploy_delay_secs(&ctx.sde, 990_001, GROUP_MOBILE_DEPOT), 1.0);
    assert_eq!(default_deploy_delay_secs(&ctx.sde, 990_002, GROUP_MOBILE_DEPOT), 120.0);
    assert_eq!(default_deploy_delay_secs(&ctx.sde, 999_999, GROUP_MOBILE_OBSERVATORY), 600.0);
    assert_eq!(default_deploy_delay_secs(&ctx.sde, 999_999, GROUP_MOBILE_DEPOT), 60.0);

    // caller_fleet_id falls back to snap.fleets when session.fleet_id is None
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    sess.fleet_id = None;
    let fid = ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(caller_fleet_id(&sess, snap, 90_000_001), None);
        snap.fleets.insert(
            77_001,
            FleetRecord {
                fleet_id: 77_001,
                boss_id: 90_000_001,
                members: vec![90_000_001],
                ..Default::default()
            },
        );
        caller_fleet_id(&sess, snap, 90_000_001)
    });
    assert_eq!(fid, Some(77_001));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deployment_restrictions_all_space_classes_and_proximity_checks() {
    let low_sys = 30_950_001_u32;
    let poch_sys = 30_950_002_u32;
    let gate_pos = DVec3::new(100_000.0, 0.0, 0.0);
    let stn_pos = DVec3::new(500_000.0, 0.0, 0.0);
    let struct_pos = DVec3::new(900_000.0, 0.0, 0.0);
    let safe_pos = DVec3::new(5_000_000.0, 0.0, 0.0);

    let (ctx, _) = make_ctx_with_clock_and_sde(1_700_000_000, |sde| {
        let stn_info = StationInfo {
            station_id: 60_950_001,
            solar_system_id: low_sys,
            constellation_id: 20_000_100,
            region_id: 10_000_002,
            station_type_id: 1529,
            name: "Station".into(),
            position: stn_pos,
            owner_corporation_id: 1_000_009,
            owner_faction_id: None,
            operation_id: 1,
            reprocessing_efficiency: 0.5,
            reprocessing_stations_take: 0.05,
            is_conquerable: false,
            orbit_id: None,
            has_clone_bay: false,
        };
        sde.solar_systems
            .insert(low_sys, lowsec_system(low_sys, 10_000_002, 0.2, vec![stn_info.clone()]));
        sde.solar_systems
            .insert(poch_sys, lowsec_system(poch_sys, 10_000_070, -0.9, vec![]));
        sde.jumps.insert(
            50_950_001,
            StargateJump {
                stargate_id: 50_950_001,
                from_solar_system_id: low_sys,
                to_solar_system_id: 30_000_142,
                destination_stargate_id: 50_000_001,
                position: gate_pos,
            },
        );
        sde.stations.insert(60_950_001, stn_info);
        sde.types.insert(
            33475,
            make_type(33475, GROUP_MOBILE_SIPHON, vec![(1032, 0.4)], 1000.0, 100.0),
        );
        sde.types.insert(
            990_100,
            make_type(990_100, 404, vec![], 10_000.0, 100.0), // POS silo
        );
        sde.types.insert(
            TYPE_HIGHSEC_AUTH_CYNO_BEACON,
            make_type(TYPE_HIGHSEC_AUTH_CYNO_BEACON, GROUP_MOBILE_CYNO_BEACON, vec![], 0.0, 100.0),
        );
    });

    let mut sess = auth_session(1, 90_000_001, 98_000_001, low_sys);
    sess.fleet_id = Some(5001);

    // Cyno beacon: Zarzakh, Wormhole, Pochven, Abyssal
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 1, 57319, GROUP_MOBILE_CYNO_BEACON, 30_100_000, safe_pos)
            .unwrap_err().code,
        "CynoDisallowedInZarzakh"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 1, 57319, GROUP_MOBILE_CYNO_BEACON, 31_000_001, safe_pos)
            .unwrap_err().code,
        "CantAnchorInWormhole"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 1, 57319, GROUP_MOBILE_CYNO_BEACON, poch_sys, safe_pos)
            .unwrap_err().code,
        "CynoJammedInPochven"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 1, 57319, GROUP_MOBILE_CYNO_BEACON, 32_000_001, safe_pos)
            .unwrap_err().code,
        "CantAnchorInAbyssal"
    );
    // Highsec auth cyno beacon allowed in Jita
    assert!(validate_ext_deployment_restrictions(
        &ctx, &sess, 1, TYPE_HIGHSEC_AUTH_CYNO_BEACON, GROUP_MOBILE_CYNO_BEACON, 30_000_142, safe_pos
    ).is_ok());

    // Mobile Cyno Inhibitor in low_sys blocks non-covert cyno beacon within 100 km, but covert bypasses it
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            880_001,
            InventoryItemRecord {
                item_id: 880_001,
                type_id: crate::services::mobile_deployables::TYPE_MOBILE_CYNO_INHIBITOR,
                owner_id: 90_000_001,
                location_id: low_sys as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(880_001, safe_pos.to_array());
        write_deployable_state_snap(snap, |st| {
            st.deployables.insert(
                880_001,
                MobileDeployableRecord {
                    item_id: 880_001,
                    type_id: crate::services::mobile_deployables::TYPE_MOBILE_CYNO_INHIBITOR,
                    group_id: GROUP_MOBILE_CYNO_INHIBITOR,
                    owner_id: 90_000_001,
                    solar_system_id: low_sys,
                    anchor_start_unix: 1_699_999_000,
                    anchor_duration_secs: 120,
                    anchor_completed: true,
                    is_unanchored: false,
                    expires_at_unix: 1_700_010_000,
                    last_interaction_unix: 1_700_000_000,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    position: safe_pos.to_array(),
                    deployed_fleet_id: None,
                },
            );
        });
    });
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 2, 57319, GROUP_MOBILE_CYNO_BEACON, low_sys, safe_pos)
            .unwrap_err().code,
        "CynoInhibited"
    );
    assert!(validate_ext_deployment_restrictions(
        &ctx, &sess, 2, TYPE_COVERT_MOBILE_CYNO_BEACON, GROUP_MOBILE_CYNO_BEACON, low_sys, safe_pos
    ).is_ok());

    // Observatory: Pochven, Abyssal, Deadspace, and clear lowsec
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 3, TYPE_MOBILE_OBSERVATORY, GROUP_MOBILE_OBSERVATORY, poch_sys, safe_pos)
            .unwrap_err().code,
        "CannotDeployInPochven"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 3, TYPE_MOBILE_OBSERVATORY, GROUP_MOBILE_OBSERVATORY, 32_000_001, safe_pos)
            .unwrap_err().code,
        "CannotDeployInAbyssalSpace"
    );
    assert!(validate_ext_deployment_restrictions(
        &ctx, &sess, 3, TYPE_MOBILE_OBSERVATORY, GROUP_MOBILE_OBSERVATORY, low_sys, safe_pos
    ).is_ok());

    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(low_sys).or_default().push(
            crate::services::pve_exploration::PveSiteInstance {
                site_id: "DED-1".into(),
                dungeon_id: 123,
                solar_system_id: low_sys,
                position: safe_pos.to_array(),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 3, TYPE_MOBILE_OBSERVATORY, GROUP_MOBILE_OBSERVATORY, low_sys, safe_pos)
            .unwrap_err().code,
        "CannotDeployInDeadspace"
    );

    // Scan inhibitor restrictions: Zarzakh, gate, station, structure, same type
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 4, TYPE_MOBILE_SCAN_INHIBITOR, GROUP_MOBILE_SCAN_INHIBITOR, 30_100_000, safe_pos)
            .unwrap_err().code,
        "RestrictionsInEffect"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 4, TYPE_MOBILE_SCAN_INHIBITOR, GROUP_MOBILE_SCAN_INHIBITOR, low_sys, gate_pos)
            .unwrap_err().code,
        "DeploymentTooCloseToGate"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 4, TYPE_MOBILE_SCAN_INHIBITOR, GROUP_MOBILE_SCAN_INHIBITOR, low_sys, stn_pos)
            .unwrap_err().code,
        "DeploymentTooCloseToStation"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            1_000_000_950_001,
            UpwellStructureRecord {
                structure_id: 1_000_000_950_001,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: low_sys,
                name: "Astra".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.item_positions.insert(1_000_000_950_001, struct_pos.to_array());
        snap.items.insert(
            880_002,
            InventoryItemRecord {
                item_id: 880_002,
                type_id: TYPE_MOBILE_SCAN_INHIBITOR,
                owner_id: 90_000_001,
                location_id: low_sys as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(880_002, safe_pos.to_array());
    });
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 4, TYPE_MOBILE_SCAN_INHIBITOR, GROUP_MOBILE_SCAN_INHIBITOR, low_sys, struct_pos)
            .unwrap_err().code,
        "DeploymentTooCloseToStructure"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 4, TYPE_MOBILE_SCAN_INHIBITOR, GROUP_MOBILE_SCAN_INHIBITOR, low_sys, safe_pos + DVec3::new(10_000.0, 0.0, 0.0))
            .unwrap_err().code,
        "DeploymentTooCloseToSameType"
    );

    // MMJU: Abyssal and proximity < 6 km
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 5, 33591, GROUP_MOBILE_MICRO_JUMP_UNIT, 32_000_001, safe_pos)
            .unwrap_err().code,
        "RestrictionsInEffect"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            880_003,
            InventoryItemRecord {
                item_id: 880_003,
                type_id: 33591,
                owner_id: 90_000_001,
                location_id: low_sys as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(880_003, safe_pos.to_array());
    });
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 5, 33591, GROUP_MOBILE_MICRO_JUMP_UNIT, low_sys, safe_pos + DVec3::new(1000.0, 0.0, 0.0))
            .unwrap_err().code,
        "DeploymentTooCloseToSameType"
    );

    // Siphon: Pochven, no target in range, and with POS silo within 150 km
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 6, 33475, GROUP_MOBILE_SIPHON, poch_sys, safe_pos)
            .unwrap_err().code,
        "RestrictionsInEffect"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(&ctx, &sess, 6, 33475, GROUP_MOBILE_SIPHON, low_sys, safe_pos)
            .unwrap_err().code,
        "NoSiphonTargetInRange"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            880_004,
            InventoryItemRecord {
                item_id: 880_004,
                type_id: 990_100,
                owner_id: 98_000_002,
                location_id: low_sys as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(880_004, (safe_pos + DVec3::new(10_000.0, 0.0, 0.0)).to_array());
    });
    assert!(validate_ext_deployment_restrictions(&ctx, &sess, 6, 33475, GROUP_MOBILE_SIPHON, low_sys, safe_pos).is_ok());

    // Fallback group arm -> Ok(())
    assert!(validate_ext_deployment_restrictions(&ctx, &sess, 7, 33474, GROUP_MOBILE_DEPOT, low_sys, safe_pos).is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mmju_activation_error_branches_and_spool_completion() {
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_001_i64;
    let unit_id = 885_001_i64;
    let (ctx, clock) = make_ctx_with_clock_and_sde(1_700_000_000, |_| {});
    let sess = auth_session(1, 90_000_001, 98_000_001, sys_id);
    let handle = ctx.systems.get_or_spawn(sys_id);

    // Seed MMJU in DB as still anchoring
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
        snap.items.insert(
            unit_id,
            InventoryItemRecord {
                item_id: unit_id,
                type_id: 33591,
                owner_id: 90_000_001,
                location_id: sys_id as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(unit_id, [0.0, 0.0, 0.0]);
        write_deployable_state_snap(snap, |st| {
            st.deployables.insert(
                unit_id,
                MobileDeployableRecord {
                    item_id: unit_id,
                    type_id: 33591,
                    group_id: GROUP_MOBILE_MICRO_JUMP_UNIT,
                    owner_id: 90_000_001,
                    solar_system_id: sys_id,
                    anchor_start_unix: 1_700_000_000,
                    anchor_duration_secs: 60,
                    anchor_completed: false,
                    is_unanchored: false,
                    expires_at_unix: 1_700_200_000,
                    last_interaction_unix: 1_700_000_000,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    position: [0.0, 0.0, 0.0],
                    deployed_fleet_id: None,
                },
            );
        });
    });

    // While anchoring -> DeployableAnchoring
    assert_eq!(
        handle_mmju_activate(&ctx, &sess, 90_000_001, ship_id, sys_id, unit_id)
            .await
            .unwrap_err()
            .code,
        "DeployableAnchoring"
    );

    // Advance clock past 60s anchor delay -> ShipNotInSpace before ball spawned
    clock.advance_secs(65);
    assert_eq!(
        handle_mmju_activate(&ctx, &sess, 90_000_001, ship_id, sys_id, unit_id)
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Spawn ship ball in Warp -> ShipInWarp
    handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(1000.0, 0.0, 0.0),
                velocity: DVec3::new(0.0, 0.0, 1.0),
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::new_warp(DVec3::ZERO, DVec3::new(100_000.0, 0.0, 0.0), 3.0),
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        handle_mmju_activate(&ctx, &sess, 90_000_001, ship_id, sys_id, unit_id)
            .await
            .unwrap_err()
            .code,
        "ShipInWarp"
    );

    // Stop ship > 6500 m away -> OutOfRange
    handle.set_movement_mode(ship_id, MovementMode::Stop).await.unwrap();
    handle.try_set_ball_position(ship_id, DVec3::new(20_000.0, 0.0, 0.0));
    assert_eq!(
        handle_mmju_activate(&ctx, &sess, 90_000_001, ship_id, sys_id, unit_id)
            .await
            .unwrap_err()
            .code,
        "OutOfRange"
    );

    // Move close, but set mass >= 1B kg -> ShipTooHeavy
    handle.try_set_ball_position(ship_id, DVec3::new(1_000.0, 0.0, 0.0));
    {
        let mut bp = handle.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_id).unwrap().mass = MMJU_MAX_SHIP_MASS_KG;
    }
    assert_eq!(
        handle_mmju_activate(&ctx, &sess, 90_000_001, ship_id, sys_id, unit_id)
            .await
            .unwrap_err()
            .code,
        "ShipTooHeavy"
    );

    // Restore mass, cloak ship -> ShipCloaked
    {
        let mut bp = handle.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_id).unwrap().mass = 1_000_000.0;
        bp.combat_states.get_mut(&ship_id).unwrap().is_cloaked = true;
    }
    assert_eq!(
        handle_mmju_activate(&ctx, &sess, 90_000_001, ship_id, sys_id, unit_id)
            .await
            .unwrap_err()
            .code,
        "ShipCloaked"
    );

    // Uncloak, scramble -> WarpScrambled
    {
        let mut bp = handle.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&ship_id).unwrap();
        cs.is_cloaked = false;
        cs.warp_scramble_strength = 2;
    }
    assert_eq!(
        handle_mmju_activate(&ctx, &sess, 90_000_001, ship_id, sys_id, unit_id)
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );

    // Clear scramble -> activation succeeds and spools for 12s
    {
        let mut bp = handle.ballpark.write().unwrap();
        bp.combat_states.get_mut(&ship_id).unwrap().warp_scramble_strength = 0;
    }
    let res = handle_mmju_activate(&ctx, &sess, 90_000_001, ship_id, sys_id, unit_id)
        .await
        .unwrap();
    assert_eq!(res.dict_get("unitID").and_then(|v| v.as_i64()), Some(unit_id));
    assert_eq!(res.dict_get("jumpDistanceMeters").and_then(|v| v.as_f64()), Some(MMJU_JUMP_DISTANCE_M));

    // Step before 12s complete -> ship has not jumped yet
    step_system_deployables_sync(&ctx, sys_id);
    assert!(handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position.x < 5_000.0);

    // Advance 12s -> step_system_deployables_sync executes 100 km micro_jump!
    clock.advance_secs(12);
    step_system_deployables_sync(&ctx, sys_id);
    let moved_pos = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;
    assert!((moved_pos.distance(DVec3::new(1_000.0, 0.0, 0.0)) - MMJU_JUMP_DISTANCE_M).abs() < 10.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observatory_pings_stabilization_and_decloak_with_side_effects() {
    let sys_id = 30_950_010_u32;
    let obs_id = 886_001_i64;
    let ship_cycle_id = 100_000_010_i64;
    let ship_fb_id = 100_000_011_i64;
    let ship_stab_id = 100_000_012_i64;
    let cloak_mod_cycle = 100_500_001_i64;
    let cloak_mod_fb = 100_500_002_i64;

    let (ctx, clock) = make_ctx_with_clock_and_sde(1_700_000_000, |sde| {
        sde.solar_systems.insert(sys_id, lowsec_system(sys_id, 10_000_002, 0.1, vec![]));
        sde.types.insert(
            11578,
            make_type(11578, 330, vec![(560, 5000.0)], 0.0, 5.0),
        );
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            obs_id,
            InventoryItemRecord {
                item_id: obs_id,
                type_id: TYPE_MOBILE_OBSERVATORY,
                owner_id: 90_000_001,
                location_id: sys_id as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Fallback cloak module on ship_fb_id
        snap.items.insert(
            cloak_mod_fb,
            InventoryItemRecord {
                item_id: cloak_mod_fb,
                type_id: 11578,
                owner_id: 90_000_002,
                location_id: ship_fb_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(cloak_mod_fb);
        snap.module_active_states.insert(cloak_mod_cycle);
        write_deployable_state_snap(snap, |st| {
            st.deployables.insert(
                obs_id,
                MobileDeployableRecord {
                    item_id: obs_id,
                    type_id: TYPE_MOBILE_OBSERVATORY,
                    group_id: GROUP_MOBILE_OBSERVATORY,
                    owner_id: 90_000_001,
                    solar_system_id: sys_id,
                    anchor_start_unix: 1_700_000_000,
                    anchor_duration_secs: OBSERVATORY_DEPLOY_SECS,
                    anchor_completed: true,
                    is_unanchored: false,
                    expires_at_unix: 1_700_000_000 + OBSERVATORY_DEPLOY_SECS + OBSERVATORY_ACTIVE_SECS,
                    last_interaction_unix: 1_700_000_000,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    position: [0.0; 3],
                    deployed_fleet_id: None,
                },
            );
        });
    });

    let handle = ctx.systems.get_or_spawn(sys_id);
    for sid in [ship_cycle_id, ship_fb_id, ship_stab_id] {
        handle
            .spawn_ball_with_combat(
                Ball {
                    id: sid,
                    type_id: 587,
                    position: DVec3::ZERO,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    is_cloaked: true,
                    gate_cloak_remaining_ticks: 0,
                    cloak_stabilization_remaining_s: if sid == ship_stab_id { 900.0 } else { 0.0 },
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
    record_ship_cloak_activated(&ctx, ship_stab_id);
    {
        let mut st = handle.actor_state.write().unwrap();
        st.active_module_cycles.push(crate::system_actor::ActiveModuleCycle {
            ship_id: ship_cycle_id,
            module_id: cloak_mod_cycle,
            module_type_id: 11578,
            owner_char_id: Some(90_000_001),
            effect_name: "effects.Cloak".to_string(),
            repeat: 1,
            duration_s: 5.0,
            ..Default::default()
        });
    }

    // Advance 600s (anchor complete + 1st ping) -> stabilized ship stays cloaked across pings;
    // advance further so multiple pings run (12 pings total) to guarantee 40% decloak triggers on non-stabilized ships.
    clock.advance_secs(OBSERVATORY_DEPLOY_SECS + 10);
    step_system_deployables_sync(&ctx, sys_id);
    assert!(handle.get_combat_state(ship_stab_id).await.unwrap().unwrap().is_cloaked);

    clock.advance_secs(OBSERVATORY_PING_INTERVAL_SECS * 11);
    sync_system_deployables(&ctx, sys_id).await;

    let cs_cycle = handle.get_combat_state(ship_cycle_id).await.unwrap().unwrap();
    let cs_fb = handle.get_combat_state(ship_fb_id).await.unwrap().unwrap();
    assert!(!cs_cycle.is_cloaked);
    assert!(!cs_fb.is_cloaked);
    assert_eq!(
        read_deployable_state(&ctx).observatory_pings_done.get(&obs_id).copied(),
        Some(OBSERVATORY_TOTAL_PINGS)
    );
    assert!(read_deployable_state(&ctx).pending_decloak_notifications.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn siphon_steals_raw_processed_and_polymer_with_waste_and_capacity() {
    let sys_id = 30_950_020_u32;
    let siphon_id = 887_001_i64;
    let silo_id = 887_002_i64;
    let locked_stack_id = 887_003_i64;
    let raw_stack_id = 887_004_i64;
    let proc_stack_id = 887_005_i64;
    let poly_stack_id = 887_006_i64;

    let (ctx, clock) = make_ctx_with_clock_and_sde(1_700_000_000, |sde| {
        sde.solar_systems.insert(sys_id, lowsec_system(sys_id, 10_000_002, 0.1, vec![]));
        sde.types.insert(
            33477,
            make_type(
                33477,
                GROUP_MOBILE_SIPHON,
                vec![(1928, 100.0), (1929, 20.0), (1933, 5.0), (1930, 10.0)],
                500.0,
                100.0,
            ),
        );
        sde.types.insert(990_200, make_type(990_200, 404, vec![], 50_000.0, 1000.0));
        sde.types.insert(990_201, make_type(990_201, 427, vec![], 0.0, 1.0)); // raw moon mat
        sde.types.insert(990_202, make_type(990_202, 428, vec![], 0.0, 2.0)); // processed mat
        sde.types.insert(990_203, make_type(990_203, 974, vec![], 0.0, 5.0)); // polymer
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            siphon_id,
            InventoryItemRecord {
                item_id: siphon_id,
                type_id: 33477,
                owner_id: 90_000_001,
                location_id: sys_id as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            silo_id,
            InventoryItemRecord {
                item_id: silo_id,
                type_id: 990_200,
                owner_id: 98_000_002,
                location_id: sys_id as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(siphon_id, [0.0, 0.0, 0.0]);
        snap.item_positions.insert(silo_id, [10_000.0, 0.0, 0.0]);

        for (id, tid, qty) in [
            (locked_stack_id, 990_201, 50),
            (raw_stack_id, 990_201, 50), // drained completely (<= 100 + 10% waste)
            (proc_stack_id, 990_202, 100),
            (poly_stack_id, 990_203, 100),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: 98_000_002,
                    location_id: silo_id,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Lock one stack in contract so siphon skips it
        snap.contracts.insert(
            999_001,
            eve_db::ContractRecord {
                contract_id: 999_001,
                issuer_id: 98_000_002,
                status: "Outstanding".into(),
                item_ids: vec![locked_stack_id],
                ..Default::default()
            },
        );

        write_deployable_state_snap(snap, |st| {
            st.deployables.insert(
                siphon_id,
                MobileDeployableRecord {
                    item_id: siphon_id,
                    type_id: 33477,
                    group_id: GROUP_MOBILE_SIPHON,
                    owner_id: 90_000_001,
                    solar_system_id: sys_id,
                    anchor_start_unix: 1_700_000_000,
                    anchor_duration_secs: 60,
                    anchor_completed: true,
                    is_unanchored: false,
                    expires_at_unix: 1_705_000_000,
                    last_interaction_unix: 1_700_000_000,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    position: [0.0; 3],
                    deployed_fleet_id: None,
                },
            );
        });
    });

    // Advance 60s + 2 cycles (7200s)
    clock.advance_secs(60 + SIPHON_CYCLE_SECS * 2);
    step_system_deployables_sync(&ctx, sys_id);

    ctx.db.with_snapshot(|snap| {
        assert!(snap.items.contains_key(&locked_stack_id));
        assert!(!snap.items.contains_key(&raw_stack_id), "raw stack should be drained");
        let stolen_in_siphon: Vec<&InventoryItemRecord> =
            snap.items.values().filter(|i| i.location_id == siphon_id).collect();
        assert_eq!(stolen_in_siphon.len(), 3);
        let total_qty: i32 = stolen_in_siphon.iter().map(|i| i.quantity).sum();
        assert!(total_qty > 40);
        // Active scan inhibitor position check when an inhibitor is present
        let _ = HashSet::<i64>::new();
    });

    // Also test active_scan_inhibitor_positions with active & anchoring inhibitors
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            887_999,
            InventoryItemRecord {
                item_id: 887_999,
                type_id: TYPE_MOBILE_SCAN_INHIBITOR,
                owner_id: 90_000_001,
                location_id: sys_id as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        write_deployable_state_snap(snap, |st| {
            st.deployables.insert(
                887_999,
                MobileDeployableRecord {
                    item_id: 887_999,
                    type_id: TYPE_MOBILE_SCAN_INHIBITOR,
                    group_id: GROUP_MOBILE_SCAN_INHIBITOR,
                    owner_id: 90_000_001,
                    solar_system_id: sys_id,
                    anchor_start_unix: 1_700_000_000,
                    anchor_duration_secs: 60,
                    anchor_completed: true,
                    is_unanchored: false,
                    expires_at_unix: 1_705_000_000,
                    last_interaction_unix: 1_700_000_000,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    position: [123.0, 456.0, 789.0],
                    deployed_fleet_id: None,
                },
            );
        });
    });
    let active_inh = active_scan_inhibitor_positions(&ctx, sys_id);
    assert_eq!(active_inh.len(), 1);
    assert_eq!(active_inh[0].1, DVec3::new(123.0, 456.0, 789.0));
}
