//! Extra branch-coverage and mutation-killing unit tests for `doomsday_aoe_ops`.

#![forbid(unsafe_code)]

use std::sync::Arc;

use crate::router::ServerContext;
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

    // Verify neut pulse drained subcap_id (50,000 - 35,000 = 15,000) and skipped tethered_ship_id without leaking victim IDs
    assert!(cone_ok.dict_get("neutedShipIDs").is_none());
    assert!(cone_ok.dict_get("hitTargetIDs").is_none());
    let after_cap = sys.get_combat_state(subcap_id).await.unwrap().unwrap().capacitor;
    assert!((after_cap - 15_000.0).abs() < 1e-3);
    let teth_cap = sys.get_combat_state(tethered_ship_id).await.unwrap().unwrap().capacitor;
    assert!((teth_cap - 10_000.0).abs() < 1e-3);

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

    // Spatial GTFO with explicit center at (8000, 0, 0) preserves orig_max_velocity (75.0) and avoids leaking victim IDs
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
    assert!(gtfo_ok.dict_get("neutedShipIDs").is_none());
    assert!(gtfo_ok.dict_get("scheduledShipIDs").is_none());
    assert!(gtfo_ok.dict_get("teleportedShipIDs").is_none());

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
    assert!(reaper_ok.dict_get("neutedShipIDs").is_none());
    assert!(reaper_ok.dict_get("hitTargetIDs").is_none());
}

