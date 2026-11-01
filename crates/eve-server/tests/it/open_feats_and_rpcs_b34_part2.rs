#![allow(unused_imports)]
//! Batch 34 Part 2: Normal Cyno Lighting Rules, Tenebrex Jammer State/Reinforcement,
//! and Strengthened Bastion & Command Burst Assertions.

use crate::modules_advanced_tactical::*;
use eve_db::{FleetRecord, InventoryFlag, UpwellStructureRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn b34_normal_cyno_lighting_rules() {
    // parity: feat:modules.cyno-lighting-rules
    eve_testkit::attest(&["feat:modules.cyno-lighting-rules"]);
    let ctx = build_ctx();

    // 1. Not in fleet (even when s.fleets is completely empty!) -> MustBeInFleet (B34-F-03)
    let mut solo_sess = seed_pilot(&ctx, 94101, 84101, 11957, NULL_SYS, true);
    fit_mod(&ctx, &mut solo_sess, 84101, 74101, 21096, 27).await.expect("fit Cynosural Field Generator I");
    give_item(&ctx, 74102, 16273, 94101, 84101, InventoryFlag::Cargo, 1_000);
    solo_sess.station_id = None;
    spawn_in_space(&ctx, 94101, 84101, 11957, NULL_SYS, DVec3::ZERO).await;

    let err_no_fleet = rpc(&ctx, &mut solo_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74101), PyRep::String("cynosuralGeneration".into())])
        .await
        .unwrap_err();
    assert_eq!(err_no_fleet.code, "MustBeInFleet", "Normal cyno must require fleet even when server has no fleets");

    // 2. High-sec system (30000142 Jita) -> CannotLightCynoInSystem
    let mut hs_sess = seed_pilot(&ctx, 94102, 84102, 11957, 30_000_142, true);
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(5101, FleetRecord {
            fleet_id: 5101,
            boss_id: 94102,
            free_move: true,
            members: vec![94102, 94101],
            ..Default::default()
        });
    });
    hs_sess.fleet_id = Some(5101);
    fit_mod(&ctx, &mut hs_sess, 84102, 74103, 21096, 27).await.expect("fit cyno in highsec");
    give_item(&ctx, 74104, 16273, 94102, 84102, InventoryFlag::Cargo, 1_000);
    hs_sess.station_id = None;
    spawn_in_space(&ctx, 94102, 84102, 11957, 30_000_142, DVec3::ZERO).await;
    let err_hs = rpc(&ctx, &mut hs_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74103), PyRep::String("cynosuralGeneration".into())])
        .await
        .unwrap_err();
    assert_eq!(err_hs.code, "CannotLightCynoInSystem");

    // 3. Speed >= 500 m/s -> ShipMovingTooFast
    solo_sess.fleet_id = Some(5101);
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let _ = sys.spawn_ball(Ball {
        id: 84101,
        type_id: 621,
        position: DVec3::ZERO,
        velocity: DVec3::new(600.0, 0.0, 0.0),
        max_velocity: 1000.0,
        inertia: 1.0,
        mass: 10_000_000.0,
        radius: 100.0,
        speed_fraction: 0.6,
        mode: MovementMode::Stop,
    }).await;
    let err_fast = rpc(&ctx, &mut solo_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74101), PyRep::String("cynosuralGeneration".into())])
        .await
        .unwrap_err();
    assert_eq!(err_fast.code, "ShipMovingTooFast");

    // Stop ship
    let _ = sys.spawn_ball(Ball {
        id: 84101,
        type_id: 621,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 1000.0,
        inertia: 1.0,
        mass: 10_000_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;

    // 4. Insufficient Liquid Ozone (< 50 LO on Force Recon) -> NotEnoughFuel
    ctx.db.with_mut_snapshot(|s| {
        if let Some(lo) = s.items.get_mut(&74102) {
            lo.quantity = 40;
        }
    });
    let err_lo = rpc(&ctx, &mut solo_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74101), PyRep::String("cynosuralGeneration".into())])
        .await
        .unwrap_err();
    assert_eq!(err_lo.code, "NotEnoughFuel");

    // 5. Valid normal cyno activation: consumes 50 LO (Force Recon 90% reduction from 500 LO), 5-minute (300s) cycle
    ctx.db.with_mut_snapshot(|s| {
        if let Some(lo) = s.items.get_mut(&74102) {
            lo.quantity = 100;
        }
    });
    let ok_cyno = rpc(&ctx, &mut solo_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74101), PyRep::String("cynosuralGeneration".into())])
        .await
        .expect("light normal cyno");
    assert_eq!(ok_cyno.dict_get("consumptionQuantity").and_then(|v| v.as_i64()), Some(50));
    let rem_lo = ctx.db.with_snapshot(|s| s.items.get(&74102).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(rem_lo, 50, "Normal cyno must consume 50 Liquid Ozone on Force Recon");
}

#[tokio::test]
async fn b34_tenebrex_jammer_state_and_reinforcement() {
    // parity: feat:modules.cynosural-field-jammer-structure
    eve_testkit::attest(&["feat:modules.cynosural-field-jammer-structure"]);
    let ctx = build_ctx();
    let jam_sys = 30_004_761u32;
    let mut sess = seed_pilot(&ctx, 94110, 84110, 621, jam_sys, true);
    sess.station_id = None;
    spawn_in_space(&ctx, 94110, 84110, 621, jam_sys, DVec3::ZERO).await;

    let now_u = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(998_200, UpwellStructureRecord {
            structure_id: 998_200,
            type_id: 37534,
            owner_corp_id: 1000125,
            solar_system_id: jam_sys,
            name: "Tenebrex".into(),
            position: [0.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 100,
            shield_hp: 100_000.0,
            armor_hp: 100_000.0,
            hull_hp: 100_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        });
        s.sov_runtime.tenebrex_spool_ready_unix.insert(998_200, now_u + 300);
    });

    // Spooling up -> not active yet
    assert!(eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, jam_sys).is_none());

    // After 300s spool -> active
    ctx.advance_virtual_clock(305);
    assert!(eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, jam_sys).is_some());

    let b_id = ctx.bind_object(&mut sess, "beyonce", jam_sys as i64).unwrap();
    let state_rep = ctx.dispatch_rpc(&mut sess, "beyonce", Some(b_id), "GetCynoJammerState", &[], &HashMap::new(), None)
        .await
        .expect("GetCynoJammerState");
    assert!(matches!(state_rep, PyRep::Integer(ts) if ts > 0), "GetCynoJammerState must return online timestamp when jammed, got {state_rep:?}");

    // Armor/hull reinforcement disables Tenebrex cyno jammer
    ctx.db.with_mut_snapshot(|s| {
        if let Some(st) = s.structures.get_mut(&998_200) {
            st.state = "ArmorReinforced".into();
            st.reinforced_until_unix = ctx.now_unix_secs() + 86_400;
        }
    });
    assert!(
        eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer(&ctx, jam_sys).is_none(),
        "Reinforced Tenebrex must NOT jam cynos"
    );
    let state_after = ctx.dispatch_rpc(&mut sess, "beyonce", Some(b_id), "GetCynoJammerState", &[], &HashMap::new(), None)
        .await
        .expect("GetCynoJammerState after reinforcement");
    assert_eq!(state_after, PyRep::None, "GetCynoJammerState must return None when reinforced");
}
