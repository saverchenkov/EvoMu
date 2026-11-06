//! Extra branch-coverage and mutation-killing unit tests for `doomsday_aoe_ops`.

#![forbid(unsafe_code)]

use std::sync::Arc;

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::PyRepTestExt;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
use crate::services::doomsday_aoe_ops::{
    activate_cone_doomsday, activate_multipoint_reaper_doomsday, activate_spatial_gtfo_doomsday,
    dispatch_superweapon_aoe_rpc, DoomsdayAoeMode,
};
use crate::services::doomsday_aoe_tick::read_superweapon_runtime;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SolarSystem;

fn build_titan_test_env() -> (ServerContext, SessionState, i64, i64, u32, i64, i64) {
    let (mut ctx, mut sess, char_id, ship_id, _highsec_id) = make_fighter_ctx_and_session();
    let null_sys_id = 30_004_759_u32;
    let titan_type_id = 11_567_u32; // Avatar (group 30, category 6)
    let dd_mod_type_id = 24_550_u32;
    let mod_id = 551_000_001_i64;
    let fuel_id = 551_000_002_i64;

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut.solar_systems.insert(
        null_sys_id,
        SolarSystem {
            solar_system_id: null_sys_id,
            region_id: 10_000_060,
            constellation_id: 20_000_700,
            name: "1DQ1-A".into(),
            security_status: -0.9,
            radius: 1.0e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        },
    );
    let mut titan_t = make_fighter_type(titan_type_id, 30, 6, 1_000_000.0, vec![]);
    titan_t.name = "Avatar".into();
    sde_mut.types.insert(titan_type_id, titan_t);

    // Doomsday module without explicit fuel type (tests default isotope matching) and 0 dmg attrs (tests 10k default)
    let mut dd_t = make_fighter_type(dd_mod_type_id, 588, 7, 100.0, vec![]);
    dd_t.default_attributes.insert(1074, 1.0); // disallowInEmpireSpace
    dd_t.default_attributes.insert(54, 150_000.0); // maxRange
    dd_t.default_attributes.insert(714, 500.0); // fuel qty = 500
    sde_mut.types.insert(dd_mod_type_id, dd_t);

    sess.solar_system_id = Some(null_sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = null_sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = titan_type_id;
            sh.location_id = null_sys_id as i64;
        }
        snap.items.insert(
            mod_id,
            make_fighter_item(mod_id, dd_mod_type_id, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.module_online_states.insert(mod_id, true);
        snap.items.insert(
            fuel_id,
            make_fighter_item(
                fuel_id,
                17888, // Nitrogen Isotopes (matches default 17887..=17889)
                char_id,
                ship_id,
                InventoryFlag::Cargo,
                500, // exact amount for single activation -> removes item
            ),
        );
    });

    (ctx, sess, char_id, ship_id, null_sys_id, mod_id, fuel_id)
}

