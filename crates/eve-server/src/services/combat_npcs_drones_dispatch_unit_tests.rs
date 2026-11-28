//! Semantic unit tests split from `combat_npcs_drones_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::super::*;
use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::*;
use eve_db::{FleetRecord, InventoryFlag};
use eve_dogma::{FittedModule, ModuleState};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn pilot_drone_capacity_and_salvager_tractor_ownership_branches() {
    let (ctx, _session, char_id, ship_id, sys_id) = setup_drone_env();

    // Overloaded triage 4294 zeroes bandwidth; dynamic attribute 283 overrides drone bay capacity
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&ship_id)
            .unwrap()
            .dynamic_attributes
            .insert(283, 777.0);
    });
    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    fit.modules.insert(
        1,
        FittedModule {
            item_id: 1,
            type_id: 4294,
            flag_id: 27,
            state: ModuleState::Overloaded,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    let (max_d, bw, cap) = pilot_drone_capacity(&ctx, char_id, ship_id, Some(&fit));
    assert_eq!(max_d, 5);
    assert_eq!(bw, 0.0);
    assert!((cap - 777.0).abs() < 1e-6);

    let attacker_ball = make_ball(ship_id, 587, DVec3::ZERO, DVec3::ZERO);
    let dummy_mod = FittedModule {
        item_id: 50,
        type_id: 65001,
        flag_id: 27,
        state: ModuleState::Active,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: None,
        charge_quantity: 0,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let tractor_type = make_fighter_type(65001, 650, 7, 5.0, vec![(54, 20_000.0)]);
    let salvager_type = make_fighter_type(65002, 1122, 7, 5.0, vec![(54, 5_000.0)]);
    let kw = HashMap::new();

    // TargetNotInSpace when wreck ball is absent
    assert_eq!(
        activate_salvager_or_tractor_module(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            50,
            &dummy_mod,
            &tractor_type,
            &attacker_ball,
            Some(999_111),
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TargetNotInSpace"
    );

    // Spawn wreck at 25 km (out of 20 km tractor range and 5 km salvager range)
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let wreck_id = 910_500_i64;
    let wreck_ball = make_ball(wreck_id, 26_561, DVec3::new(25_000.0, 0.0, 0.0), DVec3::ZERO);
    let _ = sys_handle
        .spawn_ball(wreck_ball)
        .await;
    assert_eq!(
        activate_salvager_or_tractor_module(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            50,
            &dummy_mod,
            &tractor_type,
            &attacker_ball,
            Some(wreck_id),
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TargetOutOfRange"
    );

    // Move wreck to 10 km: out of salvager range (5 km), within tractor range (20 km)
    let _ = sys_handle
        .set_ball_position(wreck_id, DVec3::new(10_000.0, 0.0, 0.0))
        .await;
    assert_eq!(
        activate_salvager_or_tractor_module(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            50,
            &dummy_mod,
            &salvager_type,
            &attacker_ball,
            Some(wreck_id),
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TargetOutOfRange"
    );

    // Wreck owned by fleetmate is allowed to be tractored
    let other_pilot = 90_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_id,
            make_fighter_item(wreck_id, 26_561, other_pilot, sys_id as i64, InventoryFlag::Unlocked, 1),
        );
        snap.fleets.insert(
            7001,
            FleetRecord {
                fleet_id: 7001,
                boss_id: char_id,
                members: vec![char_id, other_pilot],
                free_move: true,
                is_registered: false,
                ..Default::default()
            },
        );
    });
    assert!(
        activate_salvager_or_tractor_module(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            50,
            &dummy_mod,
            &tractor_type,
            &attacker_ball,
            Some(wreck_id),
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
}

#[tokio::test]
async fn handle_npcs_drones_launch_return_engage_scoop_and_salvage_branches() {
    let (ctx, mut session, char_id, ship_id, sys_id) = setup_drone_env();
    let kw = HashMap::new();

    // NoSolarSystem on LaunchDrones
    session.solar_system_id = None;
    assert_eq!(
        handle_npcs_drones(&ctx, &mut session, "ship", "LaunchDrones", &[PyRep::Integer(1)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    session.solar_system_id = Some(sys_id);

    // Spawn the solar system before running in-space validation checks (`#1542` cold fail-closed).
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Drones skill 0 -> NoDroneManagementAbilities
    seed_skill(&ctx, char_id, 3436, 0);
    assert_eq!(
        handle_npcs_drones(&ctx, &mut session, "ship", "LaunchDrones", &[PyRep::Integer(999)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoDroneManagementAbilities"
    );
    seed_skill(&ctx, char_id, 3436, 5);

    // DroneNotFound and DroneNotInBay
    assert_eq!(
        handle_npcs_drones(&ctx, &mut session, "ship", "LaunchDrones", &[PyRep::Integer(999_888)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DroneNotFound"
    );
    let cargo_drone = 911_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cargo_drone,
            make_fighter_item(cargo_drone, 2_456, char_id, ship_id, InventoryFlag::Cargo, 1),
        );
    });
    assert_eq!(
        handle_npcs_drones(&ctx, &mut session, "ship", "LaunchDrones", &[PyRep::Integer(cargo_drone)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DroneNotInBay"
    );

    // Switch ship to Rorqual (28352) and launch an Excavator (41030) + Sentry (28280) + Salvage Drone (33681)
    let exc_id = 911_002_i64;
    let sen_id = 911_003_i64;
    let salv_id = 911_004_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 28_352;
        let mut exc = make_fighter_item(exc_id, 41_030, char_id, ship_id, InventoryFlag::DroneBay, 1);
        exc.dynamic_attributes.insert(263, 400.0);
        exc.dynamic_attributes.insert(265, 300.0);
        exc.dynamic_attributes.insert(9, 200.0);
        snap.items.insert(exc_id, exc);
        snap.items.insert(
            sen_id,
            make_fighter_item(sen_id, 28_280, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            salv_id,
            make_fighter_item(salv_id, 33_681, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );
    });
    let _ = sys_handle
        .spawn_ball(make_ball(ship_id, 28_352, DVec3::ZERO, DVec3::ZERO))
        .await;
    let launched = handle_npcs_drones(
        &ctx,
        &mut session,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![
            PyRep::Integer(exc_id),
            PyRep::Integer(sen_id),
            PyRep::Integer(salv_id),
        ])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(ref launched_ids) = launched else {
        panic!("expected List from LaunchDrones");
    };
    assert_eq!(launched_ids.len(), 3);
    let sen_ball_id = launched_ids[1].as_i64().unwrap();

    // CmdReturnAndOrbit: sentry stays stopped, excavator orbits ship
    handle_npcs_drones(&ctx, &mut session, "entity", "CmdReturnAndOrbit", &[], &kw)
        .await
        .unwrap()
        .unwrap();

    // CmdEngage with TargetNotInSpace error, then against PvP player ball (90_000_002)
    assert_eq!(
        handle_npcs_drones(
            &ctx,
            &mut session,
            "entity",
            "CmdEngage",
            &[PyRep::List(vec![PyRep::Integer(exc_id)]), PyRep::Integer(888_777)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TargetNotInSpace"
    );

    let pvp_victim = 90_000_002_i64;
    let victim_ball = make_ball(pvp_victim, 587, DVec3::new(1_200.0, 0.0, 0.0), DVec3::ZERO);
    let _ = sys_handle
        .spawn_ball(victim_ball)
        .await;
    let _ = sys_handle
        .add_target_lock(
            session.ship_id.unwrap(),
            eve_destiny::TargetLock {
                target_id: pvp_victim,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
    handle_npcs_drones(
        &ctx,
        &mut session,
        "entity",
        "CmdEngage",
        &[
            PyRep::List(vec![PyRep::Integer(exc_id)]),
            PyRep::Integer(pvp_victim),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    // SetAggressive via kwargs
    let mut aggr_kw = HashMap::new();
    aggr_kw.insert("aggressive".into(), PyRep::Bool(false));
    handle_npcs_drones(&ctx, &mut session, "entity", "SetAggressive", &[], &aggr_kw)
        .await
        .unwrap()
        .unwrap();

    // CmdSalvage without wreckID -> TargetRequired
    assert_eq!(
        handle_npcs_drones(&ctx, &mut session, "entity", "CmdSalvage", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetRequired"
    );

    // ScoopDrone error branches: DroneNotInSpace, OutOfScoopRange, DroneOwnedByOther
    assert_eq!(
        handle_npcs_drones(&ctx, &mut session, "ship", "ScoopDrone", &[PyRep::Integer(777_666)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DroneNotInSpace"
    );
    let _ = sys_handle
        .set_ball_position(sen_ball_id, DVec3::new(5_000.0, 0.0, 0.0))
        .await;
    assert_eq!(
        handle_npcs_drones(&ctx, &mut session, "ship", "ScoopDrone", &[PyRep::Integer(sen_ball_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OutOfScoopRange"
    );
    let _ = sys_handle.set_ball_position(sen_ball_id, DVec3::ZERO).await;
    crate::services::drone_test_helpers::update_active_drone(&ctx, sen_ball_id, |d| {
        d.owner_char_id = pvp_victim;
    })
    .unwrap();
    assert_eq!(
        handle_npcs_drones(&ctx, &mut session, "ship", "ScoopDrone", &[PyRep::Integer(sen_ball_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DroneOwnedByOther"
    );
}

#[tokio::test]
async fn cold_system_docked_cmd_engage_and_inspace_drone_commands_fail_closed_without_waking() {
    use crate::services::combat_crimewatch::combat_missiles_aoe::step_missiles_and_breachers;
    use crate::services::combat_crimewatch::combat_wrecks_killmails::{
        register_container_lifetime, step_wreck_and_jetcan_decay,
    };
    use crate::sp6_lifecycle::drone_return::{
        abandon_drones_and_fighters_for_ship_sync, abandon_drones_on_warp_sync, handle_cmd_return_bay,
    };
    use crate::system_actor::SystemLifecycleState;

    let (ctx, mut session, char_id, ship_id, _default_sys_id) = setup_drone_env();
    let cold_sys_id = 30_000_144_u32;
    let station_id = 60_003_760_i64;
    let struct_id = 1_000_000_400_001_i64;
    let drone_id = 911_901_i64;
    let target_id = 911_902_i64;
    let expired_can_id = 911_903_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = cold_sys_id;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = i64::from(cold_sys_id);
        }
        snap.items.insert(
            drone_id,
            make_fighter_item(drone_id, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            expired_can_id,
            make_fighter_item(expired_can_id, 23, char_id, i64::from(cold_sys_id), InventoryFlag::None, 1),
        );
    });
    register_container_lifetime(&ctx, expired_can_id, cold_sys_id, 23, char_id, 1, false);

    assert_eq!(ctx.systems.system_lifecycle_state(cold_sys_id), SystemLifecycleState::Cold);
    assert!(!ctx.systems.is_system_loaded(cold_sys_id));

    // 1. Docked CmdEngage (station and structure) returns ShipDocked before touching cold system.
    session.solar_system_id = Some(cold_sys_id);
    session.station_id = Some(station_id);
    session.structure_id = None;
    let station_engage_err = handle_npcs_drones(
        &ctx,
        &mut session,
        "entity",
        "CmdEngage",
        &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(target_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("docked CmdEngage must return ShipDocked");
    assert_eq!(station_engage_err.code, "ShipDocked");
    assert_eq!(ctx.systems.system_lifecycle_state(cold_sys_id), SystemLifecycleState::Cold);

    session.station_id = None;
    session.structure_id = Some(struct_id);
    let struct_engage_err = handle_npcs_drones(
        &ctx,
        &mut session,
        "entity",
        "CmdEngage",
        &[PyRep::Integer(target_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("structure-docked CmdEngage must return ShipDocked");
    assert_eq!(struct_engage_err.code, "ShipDocked");
    assert_eq!(ctx.systems.system_lifecycle_state(cold_sys_id), SystemLifecycleState::Cold);

    // 2. Undocked cold system: LaunchDrones, CmdEngage, CmdReturnBay fail closed with ShipNotInSpace.
    session.station_id = None;
    session.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
        }
    });

    let launch_err = handle_npcs_drones(
        &ctx,
        &mut session,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(drone_id)])],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("cold-system LaunchDrones must fail closed");
    assert_eq!(launch_err.code, "ShipNotInSpace");
    assert_eq!(
        launch_err
            .to_client_error("ship", "LaunchDrones")
            .as_user_error()
            .map(|(k, _)| k),
        Some(eve_proto::UserErrorKey::CantShipNotPresent)
    );

    let engage_err = handle_npcs_drones(
        &ctx,
        &mut session,
        "entity",
        "CmdEngage",
        &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(target_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("cold-system CmdEngage must fail closed");
    assert_eq!(engage_err.code, "ShipNotInSpace");
    assert_eq!(
        engage_err
            .to_client_error("entity", "CmdEngage")
            .as_user_error()
            .map(|(k, _)| k),
        Some(eve_proto::UserErrorKey::CantShipNotPresent)
    );

    let return_dispatch_err = handle_npcs_drones(
        &ctx,
        &mut session,
        "entity",
        "CmdReturnBay",
        &[PyRep::List(vec![PyRep::Integer(drone_id)])],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("cold-system entity.CmdReturnBay must fail closed");
    assert_eq!(return_dispatch_err.code, "ShipNotInSpace");

    let return_direct_err = handle_cmd_return_bay(
        &ctx,
        &mut session,
        &[PyRep::Integer(drone_id)],
        &kw,
    )
    .await
    .expect_err("cold-system drone_return::handle_cmd_return_bay must fail closed");
    assert_eq!(return_direct_err.code, "ShipNotInSpace");
    assert_eq!(
        return_direct_err
            .to_client_error("entity", "CmdReturnBay")
            .as_user_error()
            .map(|(k, _)| k),
        Some(eve_proto::UserErrorKey::CantShipNotPresent)
    );

    // 3. Cold-safe no-op/cleanup helpers stay Cold while still pruning DB decay records.
    let abandon_res = handle_npcs_drones(&ctx, &mut session, "entity", "CmdAbandonDrone", &[], &kw)
        .await
        .expect("handled")
        .expect("cold CmdAbandonDrone returns empty dict");
    assert_eq!(abandon_res, PyRep::Dict(Vec::new()));

    let reconnect_res = handle_npcs_drones(&ctx, &mut session, "entity", "CmdReconnectToDrones", &[], &kw)
        .await
        .expect("handled")
        .expect("cold CmdReconnectToDrones returns empty dict");
    assert_eq!(reconnect_res, PyRep::Dict(Vec::new()));

    assert_eq!(
        apply_damage_to_drone_in_space(
            &ctx,
            cold_sys_id,
            999_001,
            DamageVector::new(10.0, 0.0, 0.0, 0.0),
        )
        .await,
        (0.0, false)
    );
    assert!(step_drone_auto_aggression_and_assist(&ctx, cold_sys_id, char_id, None, None, None)
        .await
        .is_empty());
    abandon_drones_on_warp_sync(&ctx, char_id, ship_id, cold_sys_id);
    abandon_drones_and_fighters_for_ship_sync(&ctx, char_id, ship_id, cold_sys_id);
    assert!(step_missiles_and_breachers(&ctx, cold_sys_id, 1.0).await.is_empty());

    let decayed = step_wreck_and_jetcan_decay(&ctx, cold_sys_id, ctx.now_unix()).await;
    assert_eq!(decayed, vec![expired_can_id]);
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&expired_can_id)));

    assert_eq!(ctx.systems.system_lifecycle_state(cold_sys_id), SystemLifecycleState::Cold);
    assert!(!ctx.systems.is_system_loaded(cold_sys_id));
}