#[tokio::test]
async fn test_doomsday_r9_capacitor_pos_forcefield_and_cloak_protections() {
    let (mut ctx, _sess, char_id, ship_id, sys_id, mod_id, _fuel_id) = build_titan_test_env();
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        if let Some(dd_t) = sde_mut.types.get_mut(&24_550) {
            dd_t.default_attributes.insert(6, 20_000.0); // capacitorNeed = 20,000 GJ
        }
        sde_mut.types.insert(
            33_475,
            make_fighter_type(33_475, 365, 23, 10_000_000.0, vec![(680, 15_000.0)]),
        );
    }

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut titan_cs = CombatState::new();
    titan_cs.capacitor = 10_000.0; // Below 20,000 GJ cap need
    titan_cs.max_capacitor = 100_000.0;
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
            titan_cs,
        )
        .await;

    // 1. Insufficient capacitor -> NotEnoughCapacitor (and fuel is NOT consumed)
    let cap_err = activate_cone_doomsday(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod_id,
        &[PyRep::Integer(mod_id), PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(cap_err.code, "NotEnoughCapacitor");

    // 2. Titan inside online POS forcefield -> CantActivateInForcefield
    let tower_id = 772_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            tower_id,
            make_fighter_item(tower_id, 33_475, 98_999_999, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.starbase_settings.insert(tower_id, Default::default());
        snap.item_positions.insert(tower_id, [10_000.0, 0.0, 0.0]);
    });
    let _ = sys.set_capacitor(ship_id, 80_000.0).await;
    let pos_err = activate_cone_doomsday(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod_id,
        &[PyRep::Integer(mod_id), PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(pos_err.code, "CantActivateInForcefield");

    // Move POS tower to (40_000, 0, 0) so Titan at origin is outside, while a victim at (35_000, 0, 0) / (5_000) can test POS protection
    // Place tower at (25_000, 0, 0) with 15km radius -> covers [10_000..=40_000] on +X, leaving Titan at origin outside!
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(tower_id, [25_000.0, 0.0, 0.0]);
    });

    let cloaked_id = 772_000_010_i64;
    let gate_cloaked_id = 772_000_011_i64;
    let pos_victim_id = 772_000_012_i64;
    let open_victim_id = 772_000_013_i64;

    let spawn_v = |id: i64, pos: DVec3, cloaked: bool, gate_ticks: u32| {
        let sys = sys.clone();
        async move {
            let mut vcs = CombatState::new();
            vcs.shield_hp = 50_000.0;
            vcs.max_shield_hp = 50_000.0;
            vcs.capacitor = 50_000.0;
            vcs.max_capacitor = 50_000.0;
            vcs.is_cloaked = cloaked;
            vcs.gate_cloak_remaining_ticks = gate_ticks;
            let _ = sys
                .spawn_ball_with_combat(
                    Ball {
                        id,
                        type_id: 600,
                        position: pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 250.0,
                        inertia: 1.0,
                        mass: 10_000_000.0,
                        radius: 100.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    vcs,
                )
                .await;
        }
    };
    spawn_v(cloaked_id, DVec3::new(5_000.0, 0.0, 0.0), true, 0).await;
    spawn_v(gate_cloaked_id, DVec3::new(6_000.0, 0.0, 0.0), false, 20).await;
    spawn_v(pos_victim_id, DVec3::new(12_000.0, 0.0, 0.0), false, 0).await; // inside POS + in cone!
    spawn_v(open_victim_id, DVec3::new(7_000.0, 0.0, 0.0), false, 0).await;

    // Also place a ship at (9_500, 0, 0) inside a second POS just at 25km? No, pos_victim_id at 12km is within 15km of 25km tower!
    // Check neut pulse + capacitor deduction + Cone tick skipping cloaked/gate-cloaked/POS targets
    let res = activate_cone_doomsday(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        mod_id,
        &[PyRep::Integer(mod_id), PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .expect("cone activation outside POS with sufficient capacitor succeeds");

    assert!(res.dict_get("neutedShipIDs").is_none());
    assert!(res.dict_get("hitTargetIDs").is_none());
    let titan_cap_after = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    assert!((titan_cap_after - 60_000.0).abs() < 1e-3, "must deduct 20,000 GJ cap need");

    // Neut pulse: cloaked & gate-cloaked skipped; open drained
    assert!((sys.get_combat_state(cloaked_id).await.unwrap().unwrap().capacitor - 50_000.0).abs() < 1e-3);
    assert!((sys.get_combat_state(gate_cloaked_id).await.unwrap().unwrap().capacitor - 50_000.0).abs() < 1e-3);
    assert!((sys.get_combat_state(open_victim_id).await.unwrap().unwrap().capacitor - 15_000.0).abs() < 1e-3);

    // Step past 10s warmup so Cone applies tick damage
    let rt = read_superweapon_runtime(&ctx.db).unwrap();
    let delta = rt.active_cycles[0].warmup_secs + 1;
    crate::services::doomsday_aoe_tick::mutate_superweapon_runtime(&ctx.db, |rt_mut| {
        rt_mut.active_cycles[0].started_at_unix =
            rt_mut.active_cycles[0].started_at_unix.saturating_sub(delta);
    });
    crate::services::doomsday_aoe_tick::step_bosonic_doomsday_system(&ctx, sys_id).await;
    assert_eq!(sys.get_combat_state(cloaked_id).await.unwrap().unwrap().shield_hp, 50_000.0);
    assert_eq!(sys.get_combat_state(gate_cloaked_id).await.unwrap().unwrap().shield_hp, 50_000.0);
    assert_eq!(sys.get_combat_state(pos_victim_id).await.unwrap().unwrap().shield_hp, 50_000.0);
    assert!(sys.get_combat_state(open_victim_id).await.unwrap().unwrap().shield_hp < 50_000.0);
}

#[tokio::test]
async fn test_doomsday_cancel_locked_fuel_multi_stack_zero_vel_and_arg_forms() {
    use crate::services::doomsday_aoe_ops::cancel_aoe_superweapon_cycles_for_ship;
    use crate::services::doomsday_aoe_tick::mutate_superweapon_runtime;
    use eve_db::{OwnerNoteRecord, PersistedSuperweaponCycle, SuperweaponCycleKind};

    // 1. cancel_aoe_superweapon_cycles_for_ship: empty no-op and ship-scoped filter
    let (ctx, _sess, char_id, ship_id, sys_id, _mod_id, _fuel_id) = build_titan_test_env();
    cancel_aoe_superweapon_cycles_for_ship(&ctx, ship_id);
    assert!(read_superweapon_runtime(&ctx.db).is_none());

    mutate_superweapon_runtime(&ctx.db, |rt| {
        for sid in [ship_id, ship_id + 1] {
            rt.active_cycles.push(PersistedSuperweaponCycle {
                sys_id,
                char_id,
                ship_id: sid,
                ship_type_id: 11_567,
                mod_type_id: 24_550,
                kind: SuperweaponCycleKind::Cone {
                    origin_pos: [0.0; 3],
                    dir_vec: [1.0, 0.0, 0.0],
                    cone_len_m: 100_000.0,
                    tick_dmg: [1_000.0; 4],
                    sig_radius_m: 1_000.0,
                },
                started_at_unix: 1,
                warmup_secs: 5,
                total_ticks: 10,
                ticks_applied: 0,
                immobilize_until_unix: 30,
                orig_max_velocity: 75.0,
                orig_warp_scramble_strength: 0,
                orig_disallow_assistance: false,
                mobility_restored: false,
            });
        }
    });
    cancel_aoe_superweapon_cycles_for_ship(&ctx, ship_id);
    let rt_after_cancel = read_superweapon_runtime(&ctx.db).expect("other ship cycle remains");
    assert_eq!(rt_after_cancel.active_cycles.len(), 1);
    assert_eq!(rt_after_cancel.active_cycles[0].ship_id, ship_id + 1);

    // 2. Non-fuel & ALSC-locked fuel exclusion + 3-stack remaining -= take + zero-vel preservation + Tuple cone (len==2)
    let (ctx2, _s2, c2, ship2, sys2, mod2, fuel2) = build_titan_test_env();
    let sys_h2 = ctx2.systems.get_or_spawn(sys2);
    sys_h2
        .spawn_ball_with_combat(
            Ball {
                id: ship2,
                type_id: 11_567,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 5_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                capacitor: 100_000.0,
                max_capacitor: 100_000.0,
                base_max_velocity: None,
                ..CombatState::new()
            },
        )
        .await
        .unwrap();
    let non_fuel_id = 551_900_001_i64;
    let partial_bay_id = 551_900_002_i64;
    let stack_a_id = 551_900_003_i64;
    let stack_b_id = 551_900_004_i64;
    ctx2.db.with_mut_snapshot(|snap| {
        // Lock original 500-isotope stack via ALSC owner note and reduce available unlocked fuel to 200
        snap.owner_notes.insert(
            -8_400_000_000_000,
            OwnerNoteRecord {
                note_id: -8_400_000_000_000,
                character_id: 0,
                item_id: 0,
                label: "container".into(),
                note: format!("{{\"alsc_locked_items\":[{fuel2}]}}"),
                timestamp_unix: 0,
            },
        );
        snap.items.insert(
            non_fuel_id,
            make_fighter_item(non_fuel_id, 34, c2, ship2, InventoryFlag::Cargo, 10_000),
        );
        snap.items.insert(
            partial_bay_id,
            make_fighter_item(
                partial_bay_id,
                17_888,
                c2,
                ship2,
                InventoryFlag::SpecializedFuelBay,
                100,
            ),
        );
        snap.items.insert(
            stack_a_id,
            make_fighter_item(stack_a_id, 17_888, c2, ship2, InventoryFlag::Cargo, 100),
        );
    });
    let locked_err = activate_cone_doomsday(&ctx2, c2, ship2, sys2, mod2, &[PyRep::Integer(mod2)])
        .await
        .unwrap_err();
    assert_eq!(locked_err.code, "InsufficientFuel");
    ctx2.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&fuel2].quantity, 500);
        assert_eq!(snap.items[&non_fuel_id].quantity, 10_000);
    });

    // Add 100 more in stack_a (200 total) + 300 in stack_b -> 600 unlocked across 3 stacks, consume exact 500 leaving 100 in stack_b
    ctx2.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&stack_a_id).unwrap().quantity = 200;
        snap.items.insert(
            stack_b_id,
            make_fighter_item(stack_b_id, 17_888, c2, ship2, InventoryFlag::Cargo, 300),
        );
    });
    activate_cone_doomsday(
        &ctx2,
        c2,
        ship2,
        sys2,
        mod2,
        &[
            PyRep::Integer(mod2),
            PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .unwrap();
    ctx2.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&partial_bay_id));
        assert!(!snap.items.contains_key(&stack_a_id));
        assert_eq!(snap.items.get(&stack_b_id).map(|i| i.quantity), Some(100));
        assert_eq!(snap.items.get(&fuel2).map(|i| i.quantity), Some(500));
    });
    assert_eq!(
        sys_h2.get_combat_state(ship2).await.unwrap().unwrap().base_max_velocity,
        None,
        "max_velocity == 0.0 must not populate base_max_velocity = Some(0.0)"
    );
    let rt2 = read_superweapon_runtime(&ctx2.db).unwrap();
    match &rt2.active_cycles.last().unwrap().kind {
        SuperweaponCycleKind::Cone { dir_vec, .. } => assert!(dir_vec[1] > 0.9),
        other => panic!("expected Cone cycle, got {other:?}"),
    }

    // 3. Preserves pre-existing base_max_velocity = Some(99.0) when orig_v > 0 AND 4-flat-arg Cone form
    let (ctx3, _s3, c3, ship3, sys3, mod3, _f3) = build_titan_test_env();
    let sys_h3 = ctx3.systems.get_or_spawn(sys3);
    let mut cs3 = CombatState::new();
    cs3.capacitor = 100_000.0;
    cs3.base_max_velocity = Some(99.0);
    sys_h3
        .spawn_ball_with_combat(
            Ball {
                id: ship3,
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
            cs3,
        )
        .await
        .unwrap();
    activate_cone_doomsday(
        &ctx3,
        c3,
        ship3,
        sys3,
        mod3,
        &[
            PyRep::Integer(mod3),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::Real(1.0),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        sys_h3.get_combat_state(ship3).await.unwrap().unwrap().base_max_velocity,
        Some(99.0)
    );
    match &read_superweapon_runtime(&ctx3.db).unwrap().active_cycles.last().unwrap().kind {
        SuperweaponCycleKind::Cone { dir_vec, .. } => assert!(dir_vec[2] > 0.9),
        other => panic!("expected Cone cycle, got {other:?}"),
    }

    // 4. GTFO Tuple (len == 2) & 4-flat-arg forms
    let (ctx4, _s4, c4, ship4, sys4, mod4, _) = build_titan_test_env();
    let sys_h4 = ctx4.systems.get_or_spawn(sys4);
    sys_h4
        .spawn_ball_with_combat(
            Ball {
                id: ship4,
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
        .await
        .unwrap();
    activate_spatial_gtfo_doomsday(
        &ctx4,
        c4,
        ship4,
        sys4,
        mod4,
        &[
            PyRep::Integer(mod4),
            PyRep::Tuple(vec![PyRep::Real(12_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .unwrap();
    match &read_superweapon_runtime(&ctx4.db).unwrap().active_cycles.last().unwrap().kind {
        SuperweaponCycleKind::SpatialGtfo { center, .. } => assert_eq!(center[0], 12_000.0),
        other => panic!("expected SpatialGtfo, got {other:?}"),
    }

    let (ctx5, _s5, c5, ship5, sys5, mod5, _) = build_titan_test_env();
    let sys_h5 = ctx5.systems.get_or_spawn(sys5);
    sys_h5
        .spawn_ball_with_combat(
            Ball {
                id: ship5,
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
        .await
        .unwrap();
    activate_spatial_gtfo_doomsday(
        &ctx5,
        c5,
        ship5,
        sys5,
        mod5,
        &[
            PyRep::Integer(mod5),
            PyRep::Real(0.0),
            PyRep::Real(12_000.0),
            PyRep::Real(0.0),
        ],
    )
    .await
    .unwrap();
    match &read_superweapon_runtime(&ctx5.db).unwrap().active_cycles.last().unwrap().kind {
        SuperweaponCycleKind::SpatialGtfo { center, .. } => assert_eq!(center[1], 12_000.0),
        other => panic!("expected SpatialGtfo, got {other:?}"),
    }

    // 5. Reaper: mixed [mod, Tuple, Integer(5)] falls back cleanly (L794 &&->||) & 7-flat-arg form (L805, L809)
    let (ctx6, _s6, c6, ship6, sys6, mod6, _) = build_titan_test_env();
    let sys_h6 = ctx6.systems.get_or_spawn(sys6);
    sys_h6
        .spawn_ball_with_combat(
            Ball {
                id: ship6,
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
        .await
        .unwrap();
    activate_multipoint_reaper_doomsday(
        &ctx6,
        c6,
        ship6,
        sys6,
        mod6,
        &[
            PyRep::Integer(mod6),
            PyRep::Tuple(vec![PyRep::Real(1_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            PyRep::Integer(5),
        ],
    )
    .await
    .unwrap();
    match &read_superweapon_runtime(&ctx6.db).unwrap().active_cycles.last().unwrap().kind {
        SuperweaponCycleKind::ReaperSweep { start_pt, .. } => assert_eq!(*start_pt, [20_000.0, 0.0, 0.0]),
        other => panic!("expected ReaperSweep, got {other:?}"),
    }

    let (ctx7, _s7, c7, ship7, sys7, mod7, _) = build_titan_test_env();
    let sys_h7 = ctx7.systems.get_or_spawn(sys7);
    sys_h7
        .spawn_ball_with_combat(
            Ball {
                id: ship7,
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
        .await
        .unwrap();
    activate_multipoint_reaper_doomsday(
        &ctx7,
        c7,
        ship7,
        sys7,
        mod7,
        &[
            PyRep::Integer(mod7),
            PyRep::Real(100.0),
            PyRep::Real(200.0),
            PyRep::Real(300.0),
            PyRep::Real(400.0),
            PyRep::Real(500.0),
            PyRep::Real(600.0),
        ],
    )
    .await
    .unwrap();
    match &read_superweapon_runtime(&ctx7.db).unwrap().active_cycles.last().unwrap().kind {
        SuperweaponCycleKind::ReaperSweep { start_pt, end_pt, .. } => {
            assert_eq!(*start_pt, [100.0, 200.0, 300.0]);
            assert_eq!(*end_pt, [400.0, 500.0, 600.0]);
        }
        other => panic!("expected ReaperSweep, got {other:?}"),
    }
}