#[tokio::test]
async fn test_doomsday_session_ship_module_empire_safety_and_state_guards() {
    let (mut ctx, sess, char_id, ship_id, sys_id, mod_id, fuel_id) = build_titan_test_env();

    // 1. Unauthenticated session
    let mut no_char = sess.clone();
    no_char.character_id = None;
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &no_char, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );

    // 2. Structure docked session
    let mut struct_docked = sess.clone();
    struct_docked.structure_id = Some(1_000_000_000_001);
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &struct_docked, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );

    // 3. No active ship or no solar system
    let mut no_ship = sess.clone();
    no_ship.ship_id = None;
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &no_ship, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );
    let mut no_sys = sess.clone();
    no_sys.solar_system_id = None;
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &no_sys, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );

    // 4. Snapshot character docked check
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760);
    });
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &sess, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
    });

    // 5. ModuleNotFound, ModuleNotFitted (wrong flag), ModuleNotOnline
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &sess, DoomsdayAoeMode::Cone, &[PyRep::Integer(999_999)])
            .await
            .is_err()
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().flag = InventoryFlag::LoSlot0;
    });
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &sess, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().flag = InventoryFlag::HiSlot0;
        snap.module_online_states.insert(mod_id, false);
    });
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &sess, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_id, true);
    });

    // 6. Non-Titan hull (group != 30) -> ShipCannotActivateSuperweapon
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 600;
    });
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &sess, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 11_567;
    });

    // 7. Empire space check (sec > 0.0 with disallowInEmpireSpace = 1.0)
    Arc::make_mut(&mut ctx.sde)
        .solar_systems
        .get_mut(&sys_id)
        .unwrap()
        .security_status = 0.4;
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &sess, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );

    // 8. SafetyLevel::Green in empire space when disallowInEmpireSpace == 0.0
    Arc::make_mut(&mut ctx.sde)
        .types
        .get_mut(&24_550)
        .unwrap()
        .default_attributes
        .insert(1074, 0.0);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &sess, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );
    Arc::make_mut(&mut ctx.sde)
        .solar_systems
        .get_mut(&sys_id)
        .unwrap()
        .security_status = -0.9;

    // 9. ShipNotInSpace before spawning ball
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &sess, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );

    // 10. Spawn Titan ball in Warp -> CantActivateInWarp
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut titan_ball = Ball {
        id: ship_id,
        type_id: 11_567,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 80.0,
        inertia: 1.0,
        mass: 1_000_000_000.0,
        radius: 5_000.0,
        speed_fraction: 1.0,
        mode: MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(100_000.0, 0.0, 0.0),
            total_distance: 100_000.0,
            warp_speed_au_s: 3.0,
            phase: eve_destiny::WarpPhase::Cruising,
            elapsed_warp_time: 1.0,
        },
    };
    let _ = sys.spawn_ball_with_combat(titan_ball.clone(), CombatState::new()).await;
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &sess, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );

    // 11. Cloaked and Tethered checks
    titan_ball.mode = MovementMode::Stop;
    let mut cloaked_cs = CombatState::new();
    cloaked_cs.is_cloaked = true;
    let _ = sys.spawn_ball_with_combat(titan_ball.clone(), cloaked_cs).await;
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &sess, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );

    let mut tethered_cs = CombatState::new();
    tethered_cs.is_tethered = true;
    let _ = sys.spawn_ball_with_combat(titan_ball.clone(), tethered_cs).await;
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &sess, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );
    let _ = sys.spawn_ball_with_combat(titan_ball, CombatState::new()).await;

    // 12. Insufficient fuel (partial stack < 500 and missing stack)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&fuel_id).unwrap().quantity = 100;
    });
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &sess, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&fuel_id);
    });
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &sess, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn test_neut_pulse_cone_gtfo_and_reaper_branch_coverage() {
    let (ctx, _sess, char_id, ship_id, sys_id, mod_id, fuel_id) = build_titan_test_env();
    let sys = ctx.systems.get_or_spawn(sys_id);

    // Spawn Titan at origin
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 11_567,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 75.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 5_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
        )
        .await;

    // Spawn nearby subcap ship (within 10km neut pulse and in +X cone)
    let subcap_id = 771_000_001_i64;
    let mut subcap_cs = CombatState::new();
    subcap_cs.capacitor = 50_000.0;
    subcap_cs.max_capacitor = 50_000.0;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: subcap_id,
                type_id: 600, // Cruiser (category 6, subcap)
                position: DVec3::new(8_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            subcap_cs,
        )
        .await;

    // Spawn tethered ship within 10km (should NOT be neuted)
    let tethered_ship_id = 771_000_002_i64;
    let mut teth_cs = CombatState::new();
    teth_cs.is_tethered = true;
    teth_cs.capacitor = 10_000.0;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: tethered_ship_id,
                type_id: 600,
                position: DVec3::new(5_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            teth_cs,
        )
        .await;

    // Non-finite cone direction vector -> InvalidTargetPoint
    assert!(
        activate_cone_doomsday(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            mod_id,
            &[PyRep::Integer(mod_id), PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0)],
        )
        .await
        .is_err()
    );

    // Activate Cone Doomsday targeting subcap_id by ball ID (args[1] = Integer(subcap_id))
    // Consumes exact 500 fuel -> removes fuel_id from snapshot
    let cone_ok = activate_cone_doomsday(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod_id,
        &[PyRep::Integer(mod_id), PyRep::Integer(subcap_id)],
    )
    .await
    .expect("cone activation by target ball ID succeeds");

    // Verify exact fuel stack was removed when remaining == 0
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&fuel_id)));

    // Verify neut pulse drained subcap_id (50,000 - 35,000 = 15,000) and skipped tethered_ship_id
    let neuted_list = cone_ok.dict_get("neutedShipIDs").and_then(|v| v.as_list()).unwrap();
    assert!(neuted_list.contains(&PyRep::Integer(subcap_id)));
    assert!(!neuted_list.contains(&PyRep::Integer(tethered_ship_id)));
    let after_cap = sys.get_combat_state(subcap_id).await.unwrap().unwrap().capacitor;
    assert!((after_cap - 15_000.0).abs() < 1e-3);

    // Verify reactivation delay blocks immediate re-use of mod_id
    assert!(
        activate_cone_doomsday(&ctx, char_id, ship_id, sys_id, mod_id, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );

    // Add fuel and a second module for GTFO & Reaper error/branch tests
    let mod_id_2 = 551_000_010_i64;
    let mod_id_3 = 551_000_011_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (mid, flg) in [(mod_id_2, InventoryFlag::HiSlot1), (mod_id_3, InventoryFlag::HiSlot2)] {
            snap.items.insert(
                mid,
                make_fighter_item(mid, 24_550, char_id, ship_id, flg, 1),
            );
            snap.module_online_states.insert(mid, true);
        }
        snap.items.insert(
            551_000_099,
            make_fighter_item(551_000_099, 17888, char_id, ship_id, InventoryFlag::Cargo, 5_000),
        );
    });

    // Spatial GTFO: unknown target ball ID -> TargetNotFound
    assert!(
        activate_spatial_gtfo_doomsday(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            mod_id_2,
            &[PyRep::Integer(mod_id_2), PyRep::Integer(999_999_999)],
        )
        .await
        .is_err()
    );

    // Spatial GTFO: target point out of range (> 150km) -> TargetOutOfRange
    assert!(
        activate_spatial_gtfo_doomsday(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            mod_id_2,
            &[
                PyRep::Integer(mod_id_2),
                PyRep::Real(200_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
            ],
        )
        .await
        .is_err()
    );

    // Spatial GTFO with explicit center at (8000, 0, 0) schedules subcap_id and preserves orig_max_velocity (75.0)
    let gtfo_ok = activate_spatial_gtfo_doomsday(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod_id_2,
        &[
            PyRep::Integer(mod_id_2),
            PyRep::Real(8_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("GTFO with explicit center succeeds");
    let sched = gtfo_ok.dict_get("scheduledShipIDs").and_then(|v| v.as_list()).unwrap();
    assert!(sched.contains(&PyRep::Integer(subcap_id)));

    let rt = read_superweapon_runtime(&ctx.db).expect("runtime present");
    assert!(rt.active_cycles.iter().all(|c| (c.orig_max_velocity - 75.0).abs() < 1e-6));

    // Reaper: SweepArcTooLong (> 100km between start and end)
    assert!(
        activate_multipoint_reaper_doomsday(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            mod_id_3,
            &[
                PyRep::Integer(mod_id_3),
                PyRep::Real(10_000.0),
                PyRep::Real(0.0),
                PyRep::Real(-60_000.0),
                PyRep::Real(10_000.0),
                PyRep::Real(0.0),
                PyRep::Real(60_000.0), // arc length = 120km > 100km
            ],
        )
        .await
        .is_err()
    );

    // Reaper: 6 flat reals sweeping across subcap_id at (8000, 0, 0)
    let reaper_ok = activate_multipoint_reaper_doomsday(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod_id_3,
        &[
            PyRep::Integer(mod_id_3),
            PyRep::Real(8_000.0),
            PyRep::Real(0.0),
            PyRep::Real(-10_000.0),
            PyRep::Real(8_000.0),
            PyRep::Real(0.0),
            PyRep::Real(10_000.0),
        ],
    )
    .await
    .expect("Reaper 6-real sweep succeeds");
    let hit_ids = reaper_ok.dict_get("hitTargetIDs").and_then(|v| v.as_list()).unwrap();
    assert!(hit_ids.contains(&PyRep::Integer(subcap_id)));
}
